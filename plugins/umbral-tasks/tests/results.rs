//! Result backend + task-status API coverage.
//!
//! Same boot shape as `integration.rs`: one OnceCell-backed tempfile sqlite
//! pool, a registered `TasksPlugin`, and a raw-SQL `CREATE TABLE` (the test
//! owns its own schema rather than standing up the M5 migration loop). The
//! one difference is the `result` column the result backend writes into.
//!
//! These tests prove:
//! - a value-returning handler's result is persisted and read back via
//!   `task_status` as the parsed JSON,
//! - a unit-returning handler stores `null` (and still compiles unchanged —
//!   that's the backward-compat proof the existing suites also carry),
//! - a failing handler leaves `result == None` and records `error`,
//! - a freshly-enqueued task reads as `Pending`,
//! - `await_result` resolves once the worker processes the task.

use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbral_tasks::{
    _clear_handlers_for_tests, EnqueueOptions, TaskState, TasksPlugin, await_result, enqueue,
    register_handler, run_worker_once, task_status,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tasks_results.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
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

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
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
// 1. Value-returning handler: result is persisted and read back.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn value_returning_handler_records_result() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    // Handler returns Ok(42i64): R infers as i64, serialized to JSON 42.
    register_handler("returns_int", |_payload: &str| async move {
        Ok::<i64, String>(42)
    });

    let id = enqueue(
        "returns_int",
        serde_json::json!({}),
        EnqueueOptions::default(),
    )
    .await
    .expect("enqueue");

    let ran = run_worker_once().await.expect("worker once");
    assert!(ran, "worker should have processed the task");

    let status = task_status(id)
        .await
        .expect("task_status")
        .expect("row exists");
    assert_eq!(status.state, TaskState::Success);
    assert_eq!(status.result, Some(serde_json::json!(42)));
    assert_eq!(status.error, None);
    assert!(status.completed_at.is_some());
}

// =========================================================================
// 2. Struct-returning handler: arbitrary serializable result round-trips.
// =========================================================================

#[derive(Debug, Serialize, Deserialize)]
struct Receipt {
    total: i64,
    label: String,
}

#[tokio::test(flavor = "multi_thread")]
async fn struct_returning_handler_round_trips_result() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_handler("returns_struct", |_payload: &str| async move {
        Ok::<Receipt, String>(Receipt {
            total: 7,
            label: "ok".to_string(),
        })
    });

    let id = enqueue(
        "returns_struct",
        serde_json::json!({}),
        EnqueueOptions::default(),
    )
    .await
    .expect("enqueue");

    run_worker_once().await.expect("worker once");

    let status = task_status(id).await.expect("status").expect("row");
    assert_eq!(status.state, TaskState::Success);
    assert_eq!(
        status.result,
        Some(serde_json::json!({ "total": 7, "label": "ok" }))
    );
}

// =========================================================================
// 3. Unit-returning handler: stores `null` (backward-compat shape).
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn unit_returning_handler_stores_null_result() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    // The pre-existing handler shape — `Ok(())` — compiles unchanged.
    register_handler("returns_unit", |_payload: &str| async move { Ok(()) });

    let id = enqueue(
        "returns_unit",
        serde_json::json!({}),
        EnqueueOptions::default(),
    )
    .await
    .expect("enqueue");

    run_worker_once().await.expect("worker once");

    let status = task_status(id).await.expect("status").expect("row");
    assert_eq!(status.state, TaskState::Success);
    // `()` serializes to JSON null.
    assert_eq!(status.result, Some(serde_json::Value::Null));
    assert_eq!(status.error, None);
}

// =========================================================================
// 4. Failing handler past max_attempts: Failed, error set, result None.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn failing_handler_leaves_result_none_and_records_error() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_handler("boom", |_payload: &str| async move {
        Err::<(), String>("boom".to_string())
    });

    // max_attempts = 1 so a single worker pass exhausts it -> Failed.
    let id = enqueue(
        "boom",
        serde_json::json!({}),
        EnqueueOptions {
            max_attempts: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    run_worker_once().await.expect("worker once");

    let status = task_status(id).await.expect("status").expect("row");
    assert_eq!(status.state, TaskState::Failed);
    assert_eq!(status.error.as_deref(), Some("boom"));
    assert_eq!(status.result, None);
}

// =========================================================================
// 5. Freshly enqueued (not yet run) task reads as Pending.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn freshly_enqueued_task_is_pending() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    let id = enqueue(
        "never_run",
        serde_json::json!({}),
        EnqueueOptions::default(),
    )
    .await
    .expect("enqueue");

    let status = task_status(id).await.expect("status").expect("row");
    assert_eq!(status.state, TaskState::Pending);
    assert_eq!(status.result, None);
    assert_eq!(status.attempts, 0);

    // Drain so we don't leave a handler-less pending row for the next test.
    drain_queue().await;
}

// =========================================================================
// 6. task_status of an unknown id returns None.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn task_status_of_unknown_id_is_none() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;

    let status = task_status(999_999).await.expect("status");
    assert!(status.is_none());
}

// =========================================================================
// 7. await_result resolves once the worker processes the task.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn await_result_resolves_after_worker_runs() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_handler("awaited", |_payload: &str| async move {
        Ok::<&str, String>("done")
    });

    let id = enqueue("awaited", serde_json::json!({}), EnqueueOptions::default())
        .await
        .expect("enqueue");

    // Run the worker, then await — the row is already terminal, so this
    // returns on the first poll.
    run_worker_once().await.expect("worker once");

    let status = await_result(id, Duration::from_secs(2))
        .await
        .expect("await_result resolves to terminal status");
    assert_eq!(status.state, TaskState::Success);
    assert_eq!(status.result, Some(serde_json::json!("done")));
}

// =========================================================================
// 8. await_result times out on a task that never runs.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn await_result_times_out_on_unprocessed_task() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    let id = enqueue("stuck", serde_json::json!({}), EnqueueOptions::default())
        .await
        .expect("enqueue");

    // No worker run -> stays Pending -> times out with the last status.
    let err = await_result(id, Duration::from_millis(150))
        .await
        .expect_err("should time out");
    match err {
        umbral_tasks::TaskError::Timeout(last) => {
            let last = *last;
            let last = last.expect("a non-terminal status was observed");
            assert_eq!(last.state, TaskState::Pending);
        }
        other => panic!("expected Timeout, got {other:?}"),
    }

    drain_queue().await;
}
