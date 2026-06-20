//! Tests for the `umbra::plugin::block_on_ready` sync→async bridge.
//!
//! This file specifically exercises the runtime scenarios that the old
//! `Handle::current().block_on(...)` bridge in `umbra-rls` did NOT
//! survive:
//!
//! - `#[tokio::test]` (default: current-thread runtime) — the bare
//!   `block_on` form panics here; `block_on_ready` must not.
//! - No ambient runtime (bare sync context) — `block_on_ready` must
//!   drive the future on a temporary runtime.
//!
//! Tests use a trivial async block (no real DB) so CI can run them
//! without a Postgres connection.

use umbra::plugin::block_on_ready;

// -----------------------------------------------------------------
// 1. Current-thread runtime — the #[tokio::test] default.
//    This is the scenario that panicked with the old bridge.
// -----------------------------------------------------------------

#[tokio::test]
async fn block_on_ready_works_under_current_thread_runtime() {
    // We are now inside a current-thread tokio runtime (the
    // #[tokio::test] default). Calling the old bare
    // `Handle::current().block_on(...)` here would panic with:
    //   "Cannot start a runtime from within a runtime. This
    //    happens because a function (like `block_on`) attempted
    //    to block the current thread while the thread is being
    //    used to drive asynchronous tasks."
    //
    // block_on_ready detects the current-thread flavor and
    // escapes to a dedicated OS thread instead.
    let result = block_on_ready(async { 42u32 });
    assert_eq!(result, 42);
}

#[tokio::test]
async fn block_on_ready_propagates_return_value() {
    let result = block_on_ready(async { String::from("hello from block_on_ready") });
    assert_eq!(result, "hello from block_on_ready");
}

#[tokio::test]
async fn block_on_ready_can_be_called_multiple_times_sequentially() {
    // Verifies no one-time-initialization or stale-handle issues.
    let a = block_on_ready(async { 1u32 });
    let b = block_on_ready(async { 2u32 });
    let c = block_on_ready(async { 3u32 });
    assert_eq!(a + b + c, 6);
}

// -----------------------------------------------------------------
// 2. No ambient runtime — bare sync test (no #[tokio::test]).
//    block_on_ready must create a temporary Runtime and drive the
//    future itself.
// -----------------------------------------------------------------

#[test]
fn block_on_ready_works_with_no_ambient_runtime() {
    // There is no tokio runtime on this thread. block_on_ready
    // must create one internally rather than panicking.
    let result = block_on_ready(async { 99u32 });
    assert_eq!(result, 99);
}

#[test]
fn block_on_ready_no_runtime_propagates_return_value() {
    let result = block_on_ready(async { "bare sync context".to_string() });
    assert_eq!(result, "bare sync context");
}

// -----------------------------------------------------------------
// 3. Multi-thread runtime — the production path.
//    block_on_ready must use block_in_place without panicking.
// -----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_on_ready_works_under_multi_thread_runtime() {
    let result = block_on_ready(async { 7u32 });
    assert_eq!(result, 7);
}

// -----------------------------------------------------------------
// 4. The rls SQLite skip path still works under current-thread.
//    A lightweight proxy that calls the skip branch of on_ready
//    without needing a real Postgres pool.
// -----------------------------------------------------------------

#[tokio::test]
async fn rls_on_ready_skip_path_does_not_panic_under_tokio_test() {
    use umbra::Settings;
    use umbra::prelude::*;
    use umbra_rls::{Action, RlsPlugin};

    let mut settings = Settings::from_env().expect("figment defaults");
    settings.database_url = "sqlite::memory:".to_string();
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    // This calls RlsPlugin::on_ready under a current-thread runtime
    // (the #[tokio::test] default). With the old bare block_on bridge
    // inside the Postgres branch this would panic IF a Postgres pool
    // were present. The SQLite skip branch doesn't call block_on at
    // all, so this test confirms the skip path still boots cleanly.
    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(
            RlsPlugin::new()
                .enable_on("some_table")
                .policy("some_table", "owner_read", Action::Select, "user_id = 1"),
        )
        .build();

    assert!(
        result.is_ok(),
        "RlsPlugin SQLite skip path panicked or errored"
    );
}
