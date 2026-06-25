//! Tests for symlink-escape guard on filesystem-served static assets.
//! Moved from umbral-static.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;

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

#[cfg(unix)]
#[tokio::test]
async fn symlink_escaping_root_returns_404() {
    let root = tempdir().unwrap();

    fs::write(root.path().join("ok.txt"), "legitimate").unwrap();

    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "sensitive content").unwrap();

    let link_path = root.path().join("escape.txt");
    symlink(outside.path().join("secret.txt"), &link_path).unwrap();

    let plugin = StoragePlugin::new().static_files("/static", root.path());
    let router = plugin.routes();

    let req_ok = Request::builder()
        .method(Method::GET)
        .uri("/static/ok.txt")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_string(router.clone().oneshot(req_ok).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "legitimate file must serve");
    assert_eq!(body, "legitimate");

    let req_escape = Request::builder()
        .method(Method::GET)
        .uri("/static/escape.txt")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_string(router.oneshot(req_escape).await.unwrap()).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "symlink escaping root must return 404, not the target's contents; body={body:?}"
    );
    assert!(
        !body.contains("sensitive content"),
        "symlink escape must not leak target content; body={body:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_within_root_serves_normally() {
    let root = tempdir().unwrap();

    fs::write(root.path().join("real.txt"), "real content").unwrap();

    fs::create_dir(root.path().join("sub")).unwrap();
    symlink(
        root.path().join("real.txt"),
        root.path().join("sub").join("alias.txt"),
    )
    .unwrap();

    let plugin = StoragePlugin::new().static_files("/static", root.path());
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/sub/alias.txt")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "symlink within root must serve normally"
    );
    assert_eq!(body, "real content");
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_loop_returns_404_not_hang() {
    let root = tempdir().unwrap();

    let a = root.path().join("a.txt");
    let b = root.path().join("b.txt");
    symlink(&b, &a).unwrap();
    symlink(&a, &b).unwrap();

    let plugin = StoragePlugin::new().static_files("/static", root.path());
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/a.txt")
        .body(Body::empty())
        .unwrap();
    let (status, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "symlink loop must return 404");
}
