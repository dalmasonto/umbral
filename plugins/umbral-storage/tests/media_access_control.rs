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

/// The callback must receive the PERCENT-DECODED key — the same string
/// `FileField` stores and `ServeDir` resolves on disk. Handing it the raw
/// encoded path (`with%20spaces.txt`) makes every spaced/escaped key
/// unmatchable in an ownership lookup, so real uploads ("Screenshot from
/// 2026-07-08.png") get denied as unknown files while unspaced ones pass —
/// found live in TaskFlow the first day the gate shipped.
#[tokio::test]
async fn media_access_gate_receives_the_decoded_key() {
    let dir = tempfile::tempdir().expect("tmp dir");
    fs::write(dir.path().join("with spaces.txt"), b"SPACED").unwrap();

    // Allow ONLY the exact decoded key — an encoded key reaching the callback
    // fails this equality, exactly like a DB lookup by stored key would.
    let app = StoragePlugin::new()
        .media("/media", dir.path())
        .media_access(|_headers: axum::http::HeaderMap, key: String| async move {
            key == "with spaces.txt"
        })
        .routes();

    let res = app
        .clone()
        .oneshot(get("/media/with%20spaces.txt", false))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "the gate must see the decoded key (`with spaces.txt`), not the raw path"
    );
    let body = axum::body::to_bytes(res.into_body(), 1 << 16)
        .await
        .unwrap();
    assert_eq!(
        &body[..],
        b"SPACED",
        "and the file serves after the gate allows"
    );

    // A key that percent-decodes to something DIFFERENT from any stored key
    // still denies: decoding must not open new paths through the gate.
    let res = app
        .clone()
        .oneshot(get("/media/with%20spaces%2Etxt.evil", false))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}
