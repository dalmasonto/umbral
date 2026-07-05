//! End-to-end coverage for umbral-tasks: enqueue, drive the worker
//! single-step, assert DB state. Same boot shape as umbral-sessions —
//! one OnceCell-backed tempfile sqlite pool, registered TasksPlugin,
//! raw SQL CREATE TABLE because the integration test owns its own
//! schema without standing up the M5 migration loop.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbral_tasks::{
    _clear_handlers_for_tests, DEFAULT_MAX_ATTEMPTS, DEFAULT_VISIBILITY_TIMEOUT, EnqueueOptions,
    RetryPolicy, STATUS_FAILED, STATUS_PENDING, STATUS_RUNNING, STATUS_SUCCEEDED, TaskRow,
    TasksPlugin, enqueue, reclaim_orphaned_tasks, reclaim_orphaned_tasks_with, register_handler,
    run_worker_once, run_worker_once_with,
};

/// A backoff-free policy for tests that drive several retries back-to-back
/// without simulating elapsed time. With `backoff_base = 0` a retriable
/// failure leaves `run_at = now`, so the very next `run_worker_once_with`
/// re-claims the row immediately — the pre-backoff behaviour these tests
/// were written against, now opted into explicitly.
fn no_backoff() -> RetryPolicy {
    RetryPolicy {
        backoff_base: Duration::from_secs(0),
        backoff_max: Duration::from_secs(0),
        task_timeout: None,
    }
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tasks_integration.sqlite");
        // Keep the directory alive for the process lifetime.
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(TasksPlugin::default())
            .build()
            .expect("App::build with TasksPlugin");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE task_row (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL,\
                payload TEXT NOT NULL,\
                status TEXT NOT NULL,\
                attempts INTEGER NOT NULL,\
                max_attempts INTEGER NOT NULL,\
                scheduled_for TEXT NOT NULL,\
                run_at TEXT,\
                started_at TEXT,\
                completed_at TEXT,\
                error TEXT,\
                result TEXT,\
                priority INTEGER,\
                created_at TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create task_row");
    })
    .await;
}

/// Fetch one row by id for assertions.
async fn fetch(id: i64) -> TaskRow {
    let pool = umbral::db::pool();
    sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row")
}

/// Drain every row from the queue. Used to keep tests independent —
/// each test enqueues fresh rows then deletes them at the end so the
/// next test's `run_worker_once` sees only its own data.
async fn drain_queue() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM task_row")
        .execute(&pool)
        .await
        .expect("drain task_row");
}

/// Per-test serialisation: every test contends for the same handler
/// registry and the same DB table, so they can't run in parallel.
/// `tokio::sync::Mutex` so the guard is async-safe across the await
/// points the test bodies hold it across.
static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

// =========================================================================
// 1. happy path
// =========================================================================

/// Register a handler that flips an AtomicBool, enqueue a task, run one
/// worker iteration, assert the flag is set and the row landed in
/// `succeeded` with `completed_at` populated.
#[tokio::test(flavor = "multi_thread")]
async fn enqueue_then_run_worker_once_processes_a_task() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    static FLAG: OnceLock<AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| AtomicBool::new(false))
        .store(false, Ordering::SeqCst);

    register_handler("happy_path", |_payload: &str| async move {
        FLAG.get().unwrap().store(true, Ordering::SeqCst);
        Ok(())
    });

    let id = enqueue(
        "happy_path",
        serde_json::json!({"foo": 1}),
        Default::default(),
    )
    .await
    .expect("enqueue");

    let processed = run_worker_once().await.expect("worker step");
    assert!(processed, "worker should have processed the enqueued task");
    assert!(
        FLAG.get().unwrap().load(Ordering::SeqCst),
        "handler should have flipped the flag",
    );

    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_SUCCEEDED);
    assert!(row.completed_at.is_some(), "completed_at should be set");
    assert_eq!(row.attempts, 1, "exactly one attempt");
    assert!(row.error.is_none(), "no error on success");
}

// =========================================================================
// 2. retry semantics
// =========================================================================

