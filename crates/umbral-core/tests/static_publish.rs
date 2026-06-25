//! `published_static()` round-trips through a real `App::build`.
//!
//! The `collectstatic` plugin command (in `umbral-storage`) can't be
//! handed the plugin list — `PluginCommand::run` doesn't receive it — so
//! `App::build` publishes every plugin's `static_dirs()` (namespaced)
//! and `static_root_dirs()` (app/site) into the ambient
//! `static_files::PUBLISHED` slot, mirroring the `settings` OnceLock.
//! This test builds a minimal App with a fake plugin that contributes
//! both kinds of static dir and asserts the published contributions and
//! root dirs come back through `published_static()`.
//!
//! One `App::build` per test binary: the static `PUBLISHED` (and
//! `settings::SETTINGS`) OnceLocks are per-process, so a second build
//! would be a no-op. Follows the App-build pattern in
//! `crates/umbral-core/tests/plugin_contract.rs`.

use std::path::PathBuf;

use umbral::Settings;
use umbral::plugin::{Plugin, StaticDir};

/// A plugin that contributes one namespaced static dir and one app/site
/// root dir, so the published round-trip can be checked for both kinds.
struct StaticFixturePlugin;

impl Plugin for StaticFixturePlugin {
    fn name(&self) -> &'static str {
        "static-fixture"
    }

    fn static_dirs(&self) -> Vec<StaticDir> {
        vec![StaticDir::new("fixture", "/src/fixture/static")]
    }

    fn static_root_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from("/src/site/static")]
    }
}

#[tokio::test]
async fn published_static_round_trips_through_app_build() {
    // Before any App::build in this process, nothing is published.
    assert!(
        umbral::static_files::published_static().is_none(),
        "published_static must be None before App::build"
    );

    let settings = Settings::from_env().expect("figment defaults always load in a test env");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite pool");

    let _app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(StaticFixturePlugin)
        .build()
        .expect("App builds with the static fixture plugin");

    let published =
        umbral::static_files::published_static().expect("App::build publishes static contributions");

    // The namespaced contribution round-trips: namespace, source dir, and
    // the contributing plugin's name.
    let fixture = published
        .contributions
        .iter()
        .find(|c| c.namespace == "fixture")
        .expect("fixture namespace published");
    assert_eq!(fixture.source_dir, PathBuf::from("/src/fixture/static"));
    assert_eq!(fixture.plugin, "static-fixture");

    // The app/site root dir round-trips.
    assert!(
        published
            .root_dirs
            .contains(&PathBuf::from("/src/site/static")),
        "static_root_dirs contribution published as a root dir, got {:?}",
        published.root_dirs
    );
}
