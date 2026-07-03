//! Symlink-escape guard on the media `ServeDir` (audit
//! `plugin-storage-tasks` #8): the media mount reuses the static side's
//! `SymlinkGuardService`, so a symlink inside the media dir pointing
//! outside it must 404 instead of leaking the target's contents.

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
async fn media_symlink_escaping_root_returns_404() {
    let root = tempdir().unwrap();

    fs::write(root.path().join("ok.txt"), "legitimate upload").unwrap();

    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "sensitive content").unwrap();

    let link_path = root.path().join("escape.txt");
    symlink(outside.path().join("secret.txt"), &link_path).unwrap();

    let plugin = StoragePlugin::new().media("/media", root.path());
    let router = plugin.routes();

    let req_ok = Request::builder()
        .method(Method::GET)
        .uri("/media/ok.txt")
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req_ok).await.unwrap();
    assert_eq!(
        resp.headers()
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff"),
        "media responses must keep the nosniff header through the guard"
    );
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "legitimate file must serve");
    assert_eq!(body, "legitimate upload");

    let req_escape = Request::builder()
        .method(Method::GET)
        .uri("/media/escape.txt")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_string(router.oneshot(req_escape).await.unwrap()).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "symlink escaping the media root must return 404, not the target's contents; body={body:?}"
    );
    assert!(
        !body.contains("sensitive content"),
        "symlink escape must not leak target content; body={body:?}"
    );
}

/// The guard must also bite when the media dir is created AFTER the router
/// is built (the common real-world order: routes are mounted at boot, the
/// dir appears on the first upload).
#[cfg(unix)]
#[tokio::test]
async fn media_symlink_guard_applies_to_dir_created_after_boot() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("media-not-yet");

    let plugin = StoragePlugin::new().media("/media", &root);
    let router = plugin.routes();

    // Dir appears after the router was built.
    fs::create_dir_all(&root).unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "sensitive content").unwrap();
    symlink(outside.path().join("secret.txt"), root.join("escape.txt")).unwrap();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/media/escape.txt")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "guard must apply to a media dir created after boot; body={body:?}"
    );
    assert!(!body.contains("sensitive content"));
}