/// A handler that always returns Err. With max_attempts=2: first
/// iteration leaves attempts=1, status=pending. Second iteration brings
/// attempts to 2; since attempts >= max_attempts that's terminal, so
/// status flips to failed and a third iteration is a no-op
/// (`run_worker_once` returns false — nothing pending).
#[tokio::test(flavor = "multi_thread")]
async fn failed_handler_retries_until_max_attempts() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    static CALLS: OnceLock<AtomicUsize> = OnceLock::new();
    CALLS
        .get_or_init(|| AtomicUsize::new(0))
        .store(0, Ordering::SeqCst);

    register_handler("always_fails", |_payload: &str| async move {
        CALLS.get().unwrap().fetch_add(1, Ordering::SeqCst);
        Err::<(), String>("kaboom".to_string())
    });

    let id = enqueue(
        "always_fails",
        serde_json::json!({}),
        EnqueueOptions {
            max_attempts: Some(2),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    // First iteration: handler fails, retries left -> pending again.
    // Drive with a zero backoff so the retry stays immediately eligible
    // (otherwise `run_at` is pushed ~2s out and step 2 wouldn't claim it).
    assert!(run_worker_once_with(no_backoff()).await.expect("step 1"));
    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_PENDING, "should reset to pending");
    assert_eq!(row.attempts, 1, "one attempt counted");
    assert_eq!(row.error.as_deref(), Some("kaboom"));

    // Second iteration: handler fails again, attempts reaches max -> failed.
    assert!(run_worker_once_with(no_backoff()).await.expect("step 2"));
    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_FAILED, "should be terminal failed");
    assert_eq!(row.attempts, 2, "exactly max_attempts attempts");
    assert!(
        row.completed_at.is_some(),
        "completed_at marked on terminal failure"
    );
    assert_eq!(row.error.as_deref(), Some("kaboom"));

    // Third iteration: queue is drained — terminal rows aren't picked back up.
    assert!(
        !run_worker_once().await.expect("step 3"),
        "no pending rows left"
    );
    assert_eq!(
        CALLS.get().unwrap().load(Ordering::SeqCst),
        2,
        "handler invoked exactly max_attempts times",
    );
}

// =========================================================================
// 3. scheduled_for in the future
// =========================================================================

/// A task scheduled an hour into the future stays invisible to
/// run_worker_once.
#[tokio::test(flavor = "multi_thread")]
async fn enqueued_task_with_future_scheduled_for_is_not_picked_up() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    let future = Utc::now() + chrono::Duration::hours(1);
    let id = enqueue(
        "ignored_handler",
        serde_json::json!({}),
        EnqueueOptions {
            scheduled_for: Some(future),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    let processed = run_worker_once().await.expect("worker step");
    assert!(!processed, "future-scheduled task should not be picked up");

    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_PENDING);
    assert_eq!(row.attempts, 0);
    assert!(row.started_at.is_none());
}

// =========================================================================
// 4. unknown handler
// =========================================================================

