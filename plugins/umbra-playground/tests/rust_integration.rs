//! Integration test: the PlaygroundPlugin serves a 200 HTML shell
//! at its base path and 404s on unknown asset paths.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbra::prelude::Plugin;
use umbra_playground::PlaygroundPlugin;

#[tokio::test]
async fn shell_returns_200_html() {
    let plugin = PlaygroundPlugin::new();
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();
    let app = plugin.routes();

    let req = Request::builder()
        .uri(format!("/{base}/"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let s = String::from_utf8_lossy(&body);
    assert!(
        s.contains("<!doctype html>"),
        "expected HTML shell, got: {s}"
    );
}

#[tokio::test]
async fn missing_asset_returns_404() {
    let plugin = PlaygroundPlugin::new();
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();
    let app = plugin.routes();

    let req = Request::builder()
        .uri(format!("/{base}/assets/does-not-exist.js"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
