//! Admin visibility coverage for umbra-tasks (planning/features.md #82):
//! the read-only `admin_model()` config + the `retry_task` re-queue path
//! that backs the admin "Retry selected" bulk action.
//!
//! Same boot shape as `reliability.rs`: one OnceCell-backed tempfile sqlite
//! pool, registered TasksPlugin, raw SQL CREATE TABLE because the
//! integration test owns its own schema without standing up the M5
//! migration loop.

use std::sync::OnceLock;

use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbra_tasks::{
    STATUS_FAILED, STATUS_PENDING, STATUS_SUCCEEDED, TaskRow, TasksPlugin,
    _clear_handlers_for_tests, enqueue, register_handler, retry_task, run_worker_once,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tasks_admin.sqlite");
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
            .plugin(TasksPlugin::default())
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
    let pool = umbra::db::pool();
    sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row")
}

async fn drain_queue() {
    let pool = umbra::db::pool();
    sqlx::query("DELETE FROM task_row")
        .execute(&pool)
        .await
        .expect("drain");
}

/// These tests mutate process-global state (the handler registry, the
/// shared queue), so they must not interleave.
static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

// =========================================================================
// admin_model() — the read-only config the admin renders.
// =========================================================================

#[cfg(feature = "admin")]
#[test]
fn admin_model_targets_task_row_read_only_with_retry() {
    let model = umbra_tasks::admin_model();
    // We can't read the private AdminModel fields from outside the crate, so
    // assert via Debug — it carries the table + action keys + columns.
    let dbg = format!("{model:?}");
    assert!(
        dbg.contains("task_row"),
        "admin_model targets the task_row table; debug = {dbg}"
    );
    assert!(
        dbg.contains("retry_failed"),
        "admin_model attaches the retry action; debug = {dbg}"
    );
    // A few columns an operator wants at a glance.
    for col in ["name", "status", "attempts", "completed_at"] {
        assert!(
            dbg.contains(col),
            "admin_model list_display includes `{col}`; debug = {dbg}"
        );
    }
}

// =========================================================================
// retry_task — re-queue a failed task.
// =========================================================================

/// A task that exhausts its attempts lands in `failed`; `retry_task` puts it
/// back to `pending` with `run_at <= now`, `error` cleared, attempts reset,
/// and a fresh `run_worker_once` then claims and completes it.
#[tokio::test(flavor = "multi_thread")]
async fn retry_requeues_a_failed_task_and_worker_picks_it_up() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    // A handler that fails on its first run but succeeds afterwards. We drive
    // it to `failed` via max_attempts = 1, then `retry_task` gives a fresh
    // budget so the next run can succeed.
    static FAIL_NEXT: OnceLock<std::sync::atomic::AtomicBool> = OnceLock::new();
    FAIL_NEXT
        .get_or_init(|| std::sync::atomic::AtomicBool::new(true))
        .store(true, std::sync::atomic::Ordering::SeqCst);
    register_handler("flaky", |_payload: &str| async move {
        let fail = FAIL_NEXT
            .get()
            .unwrap()
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        if fail {
            Err::<(), _>("boom".to_string())
        } else {
            Ok(())
        }
    });

    let id = enqueue(
        "flaky",
        &(),
        umbra_tasks::EnqueueOptions {
            max_attempts: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    // Drive the (only) attempt — it fails and, with max_attempts = 1, the row
    // is terminal `failed`.
    assert!(run_worker_once().await.expect("worker run 1"));
    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_FAILED, "exhausted -> failed");
    assert!(row.error.is_some(), "failure reason recorded");

    // Retry it.
    let did = retry_task(id).await.expect("retry");
    assert!(did, "retry_task re-queued the failed row");

    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_PENDING, "retry -> pending");
    assert_eq!(row.attempts, 0, "attempts reset to a fresh budget");
    assert!(row.error.is_none(), "error cleared on retry");
    assert!(row.completed_at.is_none(), "no longer terminal");
    let run_at = row.run_at.expect("run_at set on retry");
    assert!(
        run_at <= Utc::now(),
        "run_at <= now so the row is immediately eligible"
    );

    // The worker now claims it and, on this second run, the handler succeeds.
    assert!(run_worker_once().await.expect("worker run 2"));
    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_SUCCEEDED, "retried task succeeds");
}

/// `retry_task` only touches a *failed* row: a pending/succeeded/absent id is
/// left untouched and returns `false`.
#[tokio::test(flavor = "multi_thread")]
async fn retry_is_a_noop_on_non_failed_or_absent() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_handler("ok_task", |_payload: &str| async move { Ok::<(), String>(()) });

    // Absent id.
    assert!(
        !retry_task(999_999).await.expect("retry absent"),
        "retry_task on a missing id returns false"
    );

    // Pending (freshly enqueued, never run) id is not disturbed.
    let pending_id = enqueue("ok_task", &(), umbra_tasks::EnqueueOptions::default())
        .await
        .expect("enqueue pending");
    assert!(
        !retry_task(pending_id).await.expect("retry pending"),
        "retry_task on a pending row returns false"
    );
    assert_eq!(
        fetch(pending_id).await.status,
        STATUS_PENDING,
        "pending row untouched by retry"
    );

    // Succeeded id is not disturbed either. Drain first so `done_id` is the
    // only claimable row — `run_worker_once` claims exactly one task and the
    // still-pending row above would otherwise be claimed ahead of it (FIFO).
    drain_queue().await;
    let done_id = enqueue("ok_task", &(), umbra_tasks::EnqueueOptions::default())
        .await
        .expect("enqueue success");
    assert!(run_worker_once().await.expect("run done_id"));
    assert_eq!(fetch(done_id).await.status, STATUS_SUCCEEDED);
    assert!(
        !retry_task(done_id).await.expect("retry succeeded"),
        "retry_task on a succeeded row returns false"
    );
    assert_eq!(
        fetch(done_id).await.status,
        STATUS_SUCCEEDED,
        "succeeded row untouched by retry"
    );
}