/// A task whose name isn't registered is marked failed (non-retriable)
/// with an error message that mentions "handler not found".
#[tokio::test(flavor = "multi_thread")]
async fn unknown_handler_marks_task_failed_with_handler_not_found_error() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    let id = enqueue(
        "no_such_handler",
        serde_json::json!({}),
        EnqueueOptions {
            max_attempts: Some(5),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    let processed = run_worker_once().await.expect("worker step");
    assert!(
        processed,
        "worker should have claimed the row even though the handler is missing"
    );

    let row = fetch(id).await;
    assert_eq!(
        row.status, STATUS_FAILED,
        "missing handler is non-retriable"
    );
    let err = row.error.as_deref().unwrap_or("");
    assert!(
        err.contains("handler not found"),
        "error column should mention handler not found; got {err:?}",
    );
}

// =========================================================================
// 4b. non-retriable: missing handler must NOT burn through max_attempts
// =========================================================================

/// A task whose handler is not registered must be marked failed on the
/// FIRST worker iteration regardless of `max_attempts`. This verifies
/// the retry decision uses the typed `TaskError::HandlerNotFound`
/// variant, not a string match — so it can't silently break if the
/// error message text changes.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_handler_is_non_retriable_regardless_of_max_attempts() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    // max_attempts is high; the task must still fail on attempt 1.
    let id = enqueue(
        "handler_that_does_not_exist",
        serde_json::json!({}),
        EnqueueOptions {
            max_attempts: Some(10),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    // Single worker step — should claim the row and immediately mark it failed.
    let processed = run_worker_once().await.expect("worker step");
    assert!(processed, "worker should have claimed the row");

    let row = fetch(id).await;
    assert_eq!(
        row.status, STATUS_FAILED,
        "HandlerNotFound must be non-retriable: expected failed, got {:?}",
        row.status
    );
    assert_eq!(
        row.attempts, 1,
        "must NOT burn through max_attempts ({}) before failing; got {} attempts",
        row.max_attempts, row.attempts
    );
    assert!(
        row.completed_at.is_some(),
        "completed_at must be set on non-retriable failure"
    );
    let err = row.error.as_deref().unwrap_or("");
    assert!(
        err.contains("handler not found"),
        "error column should mention the missing handler; got {err:?}",
    );

    // Queue is now drained — a second iteration should be a no-op.
    assert!(
        !run_worker_once().await.expect("step 2"),
        "no pending rows should remain after non-retriable failure"
    );
}

// =========================================================================
// 5. basic enqueue shape
// =========================================================================

/// enqueue returns the new row's id and writes a pending row with the
/// expected shape (status=pending, attempts=0, default max_attempts).
#[tokio::test(flavor = "multi_thread")]
async fn enqueue_returns_new_id_and_writes_pending_row() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    let id = enqueue(
        "ping",
        serde_json::json!({"hello": "world"}),
        Default::default(),
    )
    .await
    .expect("enqueue");
    assert!(id > 0, "expected positive id, got {id}");

    let row = fetch(id).await;
    assert_eq!(row.name, "ping");
    assert!(row.payload.contains("hello"));
    assert_eq!(row.status, STATUS_PENDING);
    assert_eq!(row.attempts, 0);
    assert_eq!(row.max_attempts, DEFAULT_MAX_ATTEMPTS);
    assert!(row.started_at.is_none());
    assert!(row.completed_at.is_none());
    assert!(row.error.is_none());
}

// =========================================================================
// 6. empty-queue idle
// =========================================================================

/// With nothing pending, run_worker_once returns Ok(false) immediately
/// without sleeping.
#[tokio::test(flavor = "multi_thread")]
async fn run_worker_once_returns_false_on_empty_queue() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    // Sanity: even with a short timeout, the call returns immediately.
    let result = tokio::time::timeout(Duration::from_secs(1), run_worker_once())
        .await
        .expect("run_worker_once should not block on an empty queue");
    assert!(matches!(result, Ok(false)));
}

// =========================================================================
// 7. tasks-worker plugin command (the M7 `Plugin::commands()` lift)
// =========================================================================

