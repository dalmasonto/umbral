//! Coverage for `StaticPlugin::embedded` — the in-memory variant
//! backed by an `include_dir!()` tree.
//!
//! The test fixture is `tests/fixtures/`: a tiny directory with one
//! CSS file, one JS file, and a nested woff2-named (just bytes)
//! "font" file. `include_dir!` bakes those into the test binary at
//! compile time, the plugin serves them through its mount.

use axum::body::Body;
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use include_dir::{Dir, include_dir};
use tower::ServiceExt;
use umbra::prelude::*;
use umbra_static::StaticPlugin;

/// The fixture tree. include_dir resolves the path relative to
/// CARGO_MANIFEST_DIR (the crate root) at macro-expansion time.
static FIXTURE: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures");

async fn body_bytes(resp: http::Response<Body>) -> (StatusCode, Vec<u8>) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

async fn body_string(resp: http::Response<Body>) -> (StatusCode, String) {
    let (status, b) = body_bytes(resp).await;
    (status, String::from_utf8_lossy(&b).into_owned())
}

#[tokio::test]
async fn embedded_css_file_serves_with_correct_mime() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/sample.css")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ct.starts_with("text/css"),
        "expected text/css content-type, got: {ct}",
    );
    assert!(
        body.contains("body"),
        "expected fixture body in response, got: {body:?}",
    );
}

#[tokio::test]
async fn embedded_js_file_serves_with_correct_mime() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/sample.js")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.starts_with("application/javascript") || ct.starts_with("text/javascript"),
        "expected javascript content-type, got: {ct}",
    );
    let (status, _) = body_bytes(resp).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn embedded_nested_file_serves_through_subdirectory_path() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/nested/inside.txt")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("nested"),
        "expected nested fixture body, got: {body:?}",
    );
}

#[tokio::test]
async fn embedded_missing_file_returns_404() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/does-not-exist.css")
        .body(Body::empty())
        .unwrap();
    let (status, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn embedded_path_traversal_attempt_returns_404_not_an_escape() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);
    let router = plugin.routes();

    // `../Cargo.toml` would escape the fixture dir on a filesystem;
    // against an in-memory tree it's just a key that doesn't match.
    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/../Cargo.toml")
        .body(Body::empty())
        .unwrap();
    let (status, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn embedded_dir_accessor_returns_none() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);
    assert!(
        plugin.dir().is_none(),
        "embedded plugins have no on-disk directory; dir() must return None",
    );
    assert_eq!(plugin.mount(), "/static");
}
