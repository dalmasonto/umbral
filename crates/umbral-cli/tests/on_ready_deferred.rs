//! gaps3 #41 — `on_ready` fires when the app is up, not when it is built.
//!
//! `on_ready` means *the application is running*. Plugins seed content, backfill
//! rows, and (in `umbral-permissions`) create the standard permission rows for
//! every registered model. All of it needs a migrated schema.
//!
//! It used to fire as the last phase of `App::build()`, and `main.rs` is
//! `let app = App::builder()…build()?; umbral_cli::dispatch(app).await` — so the
//! hooks ran before `dispatch` had even parsed argv, including when argv said
//! `migrate`. On the first umbralrs.dev deploy that produced a wall of
//! `relation "..." does not exist` before the migration engine had created a
//! single table. It survived only because those seeds log-and-swallow: a plugin
//! that propagated the error made `migrate` unrunnable, and one that performed a
//! write silently skipped it.
//!
//! `AppBuilder::build_deferred()` wires without firing; `dispatch` decides.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::{App, Settings};

/// Counts `on_ready` firings. A real plugin seeds rows here; the count is the
/// observable stand-in.
#[derive(Clone, Debug)]
struct SeedingPlugin {
    fired: Arc<AtomicUsize>,
}

impl Plugin for SeedingPlugin {
    fn name(&self) -> &'static str {
        "seeding"
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        self.fired.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// `App::build*()` publishes process-global `OnceLock`s (`db::init`, the model
/// registry) and panics on a second call, so each of these cases lives in its
/// own test binary with exactly one build.
async fn deferred_app() -> (App, Arc<AtomicUsize>) {
    let settings = Settings::from_env().expect("figment defaults always load");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");

    let fired = Arc::new(AtomicUsize::new(0));
    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(SeedingPlugin {
            fired: fired.clone(),
        })
        .build_deferred()
        .expect("App::build_deferred");

    (app, fired)
}

#[allow(dead_code)]
fn argv(args: &[&str]) -> Vec<std::ffi::OsString> {
    std::iter::once("umbral".into())
        .chain(args.iter().map(|a| std::ffi::OsString::from(*a)))
        .collect()
}

/// The premise every other case rests on: `build_deferred()` leaves the hooks
/// unfired, `ready()` fires them, and a second `ready()` does not re-seed
/// (`serve()` and `dispatch` may both call it).
#[tokio::test]
async fn build_deferred_defers_and_ready_is_idempotent() {
    let (app, fired) = deferred_app().await;

    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "build_deferred() must not fire on_ready",
    );
    assert!(!app.ready_already_fired());

    app.ready().expect("ready");
    assert_eq!(fired.load(Ordering::SeqCst), 1, "ready() fires the hook");
    assert!(app.ready_already_fired());

    app.ready().expect("ready again");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "ready() is idempotent — a second call must not re-seed",
    );
}
