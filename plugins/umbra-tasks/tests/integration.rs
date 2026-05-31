//! End-to-end coverage for umbra-tasks: enqueue, drive the worker
//! single-step, assert DB state. Same boot shape as umbra-sessions —
//! one OnceCell-backed tempfile sqlite pool, registered TasksPlugin,
//! raw SQL CREATE TABLE because the integration test owns its own
//! schema without standing up the M5 migration loop.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbra_tasks::{
    _clear_handlers_for_tests, DEFAULT_MAX_ATTEMPTS, EnqueueOptions, STATUS_FAILED, STATUS_PENDING,
    STATUS_SUCCEEDED, TaskRow, TasksPlugin, enqueue, register_handler, run_worker_once,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tasks_integration.sqlite");
        // Keep the directory alive for the process lifetime.
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(TasksPlugin)
            .build()
            .expect("App::build with TasksPlugin");

        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE task_row (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL,\
                payload TEXT NOT NULL,\
                status TEXT NOT NULL,\
                attempts INTEGER NOT NULL,\
                max_attempts INTEGER NOT NULL,\
                scheduled_for TEXT NOT NULL,\
                started_at TEXT,\
                completed_at TEXT,\
                error TEXT,\
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
    let pool = umbra::db::pool();
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
    let pool = umbra::db::pool();
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
        Err("kaboom".to_string())
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
    assert!(run_worker_once().await.expect("step 1"));
    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_PENDING, "should reset to pending");
    assert_eq!(row.attempts, 1, "one attempt counted");
    assert_eq!(row.error.as_deref(), Some("kaboom"));

    // Second iteration: handler fails again, attempts reaches max -> failed.
    assert!(run_worker_once().await.expect("step 2"));
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

/// Dispatch `tasks-worker --once` through umbra::cli::dispatch and
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

    let plugins: Vec<Box<dyn umbra::prelude::Plugin>> = vec![Box::new(TasksPlugin)];
    let outcome = umbra::cli::dispatch(&plugins, vec!["umbra-cli", "tasks-worker", "--once"])
        .await
        .expect("dispatch ok");
    match outcome {
        umbra::cli::DispatchOutcome::Matched(name) => assert_eq!(name, "tasks-worker"),
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

    let plugins: Vec<Box<dyn umbra::prelude::Plugin>> = vec![Box::new(TasksPlugin)];
    let outcome = umbra::cli::dispatch(&plugins, vec!["umbra-cli", "no-such-cmd"])
        .await
        .map_err(|e| format!("{e}"));
    // clap reports an unknown subcommand as a parse error, which the
    // dispatcher bubbles out as Err. That's the contract: clap surfaces
    // the typo at the boundary.
    assert!(outcome.is_err(), "got: {outcome:?}");
}
