//! Concurrency + panic-recovery coverage for umbral-tasks (gaps2 #85):
//!
//! 1. **double-claim guard** — two worker iterations racing on ONE pending
//!    task must result in exactly one claim. The losing worker's
//!    conditional `UPDATE ... WHERE status = 'pending'` matches zero rows
//!    (the row already flipped to `running`), so it reports no work. We
//!    sequence the race deterministically: the winner's handler blocks on a
//!    barrier AFTER its claim has committed, the loser races while the row
//!    is `running`, then the winner is released. Exactly one `Ok(true)`.
//!
//! 2. **handler-panic recovery** — a task whose handler PANICS is caught by
//!    the worker (the `spawn` + `JoinHandle::is_panic` path). The worker
//!    SURVIVES (the panic does not unwind the worker), the task is recorded
//!    as a failure (terminal at max_attempts=1, NOT silently lost), and a
//!    subsequent unrelated task still processes normally.
//!
//! Same boot shape as `integration.rs`: one OnceCell-backed tempfile sqlite
//! pool, registered TasksPlugin, raw CREATE TABLE.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, Notify, OnceCell};

use umbral_tasks::{
    _clear_handlers_for_tests, STATUS_FAILED, STATUS_RUNNING, STATUS_SUCCEEDED, TaskRow,
    TasksPlugin, enqueue, register_handler, run_worker_once,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tasks_concurrency_panic.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
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

async fn fetch(id: i64) -> TaskRow {
    let pool = umbral::db::pool();
    sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row")
}

async fn count_with_status(status: &str) -> i64 {
    let pool = umbral::db::pool();
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM task_row WHERE status = ?")
        .bind(status)
        .fetch_one(&pool)
        .await
        .expect("count")
}

async fn drain_queue() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM task_row")
        .execute(&pool)
        .await
        .expect("drain task_row");
}

static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

// =========================================================================
// 1. concurrent double-claim guard
// =========================================================================

/// Two workers race on ONE pending task. The winner claims it (flips it to
/// RUNNING) and its handler then blocks on a barrier; while it's parked, a
/// second worker attempts to claim the SAME row and must lose — the
/// conditional UPDATE finds it no longer `pending`, so `run_worker_once`
/// returns `Ok(false)`. Exactly one worker processes the task.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workers_racing_one_task_claim_it_exactly_once() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    // The handler signals when it has begun (so the loser is guaranteed to
    // race against an already-RUNNING row), then waits for release.
    static STARTED: OnceLock<Arc<Notify>> = OnceLock::new();
    static RELEASE: OnceLock<Arc<Notify>> = OnceLock::new();
    static RUNS: OnceLock<AtomicUsize> = OnceLock::new();
    let started = STARTED.get_or_init(|| Arc::new(Notify::new())).clone();
    let release = RELEASE.get_or_init(|| Arc::new(Notify::new())).clone();
    RUNS.get_or_init(|| AtomicUsize::new(0))
        .store(0, Ordering::SeqCst);

    register_handler("racy", |_payload: &str| async move {
        RUNS.get().unwrap().fetch_add(1, Ordering::SeqCst);
        // Tell the test the claim has committed and we're inside the handler.
        STARTED.get().unwrap().notify_one();
        // Park until the test releases us, holding the row in RUNNING so the
        // racing worker can only see a non-pending row.
        RELEASE.get().unwrap().notified().await;
        Ok(())
    });

    let id = enqueue("racy", serde_json::json!({}), Default::default())
        .await
        .expect("enqueue");

    // Winner: claims, runs the handler (which blocks on RELEASE).
    let winner = tokio::spawn(async { run_worker_once().await });

    // Wait until the winner is inside the handler (claim committed, row is
    // RUNNING). Bound the wait so a regression fails loud instead of hanging.
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("winner handler should start within 5s");

    // Sanity: the row is RUNNING right now — exactly the window where a
    // second claim must be rejected.
    let mid = fetch(id).await;
    assert_eq!(mid.status, STATUS_RUNNING, "row must be RUNNING mid-flight");

    // Loser: races against the RUNNING row. The conditional claim must miss.
    let loser_processed = run_worker_once().await.expect("loser worker step");
    assert!(
        !loser_processed,
        "the second worker must NOT double-claim an already-running task"
    );

    // Release the winner and collect its result.
    release.notify_one();
    let winner_processed = winner.await.expect("winner join").expect("winner step");
    assert!(winner_processed, "the winning worker processed the task");

    // Exactly one handler invocation, one succeeded row.
    assert_eq!(
        RUNS.get().unwrap().load(Ordering::SeqCst),
        1,
        "the handler must have run exactly once across both workers"
    );
    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_SUCCEEDED, "the one claim ran to success");
    assert_eq!(row.attempts, 1, "exactly one attempt counted");
}

// =========================================================================
// 2. handler-panic recovery
// =========================================================================

/// A panicking handler is caught: the worker survives, the task is recorded
/// failed (not lost), and a later task still processes.
#[tokio::test(flavor = "multi_thread")]
async fn panicking_handler_is_caught_worker_survives_task_recorded_failed() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_handler("boom", |_payload: &str| async move {
        panic!("handler intentionally panics");
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    });

    static OK_RAN: OnceLock<AtomicBool> = OnceLock::new();
    OK_RAN
        .get_or_init(|| AtomicBool::new(false))
        .store(false, Ordering::SeqCst);
    register_handler("after_boom", |_payload: &str| async move {
        OK_RAN.get().unwrap().store(true, Ordering::SeqCst);
        Ok(())
    });

    // max_attempts=1 so the single panic is terminal (failed), not retried.
    let boom_id = enqueue(
        "boom",
        serde_json::json!({}),
        umbral_tasks::EnqueueOptions {
            max_attempts: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue boom");

    // The worker step must RETURN (not unwind) despite the handler panic.
    let processed = run_worker_once()
        .await
        .expect("worker survives handler panic");
    assert!(processed, "a panicking task still counts as processed");

    let row = fetch(boom_id).await;
    assert_eq!(
        row.status, STATUS_FAILED,
        "a panicking handler at max_attempts=1 is a terminal failure"
    );
    assert_eq!(row.attempts, 1, "the panic counts as one attempt");
    assert!(
        row.completed_at.is_some(),
        "completed_at set on terminal failure (task not left dangling)"
    );
    let err = row.error.as_deref().unwrap_or("");
    assert!(
        err.contains("panic"),
        "the failure must be recorded as a panic; got {err:?}"
    );
    // The task was NOT silently lost — no RUNNING/PENDING row left behind.
    assert_eq!(
        count_with_status(STATUS_RUNNING).await,
        0,
        "no row left stuck in RUNNING after the panic"
    );

    // A subsequent, unrelated task processes normally — the worker is alive.
    let ok_id = enqueue("after_boom", serde_json::json!({}), Default::default())
        .await
        .expect("enqueue after_boom");
    let processed = run_worker_once()
        .await
        .expect("worker still works after panic");
    assert!(
        processed,
        "the worker processes the next task after a panic"
    );
    assert!(
        OK_RAN.get().unwrap().load(Ordering::SeqCst),
        "the post-panic handler ran"
    );
    let row = fetch(ok_id).await;
    assert_eq!(
        row.status, STATUS_SUCCEEDED,
        "the post-panic task succeeded"
    );
}
