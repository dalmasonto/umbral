//! Tests for the `#[task]` attribute macro.
//!
//! Covers:
//!   1. Success path: macro expands and the generated `register_*` function
//!      correctly calls `umbra_tasks::register_handler`.
//!   2. Optional `name = "..."` override propagates to the registration key.
//!   3. Rejection: non-async fn emits a compile error.
//!   4. Rejection: zero parameters emits a compile error.
//!   5. Rejection: two or more parameters emit a compile error.
//!   6. Rejection: wrong return type emits a compile error.
//!
//! Tests 3-6 use `trybuild` to assert the macro produces `compile_error!`
//! with an appropriate message. Tests 1-2 run inline via the
//! `umbra_tasks` handler registry.

use std::sync::OnceLock;

use tokio::sync::{Mutex, OnceCell};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("settings from env");
        let pool = sqlx::SqlitePool::connect(":memory:")
            .await
            .expect("in-memory pool");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .build()
            .expect("App::build");

        // Create the task_row table so enqueue works.
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
// Payload + task declarations (expanded at crate level so the
// generated `register_*` functions are in scope in the tests below).
// =========================================================================

#[derive(serde::Serialize, serde::Deserialize)]
struct TestPayload {
    value: i64,
}

/// Default name: task name == Rust fn name "process_item".
#[umbra::task]
async fn process_item(payload: TestPayload) -> Result<(), String> {
    let _ = payload.value;
    Ok(())
}

/// Name override: task name is "myapp.process_item_v2".
#[umbra::task(name = "myapp.process_item_v2")]
async fn process_item_v2(payload: TestPayload) -> Result<(), String> {
    let _ = payload.value;
    Ok(())
}

// =========================================================================
// 1. Happy path: register → enqueue → worker_once → succeeded
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn task_macro_registers_handler_under_fn_name() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    umbra_tasks::_clear_handlers_for_tests();

    // Generated companion function calls register_handler("process_item", …).
    register_process_item();

    let id = umbra_tasks::enqueue(
        "process_item",
        TestPayload { value: 42 },
        umbra_tasks::EnqueueOptions::default(),
    )
    .await
    .expect("enqueue");

    let processed = umbra_tasks::run_worker_once().await.expect("worker step");
    assert!(processed, "worker should have claimed the task");

    let row: (String,) = sqlx::query_as("SELECT status FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&umbra::db::pool())
        .await
        .expect("fetch row");
    assert_eq!(row.0, umbra_tasks::STATUS_SUCCEEDED);
}

// =========================================================================
// 2. Name override: registered under custom key, not Rust fn name
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn task_macro_name_override_registers_under_custom_key() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    umbra_tasks::_clear_handlers_for_tests();

    // Generated fn is `register_process_item_v2`; but the key is "myapp.process_item_v2".
    register_process_item_v2();

    let id = umbra_tasks::enqueue(
        "myapp.process_item_v2",
        TestPayload { value: 99 },
        umbra_tasks::EnqueueOptions::default(),
    )
    .await
    .expect("enqueue");

    let processed = umbra_tasks::run_worker_once().await.expect("worker step");
    assert!(processed, "worker should have claimed the task");

    let row: (String,) = sqlx::query_as("SELECT status FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&umbra::db::pool())
        .await
        .expect("fetch row");
    assert_eq!(row.0, umbra_tasks::STATUS_SUCCEEDED);
}

// =========================================================================
// Rejection tests via trybuild
// =========================================================================

#[test]
fn task_macro_rejects_non_async_fn() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/task_macro_fixtures/non_async_fn.rs");
}

#[test]
fn task_macro_rejects_zero_params() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/task_macro_fixtures/zero_params.rs");
}

#[test]
fn task_macro_rejects_two_params() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/task_macro_fixtures/two_params.rs");
}

#[test]
fn task_macro_rejects_wrong_return_type() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/task_macro_fixtures/wrong_return.rs");
}
