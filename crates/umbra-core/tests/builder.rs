//! End-to-end coverage for `App::builder().build()`.
//!
//! `settings::init` and `db::init` write to process-wide `OnceLock`s, so
//! the unit tests in `src/db.rs` and `src/settings.rs` can't exercise them
//! under cargo's parallel runner. Integration tests under `tests/` sidestep
//! that: each file is compiled into its own test binary and runs in its
//! own process, so each file gets a fresh `OnceLock`. The price is that
//! within a single file, `build()` may only be called once — the second
//! call would trip the "init called more than once" panic. That's why the
//! companion auto-connect scenario lives in `builder_defaults.rs`.

use umbra_core::app::App;
use umbra_core::db;
use umbra_core::routes::Routes;
use umbra_core::settings::Settings;

/// The happy path: hand a settings struct, an in-memory pool, and a small
/// route bundle to the builder and confirm everything wires together.
#[tokio::test]
async fn build_succeeds_with_explicit_pool_and_router() {
    let settings = Settings::from_env().expect("figment defaults always load");

    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .routes(Routes::new().get("/ping", || async { "pong" }))
        .build();

    assert!(
        result.is_ok(),
        "builder should succeed on the happy path, got {:?}",
        result.err(),
    );

    // Ambient accessors must work once `build()` has published state.
    let _pool = db::pool();
    let _settings = umbra_core::settings::get();
}
