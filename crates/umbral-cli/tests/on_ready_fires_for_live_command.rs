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

/// The other half of the contract: a command that runs against live data still
/// fires the hooks, exactly as before the split. Regressing this would mean
/// `createsuperuser`, `worker`, or an app's own `seed_orm_data` runs without the
/// ambient state its plugins install in `on_ready`.
#[tokio::test]
async fn a_command_that_runs_against_live_data_fires_on_ready() {
    let (app, fired) = deferred_app().await;

    let out = std::env::temp_dir().join("umbral_on_ready_lifecycle_dump.json");
    let _ = umbral_cli::dispatch_with_argv(
        app,
        argv(&[
            "dumpdata",
            "--output",
            out.to_str().expect("utf-8 temp path"),
        ]),
    )
    .await;
    let _ = std::fs::remove_file(&out);

    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "dumpdata runs against a migrated database, so on_ready must fire first",
    );
}
