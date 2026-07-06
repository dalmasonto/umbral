//! gaps3 #23 — `AppBuilder::auto_migrate_on_serve()` threads through to the
//! built `App`, where `umbral_cli`'s serve path reads it. One `build()` per
//! test file (settings/db init write process-wide OnceLocks).

use umbral_core::app::App;
use umbral_core::db;
use umbral_core::settings::Settings;

#[tokio::test]
async fn auto_migrate_on_serve_opt_in_threads_through_to_the_app() {
    let settings = Settings::from_env().expect("figment defaults always load");
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .auto_migrate_on_serve()
        .build()
        .expect("build should succeed");

    assert!(
        app.auto_migrate_on_serve_enabled(),
        "opting in via .auto_migrate_on_serve() must be readable on the built App \
         (the CLI serve path gates the migrate on it)"
    );
}
