//! audit_2 plugin-storage-tasks #3 — `StoragePlugin::media_access(..)` gates the
//! media GET route: without the gate every uploaded file is world-readable by
//! URL (an IDOR for private uploads); with it, a request is served only when the
//! callback returns `true`.

use std::fs;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbral::prelude::Plugin;
use umbral_storage::StoragePlugin;

fn get(uri: &str, allow_header: bool) -> Request<Body> {
    let mut b = Request::builder().uri(uri);
    if allow_header {
        b = b.header("x-allow", "yes");
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn media_access_gate_allows_and_denies() {
    let dir = tempfile::tempdir().expect("tmp dir");
    fs::write(dir.path().join("secret.txt"), b"TOPSECRET").unwrap();

    // Gate: serve only when the request carries `x-allow` (a stand-in for a real
    // session/ownership check).
    let app = StoragePlugin::new()
        .media("/media", dir.path())
        .media_access(|headers: axum::http::HeaderMap, _key: String| async move {
            headers.contains_key("x-allow")
        })
        .routes();

    // No credential → 403, and the bytes never leave the server.
    let denied = app
        .clone()
        .oneshot(get("/media/secret.txt", false))
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "an ungated request must be refused (403), not served"
    );
    let body = axum::body::to_bytes(denied.into_body(), 1 << 16)
        .await
        .unwrap();
    assert!(
        !body.windows(9).any(|w| w == b"TOPSECRET"),
        "the file contents must NOT be in a denied response"
    );

    // With the credential → 200 + the file.
    let allowed = app
        .clone()
        .oneshot(get("/media/secret.txt", true))
        .await
        .unwrap();
    assert_eq!(
        allowed.status(),
        StatusCode::OK,
        "an allowed request must serve the file"
    );
    let body = axum::body::to_bytes(allowed.into_body(), 1 << 16)
        .await
        .unwrap();
    assert_eq!(
        &body[..],
        b"TOPSECRET",
        "the allowed response serves the bytes"
    );
}

#[tokio::test]
async fn without_a_gate_serving_is_unchanged() {
    let dir = tempfile::tempdir().expect("tmp dir");
    fs::write(dir.path().join("public.txt"), b"HELLO").unwrap();

    // No `.media_access(..)` → backward-compatible: served to anyone.
    let app = StoragePlugin::new().media("/media", dir.path()).routes();
    let res = app.oneshot(get("/media/public.txt", false)).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "with no gate configured, media serves as before"
    );
}
