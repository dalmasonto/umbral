//! A `StaticPlugin` mounted at the configured `static_url` must coexist
//! with the framework's unified static pipeline instead of colliding
//! with it.
//!
//! Regression test for the boot panic
//! `Invalid route "/static/{*__private__axum_nest_tail_param}": conflict`
//! — two catch-all nests at `/static`, one from `StaticPlugin::new`'s
//! `nest_service` and one from `App::build`'s Phase-5.45 pipeline mount.
//! The fix: a filesystem `StaticPlugin` whose mount equals `static_url`
//! contributes its directory to the pipeline via
//! `Plugin::static_root_dirs()` and returns an empty `routes()`, so the
//! framework owns `static_url` as ONE mount. The site's own files are
//! served at the bare `/static/<file>` space; plugin assets keep their
//! `/static/<namespace>/<file>` space.

use std::fs;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbra::prelude::{Plugin, StaticDir};
use umbra::{App, Environment, Settings};
use umbra_static::StaticPlugin;

/// A stand-in plugin contributing a namespaced static source dir, the
/// way `umbra-admin` / `umbra-playground` do via `static_dirs()`.
#[derive(Clone)]
struct AssetPlugin {
    source: PathBuf,
}

impl Plugin for AssetPlugin {
    fn name(&self) -> &'static str {
        "asset"
    }

    fn static_dirs(&self) -> Vec<StaticDir> {
        vec![StaticDir::new("playground", self.source.clone())]
    }
}

async fn body_string(res: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 16)
        .await
        .expect("read body");
    String::from_utf8(bytes.to_vec()).expect("utf8")
}

#[tokio::test]
async fn static_plugin_at_static_url_coexists_with_the_pipeline() {
    // Site-level static the app serves at the bare `/static/` space.
    let site = tempfile::tempdir().expect("site tmp");
    fs::create_dir_all(site.path().join("css")).unwrap();
    fs::write(site.path().join("css/site.css"), b"body{color:red}").unwrap();

    // A plugin's namespaced source dir, served at `/static/playground/`.
    let pg = tempfile::tempdir().expect("pg tmp");
    fs::create_dir_all(pg.path().join("assets")).unwrap();
    fs::write(pg.path().join("assets/app.js"), b"console.log(1)").unwrap();

    let mut settings = Settings::from_env().expect("figment defaults load in tests");
    // Dev so namespaced assets serve live from the source dir.
    settings.environment = Environment::Dev;
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");

    // Before the fix, this build panicked: StaticPlugin nested
    // `/static/{*rest}` AND the pipeline nested `/static/{*rest}`.
    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(StaticPlugin::new("/static", site.path()))
        .plugin(AssetPlugin {
            source: pg.path().to_path_buf(),
        })
        .build()
        .expect("App::build must not panic: StaticPlugin defers to the pipeline at static_url");

    let router = app.into_router();

    // Site file resolves at the bare `/static/` root (StaticPlugin dir
    // contributed as a pipeline root source).
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/static/css/site.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "site css must serve");
    assert_eq!(body_string(res).await, "body{color:red}");

    // Namespaced plugin asset resolves under its namespace, live from the
    // plugin's source dir (dev).
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/static/playground/assets/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "namespaced asset must serve");
    assert_eq!(body_string(res).await, "console.log(1)");

    // A path neither under a namespace nor present in the site dir is a
    // clean 404 (not a 500, not a panic).
    let res = router
        .oneshot(
            Request::builder()
                .uri("/static/nope/missing.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
