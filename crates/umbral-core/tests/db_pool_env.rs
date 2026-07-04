//! audit_2 H16 — the default DB pool is opened via `db::connect()` BEFORE
//! `App::build()` publishes the ambient settings, so `PoolConfig::resolve()`
//! must fall back to reading `UMBRAL_DB_*` straight from the environment.
//! Otherwise an operator's tuning knobs are silently discarded for the pool
//! that serves ALL traffic.
//!
//! Own test binary so `App::build()` is never called here — `settings::get_opt()`
//! stays `None`, exercising exactly the pre-build boot order the finding describes.

use sqlx::Pool;

#[tokio::test]
async fn umbral_db_env_applies_when_pool_opened_before_build() {
    // Pre-build boot order: no ambient settings published yet.
    // SAFETY: single-threaded test start; no other thread reads the env here.
    unsafe {
        std::env::set_var("UMBRAL_DB_MAX_CONNECTIONS", "7");
    }

    let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("open sqlite pool");

    // The knob reached the pool via the env fallback, not the hardcoded default of 10.
    assert_eq!(
        pool.options().get_max_connections(),
        7,
        "UMBRAL_DB_MAX_CONNECTIONS must apply to a pool opened before App::build()"
    );

    // SAFETY: end of test; nothing else depends on this var afterwards.
    unsafe {
        std::env::remove_var("UMBRAL_DB_MAX_CONNECTIONS");
    }

    // Silence the unused-import lint if the assoc-fn path above is inlined by rustc.
    let _ = Pool::<sqlx::Sqlite>::connect_lazy;
}