/// Dispatch `tasks-worker --once` through umbral::cli::dispatch and
/// verify it ran one iteration of the worker. End-to-end check that
/// `Plugin::commands()` is wired correctly: TasksPlugin contributes
/// the WorkerCommand, dispatch matches it by name, and the handler's
/// `run()` calls into `run_worker_once`.
#[tokio::test(flavor = "multi_thread")]
async fn dispatch_routes_tasks_worker_command_to_run_worker_once() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    static FLAG: OnceLock<AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| AtomicBool::new(false))
        .store(false, Ordering::SeqCst);

    register_handler("cli_routed", |_payload: &str| async move {
        FLAG.get().unwrap().store(true, Ordering::SeqCst);
        Ok(())
    });
    enqueue(
        "cli_routed",
        serde_json::json!({"via": "cli"}),
        Default::default(),
    )
    .await
    .expect("enqueue");

    let plugins: Vec<Box<dyn umbral::prelude::Plugin>> = vec![Box::new(TasksPlugin::default())];
    let outcome = umbral::cli::dispatch(&plugins, vec!["umbral-cli", "tasks-worker", "--once"])
        .await
        .expect("dispatch ok");
    match outcome {
        umbral::cli::DispatchOutcome::Matched(name) => assert_eq!(name, "tasks-worker"),
        other => panic!("expected Matched(tasks-worker), got {other:?}"),
    }

    assert!(
        FLAG.get().unwrap().load(Ordering::SeqCst),
        "handler should have run via dispatched CLI command"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dispatch_returns_unmatched_for_unknown_command() {
    let _guard = test_lock().await;
    boot().await;

    let plugins: Vec<Box<dyn umbral::prelude::Plugin>> = vec![Box::new(TasksPlugin::default())];
    let outcome = umbral::cli::dispatch(&plugins, vec!["umbral-cli", "no-such-cmd"])
        .await
        .expect("dispatch should return DispatchOutcome::Unmatched, not Err");
    // Updated contract: when argv references a subcommand that's not in
    // the registered plugins' command set, dispatch returns Unmatched
    // so the caller (umbral-cli's own clap parser) gets to try the
    // built-in subcommands like `serve`, `migrate`, `dev`. The old
    // behavior of propagating clap's InvalidSubcommand error meant
    // built-in subcommands could never coexist with plugin subcommands.
    assert!(
        matches!(outcome, umbral::cli::DispatchOutcome::Unmatched),
        "expected Unmatched for an unknown subcommand, got: {outcome:?}",
    );
}

// =========================================================================
// 9. Orphan-task reclaim (visibility timeout / at-least-once guarantee)
// =========================================================================

/// Directly insert a RUNNING row with an expired started_at to simulate a
/// crashed worker. Verify that WITHOUT calling reclaim_orphaned_tasks the
/// row stays stuck (proving the test would fail before the fix).
#[tokio::test(flavor = "multi_thread")]
async fn stuck_running_task_stays_stuck_without_reclaim() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    let pool = umbral::db::pool();
    let old_started_at = Utc::now() - chrono::Duration::hours(2);
    sqlx::query(
        "INSERT INTO task_row \
         (name, payload, status, attempts, max_attempts, scheduled_for, \
          started_at, completed_at, error, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
    )
    .bind("orphan_handler")
    .bind("{}")
    .bind(STATUS_RUNNING)
    .bind(1i64)
    .bind(3i64)
    .bind(Utc::now().to_rfc3339())
    .bind(old_started_at.to_rfc3339())
    .bind(Utc::now().to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert stuck running row");

    // Without reclaim: run_worker_once should NOT pick up the RUNNING row.
    let processed = run_worker_once().await.expect("worker step");
    assert!(
        !processed,
        "run_worker_once must not re-claim an already-RUNNING row; \
         without reclaim the task stays stuck"
    );

    // Confirm the row is still in RUNNING state — it's orphaned.
    let rows: Vec<TaskRow> = {
        sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row WHERE status = ?")
            .bind(STATUS_RUNNING)
            .fetch_all(&pool)
            .await
            .expect("fetch running rows")
    };
    assert_eq!(
        rows.len(),
        1,
        "stuck row should still be RUNNING without reclaim"
    );
}

/// A task left in RUNNING with an expired started_at (crashed worker) is
/// reclaimed by reclaim_orphaned_tasks and becomes runnable again. A
/// subsequent run_worker_once completes it successfully.
#[tokio::test(flavor = "multi_thread")]
async fn orphaned_running_task_is_reclaimed_and_completes() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    static FLAG: OnceLock<AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| AtomicBool::new(false))
        .store(false, Ordering::SeqCst);

    register_handler("orphan_completes", |_payload: &str| async move {
        FLAG.get().unwrap().store(true, Ordering::SeqCst);
        Ok(())
    });

    // Simulate a crashed-worker: insert a RUNNING row whose started_at
    // is well past the visibility timeout.
    let pool = umbral::db::pool();
    let old_started_at = Utc::now() - chrono::Duration::hours(2);
    sqlx::query(
        "INSERT INTO task_row \
         (name, payload, status, attempts, max_attempts, scheduled_for, \
          started_at, completed_at, error, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
    )
    .bind("orphan_completes")
    .bind("{}")
    .bind(STATUS_RUNNING)
    .bind(1i64) // one attempt already counted by the crashed worker
    .bind(3i64)
    .bind(Utc::now().to_rfc3339())
    .bind(old_started_at.to_rfc3339())
    .bind(Utc::now().to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert stuck running row");

    // Reclaim with a tiny timeout so the row qualifies immediately, and a
    // zero backoff so the reclaimed row's `run_at` stays at `now` (the
    // default policy would push it ~2s out and the claim below would miss).
    let reclaimed = reclaim_orphaned_tasks_with(Duration::from_millis(1), no_backoff())
        .await
        .expect("reclaim");
    assert_eq!(
        reclaimed, 1,
        "exactly one orphaned task should be reclaimed"
    );

    // The row should now be PENDING again.
    let rows: Vec<TaskRow> = sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row")
        .fetch_all(&pool)
        .await
        .expect("fetch all");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].status, STATUS_PENDING,
        "reclaimed task should be PENDING, got {:?}",
        rows[0].status
    );
    assert!(
        rows[0].started_at.is_none(),
        "started_at should be cleared on reclaim"
    );

    // The next worker iteration should pick up and complete the task.
    let processed = run_worker_once().await.expect("worker step after reclaim");
    assert!(processed, "worker should process the reclaimed task");
    assert!(
        FLAG.get().unwrap().load(Ordering::SeqCst),
        "handler should have run after reclaim"
    );

    let rows: Vec<TaskRow> = sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row")
        .fetch_all(&pool)
        .await
        .expect("fetch all after completion");
    assert_eq!(rows[0].status, STATUS_SUCCEEDED);
}

