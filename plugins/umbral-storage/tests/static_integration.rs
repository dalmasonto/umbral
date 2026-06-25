//! Integration coverage for the StoragePlugin static side. Moved from
//! umbral-static.

use std::fs;
use std::io::Write;

use axum::body::Body;
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tempfile::tempdir;
use tower::ServiceExt;
use umbral::prelude::*;
use umbral_storage::StoragePlugin;

async fn body_string(resp: http::Response<Body>) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn served_files_round_trip_at_the_mount_path() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("hello.txt");
    let mut f = fs::File::create(&path).unwrap();
    writeln!(f, "from umbral-storage").unwrap();
    drop(f);

    let plugin = StoragePlugin::new().static_files("/static", dir.path());
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/hello.txt")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("from umbral-storage"),
        "expected file contents in body, got: {body:?}"
    );
}

#[tokio::test]
async fn missing_files_return_404_under_the_mount() {
    let dir = tempdir().unwrap();
    let plugin = StoragePlugin::new().static_files("/assets", dir.path());
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/assets/does-not-exist.css")
        .body(Body::empty())
        .unwrap();
    let (status, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn nested_subdirectories_resolve_correctly() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("css")).unwrap();
    fs::write(dir.path().join("css/site.css"), "body { color: red }").unwrap();

    let plugin = StoragePlugin::new().static_files("/static", dir.path());
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/css/site.css")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("color: red"));
}

#[tokio::test]
async fn plugin_name_is_storage() {
    let plugin = StoragePlugin::new().static_files("/static", "/tmp");
    assert_eq!(plugin.name(), "storage");
}

#[tokio::test]
async fn plugin_with_missing_dir_still_builds_router_and_returns_404() {
    let plugin = StoragePlugin::new().static_files("/static", "/this/does/not/exist");
    let router = plugin.routes();
    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/anything.txt")
        .body(Body::empty())
        .unwrap();
    let (status, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
