//! Tests for symlink-escape guard on filesystem-served static assets.
//!
//! A symlink inside the served root that points outside the root (e.g. to
//! `/etc/passwd`) must return 404, not the target's contents and not a hang.
//! A symlink that stays inside the root must serve normally.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;

use axum::body::Body;
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tempfile::tempdir;
use tower::ServiceExt;
use umbra::prelude::*;
use umbra_static::StaticPlugin;

async fn body_string(resp: http::Response<Body>) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A symlink inside the root that points to a file OUTSIDE the root
/// (e.g. /etc/passwd or a sibling tempdir) must return 404, not the
/// target's contents.
#[cfg(unix)]
#[tokio::test]
async fn symlink_escaping_root_returns_404() {
    let root = tempdir().unwrap();

    // Create a legitimate file so we know the plugin works.
    fs::write(root.path().join("ok.txt"), "legitimate").unwrap();

    // Create a sibling dir with a sensitive file.
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "sensitive content").unwrap();

    // Symlink inside root → outside.
    let link_path = root.path().join("escape.txt");
    symlink(outside.path().join("secret.txt"), &link_path).unwrap();

    let plugin = StaticPlugin::new("/static", root.path());
    let router = plugin.routes();

    // Legitimate file must still serve.
    let req_ok = Request::builder()
        .method(Method::GET)
        .uri("/static/ok.txt")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_string(router.clone().oneshot(req_ok).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "legitimate file must serve");
    assert_eq!(body, "legitimate");

    // Escaping symlink must return 404.
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

/// A symlink that points to a file INSIDE the root must serve normally.
#[cfg(unix)]
#[tokio::test]
async fn symlink_within_root_serves_normally() {
    let root = tempdir().unwrap();

    // Create a real file.
    fs::write(root.path().join("real.txt"), "real content").unwrap();

    // Create a subdirectory with a symlink back to the real file.
    fs::create_dir(root.path().join("sub")).unwrap();
    symlink(
        root.path().join("real.txt"),
        root.path().join("sub").join("alias.txt"),
    )
    .unwrap();

    let plugin = StaticPlugin::new("/static", root.path());
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

/// A symlink that forms a loop (target doesn't resolve) must return 404,
/// not hang indefinitely.
#[cfg(unix)]
#[tokio::test]
async fn symlink_loop_returns_404_not_hang() {
    let root = tempdir().unwrap();

    // Create a symlink loop: a → b → a.
    let a = root.path().join("a.txt");
    let b = root.path().join("b.txt");
    symlink(&b, &a).unwrap();
    symlink(&a, &b).unwrap();

    let plugin = StaticPlugin::new("/static", root.path());
    let router = plugin.routes();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/static/a.txt")
        .body(Body::empty())
        .unwrap();
    // Must complete (not hang) and return 404.
    let (status, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "symlink loop must return 404"
    );
}
