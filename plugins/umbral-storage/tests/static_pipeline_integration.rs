//! A `StoragePlugin` static side mounted at the configured `static_url`
//! must coexist with the framework's unified static pipeline instead of
//! colliding with it. Moved from umbral-static.

use std::fs;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbral::prelude::{Plugin, StaticDir};
use umbral::{App, Environment, Settings};
use umbral_storage::StoragePlugin;

/// A stand-in plugin contributing a namespaced static source dir, the way
/// `umbral-admin` / `umbral-playground` do via `static_dirs()`.
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
    let site = tempfile::tempdir().expect("site tmp");
    fs::create_dir_all(site.path().join("css")).unwrap();
    fs::write(site.path().join("css/site.css"), b"body{color:red}").unwrap();

    let pg = tempfile::tempdir().expect("pg tmp");
    fs::create_dir_all(pg.path().join("assets")).unwrap();
    fs::write(pg.path().join("assets/app.js"), b"console.log(1)").unwrap();

    let mut settings = Settings::from_env().expect("figment defaults load in tests");
    settings.environment = Environment::Dev;
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(StoragePlugin::new().static_files("/static", site.path()))
        .plugin(AssetPlugin {
            source: pg.path().to_path_buf(),
        })
        .build()
        .expect("App::build must not panic: static side defers to the pipeline at static_url");

    let router = app.into_router();

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
