//! Coverage for the auto-connect path: no `.database(...)` call, so
//! `AppBuilder::build()` must open a default pool from
//! `settings.database_url` on its own.
//!
//! Lives in a separate file from `builder.rs` because both tests need to
//! call `App::builder().build()`, and `settings::init` / `db::init` each
//! own a process-wide `OnceLock`. Splitting on file boundaries gives each
//! scenario a fresh process and a fresh lock.
//!
//! The test is deliberately a plain `#[test]`, not `#[tokio::test]`. When
//! no pool is registered, `AppBuilder::build()` spins up its own
//! current-thread runtime to drive `db::connect`, and calling that from
//! inside an outer tokio runtime panics with "Cannot start a runtime from
//! within a runtime."

use umbra_core::app::App;
use umbra_core::settings::Settings;

/// `build()` with no explicit database opens a pool from
/// `settings.database_url`. We point that URL at an in-memory sqlite so
/// the test leaves no file on disk.
#[test]
fn build_auto_connects_default_pool_from_settings() {
    // Env vars are process-global, but every integration-test file runs
    // in its own process, so this can't leak into another test.
    // SAFETY: single-threaded test, no other thread is reading env.
    unsafe {
        std::env::set_var("UMBRA_DATABASE_URL", "sqlite::memory:");
    }

    let settings = Settings::from_env().expect("figment defaults always load");
    assert_eq!(settings.database_url, "sqlite::memory:");

    let result = App::builder().settings(settings).build();

    assert!(
        result.is_ok(),
        "builder should auto-connect a default pool, got {:?}",
        result.err(),
    );

    // The auto-connected pool must be reachable through the ambient accessor.
    let _pool = umbra_core::db::pool();
}