/// A RUNNING task with a fresh started_at (live worker) must NOT be
/// reclaimed — the lease is still valid.
#[tokio::test(flavor = "multi_thread")]
async fn fresh_running_task_is_not_reclaimed() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    let pool = umbral::db::pool();
    // started_at is just now — well within any visibility timeout.
    let fresh_started_at = Utc::now();
    sqlx::query(
        "INSERT INTO task_row \
         (name, payload, status, attempts, max_attempts, scheduled_for, \
          started_at, completed_at, error, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
    )
    .bind("live_handler")
    .bind("{}")
    .bind(STATUS_RUNNING)
    .bind(1i64)
    .bind(3i64)
    .bind(Utc::now().to_rfc3339())
    .bind(fresh_started_at.to_rfc3339())
    .bind(Utc::now().to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert fresh running row");

    // Reclaim with the full default timeout — fresh row must not qualify.
    let reclaimed = reclaim_orphaned_tasks(DEFAULT_VISIBILITY_TIMEOUT)
        .await
        .expect("reclaim");
    assert_eq!(
        reclaimed, 0,
        "a fresh RUNNING task must NOT be reclaimed; got {reclaimed}"
    );

    // Confirm still RUNNING.
    let rows: Vec<TaskRow> = sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row")
        .fetch_all(&pool)
        .await
        .expect("fetch all");
    assert_eq!(
        rows[0].status, STATUS_RUNNING,
        "row should still be RUNNING"
    );
}

/// An orphaned RUNNING task that has already consumed all max_attempts must
/// be marked FAILED by reclaim_orphaned_tasks, not reset to PENDING for an
/// infinite retry loop.
#[tokio::test(flavor = "multi_thread")]
async fn orphaned_task_at_max_attempts_is_failed_not_retried() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    let pool = umbral::db::pool();
    let old_started_at = Utc::now() - chrono::Duration::hours(2);
    // attempts == max_attempts: exhausted.
    sqlx::query(
        "INSERT INTO task_row \
         (name, payload, status, attempts, max_attempts, scheduled_for, \
          started_at, completed_at, error, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
    )
    .bind("exhausted_handler")
    .bind("{}")
    .bind(STATUS_RUNNING)
    .bind(3i64) // attempts
    .bind(3i64) // max_attempts — exhausted
    .bind(Utc::now().to_rfc3339())
    .bind(old_started_at.to_rfc3339())
    .bind(Utc::now().to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert exhausted running row");

    let reclaimed = reclaim_orphaned_tasks(Duration::from_millis(1))
        .await
        .expect("reclaim");
    assert_eq!(reclaimed, 1, "exhausted orphan should be reclaimed");

    let rows: Vec<TaskRow> = sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row")
        .fetch_all(&pool)
        .await
        .expect("fetch all");
    assert_eq!(
        rows[0].status, STATUS_FAILED,
        "exhausted orphan must be FAILED, not PENDING; got {:?}",
        rows[0].status
    );
    assert!(
        rows[0].completed_at.is_some(),
        "completed_at should be set when marking FAILED via reclaim"
    );
    assert!(
        rows[0].error.is_some(),
        "error column should explain the failure"
    );

    // Queue should now be empty — no pending rows for the worker.
    let processed = run_worker_once()
        .await
        .expect("worker step after exhausted reclaim");
    assert!(
        !processed,
        "no runnable tasks should remain after exhausted-orphan reclaim"
    );
}
