//! End-to-end integration test for the `#[task]` proc-macro.
//!
//! Verifies:
//!   1. A task declared with `#[task]` can be registered, enqueued, and
//!      processed by the worker, with the row landing in `succeeded`.
//!   2. `#[task(name = "...")]` override: the task is registered under
//!      the custom key, not the Rust function name.
//!   3. The generated registration function deserialises the payload
//!      correctly from JSON — a payload mismatch lands in `failed` with
//!      a descriptive error message.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbra_tasks::{
    _clear_handlers_for_tests, EnqueueOptions, STATUS_FAILED, STATUS_SUCCEEDED, enqueue,
    run_worker_once,
};

// =========================================================================
// Boot helpers — mirror of integration.rs
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("macro_integration.sqlite");
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
            .plugin(umbra_tasks::TasksPlugin)
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

async fn fetch_status(id: i64) -> String {
    let pool = umbra::db::pool();
    let row: (String,) = sqlx::query_as("SELECT status FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row");
    row.0
}

async fn fetch_error(id: i64) -> Option<String> {
    let pool = umbra::db::pool();
    let row: (Option<String>,) = sqlx::query_as("SELECT error FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row");
    row.0
}

async fn drain_queue() {
    let pool = umbra::db::pool();
    sqlx::query("DELETE FROM task_row")
        .execute(&pool)
        .await
        .expect("drain");
}

static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

// =========================================================================
// Task declarations
// =========================================================================

#[derive(serde::Serialize, serde::Deserialize)]
struct GreetPayload {
    name: String,
}

/// Flag set by `greet_user` to verify the handler ran.
static GREET_FLAG: OnceLock<AtomicBool> = OnceLock::new();
fn greet_flag() -> &'static AtomicBool {
    GREET_FLAG.get_or_init(|| AtomicBool::new(false))
}

#[umbra::task]
async fn greet_user(payload: GreetPayload) -> Result<(), String> {
    let _ = payload.name;
    greet_flag().store(true, Ordering::SeqCst);
    Ok(())
}

// Named override.
#[derive(serde::Serialize, serde::Deserialize)]
struct PingPayload {
    seq: u32,
}

static PING_FLAG: OnceLock<AtomicBool> = OnceLock::new();
fn ping_flag() -> &'static AtomicBool {
    PING_FLAG.get_or_init(|| AtomicBool::new(false))
}

#[umbra::task(name = "infra.ping")]
async fn do_ping(payload: PingPayload) -> Result<(), String> {
    let _ = payload.seq;
    ping_flag().store(true, Ordering::SeqCst);
    Ok(())
}

// =========================================================================
// 1. Happy path: enqueue → worker_once → succeeded
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn macro_task_processes_and_marks_succeeded() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    greet_flag().store(false, Ordering::SeqCst);

    // Use the generated registration function.
    register_greet_user();

    let id = enqueue(
        "greet_user",
        GreetPayload {
            name: "Alice".to_string(),
        },
        EnqueueOptions::default(),
    )
    .await
    .expect("enqueue");

    let processed = run_worker_once().await.expect("worker step");
    assert!(processed, "worker should have claimed and run the task");
    assert!(
        greet_flag().load(Ordering::SeqCst),
        "handler should have set the flag"
    );
    assert_eq!(fetch_status(id).await, STATUS_SUCCEEDED);
}

// =========================================================================
// 2. Name override: task registered under custom key
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn macro_task_name_override_registers_under_custom_key() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    ping_flag().store(false, Ordering::SeqCst);

    // Generated fn name is still `register_do_ping` (Rust identifier),
    // but the handler key is "infra.ping".
    register_do_ping();

    let id = enqueue(
        "infra.ping",
        PingPayload { seq: 1 },
        EnqueueOptions::default(),
    )
    .await
    .expect("enqueue");

    let processed = run_worker_once().await.expect("worker step");
    assert!(processed);
    assert!(
        ping_flag().load(Ordering::SeqCst),
        "handler should have set the flag"
    );
    assert_eq!(fetch_status(id).await, STATUS_SUCCEEDED);
}

// =========================================================================
// 3. Payload deserialise failure lands in failed with a descriptive error
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn macro_task_bad_payload_marks_failed_with_deserialise_error() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_greet_user();

    // Enqueue a payload that does NOT match GreetPayload (missing `name` field).
    let id = enqueue(
        "greet_user",
        serde_json::json!({"wrong_field": 99}),
        EnqueueOptions {
            max_attempts: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    let processed = run_worker_once().await.expect("worker step");
    assert!(processed, "worker should have claimed the task");

    // With max_attempts=1, a failure on the first attempt is terminal.
    assert_eq!(fetch_status(id).await, STATUS_FAILED);
    let err = fetch_error(id).await.unwrap_or_default();
    assert!(
        err.contains("payload deserialise error"),
        "error should mention payload deserialise error; got {err:?}",
    );
}
