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

// ── gaps4 sweep: the identity-aware gate + coverage the audit flagged ──

use std::sync::Arc;

/// Install an ambient auth backend that reads `x-user` as an i64 identity.
/// Process-global (OnceLock), so ONLY this test installs it — the sibling
/// header-based tests don't use identity gating and are unaffected.
fn install_x_user_auth() {
    umbral::auth::set_default_authentication(Arc::new(umbral::auth::FnAuthentication::new(
        |headers: axum::http::HeaderMap| async move {
            let uid: i64 = headers.get("x-user")?.to_str().ok()?.parse().ok()?;
            Some(umbral::auth::Identity::user(uid))
        },
    )));
}

fn get_as(uri: &str, user: Option<i64>) -> Request<Body> {
    let mut b = Request::builder().uri(uri);
    if let Some(u) = user {
        b = b.header("x-user", u.to_string());
    }
    b.body(Body::empty()).unwrap()
}

/// The headline auth check: `media_access_identity` resolves the caller
/// through the app-wide auth backend (gaps4 #42), so "signed-in users
/// only" is a one-liner with no manual cookie parsing.
#[tokio::test]
async fn media_access_identity_gates_on_the_resolved_caller() {
    install_x_user_auth();
    let dir = tempfile::tempdir().expect("tmp dir");
    fs::write(dir.path().join("private.txt"), b"OWNER-ONLY").unwrap();

    let app = StoragePlugin::new()
        .media("/media", dir.path())
        // Authenticated users only — the closure sees Option<Identity>,
        // never raw headers.
        .media_access_identity(|caller, _key| async move { caller.is_some() })
        .routes();

    let anon = app
        .clone()
        .oneshot(get_as("/media/private.txt", None))
        .await
        .unwrap();
    assert_eq!(
        anon.status(),
        StatusCode::FORBIDDEN,
        "an anonymous caller is denied — the ambient backend resolved None"
    );

    let signed_in = app
        .clone()
        .oneshot(get_as("/media/private.txt", Some(7)))
        .await
        .unwrap();
    assert_eq!(
        signed_in.status(),
        StatusCode::OK,
        "an authenticated caller (x-user: 7 → Identity) is served"
    );
    let body = axum::body::to_bytes(signed_in.into_body(), 1 << 16)
        .await
        .unwrap();
    assert_eq!(&body[..], b"OWNER-ONLY");
}

/// The gate runs on a Range request too — a partial fetch is not a bypass
/// (the audit flagged this as unpinned). Denied → 403 with no bytes.
#[tokio::test]
async fn media_access_gate_covers_range_requests() {
    let dir = tempfile::tempdir().expect("tmp dir");
    fs::write(dir.path().join("clip.bin"), b"0123456789").unwrap();

    let app = StoragePlugin::new()
        .media("/media", dir.path())
        .media_access(|headers: axum::http::HeaderMap, _key: String| async move {
            headers.contains_key("x-allow")
        })
        .routes();

    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/media/clip.bin")
                .header("range", "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "a Range request must pass the gate first — no partial-content bypass"
    );
    let body = axum::body::to_bytes(denied.into_body(), 1 << 16)
        .await
        .unwrap();
    assert!(
        !body.windows(4).any(|w| w == b"0123"),
        "no bytes leak through a denied Range request"
    );

    // With the credential, the Range is honoured (206) behind the gate.
    let allowed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/media/clip.bin")
                .header("range", "bytes=0-3")
                .header("x-allow", "yes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        allowed.status(),
        StatusCode::PARTIAL_CONTENT,
        "an allowed Range request serves 206 Partial Content"
    );
}

/// A subdirectory key (`/media/a/b/c.txt` → key `a/b/c.txt`) reaches the
/// gate as the full nested path — an ownership lookup needs the whole key.
#[tokio::test]
async fn media_access_gate_sees_subdirectory_keys() {
    let dir = tempfile::tempdir().expect("tmp dir");
    fs::create_dir_all(dir.path().join("2026/receipts")).unwrap();
    fs::write(dir.path().join("2026/receipts/r1.txt"), b"RECEIPT").unwrap();

    let seen: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let seen_c = seen.clone();
    let app = StoragePlugin::new()
        .media("/media", dir.path())
        .media_access(move |_headers: axum::http::HeaderMap, key: String| {
            let seen = seen_c.clone();
            async move {
                *seen.lock().unwrap() = Some(key);
                true
            }
        })
        .routes();

    let resp = app
        .oneshot(get("/media/2026/receipts/r1.txt", false))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        seen.lock().unwrap().as_deref(),
        Some("2026/receipts/r1.txt"),
        "the gate must see the full nested key, not just the leaf"
    );
}
