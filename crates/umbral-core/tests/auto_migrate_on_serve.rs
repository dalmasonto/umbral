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

    // gaps4 #47: the seed hook rides the same builder → App threading. A
    // flag records the call so the test can prove the CLI-facing accessor
    // hands back the exact closure that was registered.
    static SEED_RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .auto_migrate_on_serve()
        .seed_on_serve(|| async {
            SEED_RAN.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .build()
        .expect("build should succeed");

    assert!(
        app.auto_migrate_on_serve_enabled(),
        "opting in via .auto_migrate_on_serve() must be readable on the built App \
         (the CLI serve path gates the migrate on it)"
    );

    let hook = app
        .seed_on_serve_hook()
        .expect("the seed hook threads through to the built App");
    hook().await.expect("the demo seed succeeds");
    assert!(
        SEED_RAN.load(std::sync::atomic::Ordering::SeqCst),
        "invoking the accessor's hook runs the registered closure"
    );
}
