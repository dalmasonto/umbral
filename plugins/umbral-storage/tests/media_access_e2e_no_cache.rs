//! Task 10 (continued): `media_access_cached` with NO ambient `TaggedCache`
//! installed. This lives in its own test binary — `umbral::cache`'s ambient
//! cache is a process-wide `OnceLock` (see `crates/umbral-core/src/cache.rs`),
//! so a shared binary with `media_access_e2e.rs` would leak the cache it
//! installs. Here the `OnceLock` is genuinely unset: the fallback path in
//! `media_access_cached` (`None => f(caller, &key).await.is_allow()`) runs
//! on every single request — enforcement must still be correct, just
//! uncached.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use tower::ServiceExt;
use umbral::auth::{FnAuthentication, Identity, set_default_authentication};
use umbral::prelude::Plugin;
use umbral_storage::{Decision, MediaCaller, StoragePlugin};

fn install_stub_auth() {
    set_default_authentication(Arc::new(FnAuthentication::new(
        |headers: HeaderMap| async move {
            let uid = headers.get("x-user")?.to_str().ok()?.to_string();
            Some(Identity::user(uid))
        },
    )));
    // Deliberately NOT calling umbral::cache::set_ambient_tagged_cache here.
}

fn get_as(uri: &str, user: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().uri(uri);
    if let Some(u) = user {
        b = b.header("x-user", u);
    }
    b.body(Body::empty()).unwrap()
}

async fn body_of(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), 1 << 16)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn no_ambient_cache_still_enforces_and_reruns_the_closure_every_time() {
    install_stub_auth();
    debug_assert!(
        umbral::cache::ambient_tagged_cache().is_none(),
        "this test binary must not have an ambient cache installed"
    );

    let dir = tempfile::tempdir().expect("tmp dir");
    fs::write(dir.path().join("secret.txt"), b"NO-CACHE-SECRET").unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let app = StoragePlugin::new()
        .media("/media", dir.path())
        .media_access_cached(move |caller: MediaCaller, _key: &str| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Decision::of(caller.user_id() == Some("owner"))
            }
        })
        .routes();

    // Denied caller → 403, no bytes, closure ran.
    let denied = app
        .clone()
        .oneshot(get_as("/media/secret.txt", Some("stranger")))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(
        !body_of(denied)
            .await
            .windows(16)
            .any(|w| w == b"NO-CACHE-SECRET"),
        "no bytes leak on denial"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Same denied caller AGAIN → still 403, and the closure ran a second
    // time — with no cache, nothing short-circuits it.
    let denied_again = app
        .clone()
        .oneshot(get_as("/media/secret.txt", Some("stranger")))
        .await
        .unwrap();
    assert_eq!(denied_again.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "with no ambient cache, the closure must re-run on every request"
    );

    // Allowed caller → 200 + bytes, closure ran a third time.
    let allowed = app
        .clone()
        .oneshot(get_as("/media/secret.txt", Some("owner")))
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(body_of(allowed).await, b"NO-CACHE-SECRET");
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    // Same allowed caller again → still served, closure runs a fourth time.
    let allowed_again = app
        .oneshot(get_as("/media/secret.txt", Some("owner")))
        .await
        .unwrap();
    assert_eq!(allowed_again.status(), StatusCode::OK);
    assert_eq!(body_of(allowed_again).await, b"NO-CACHE-SECRET");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "an allowed repeat request must also re-run the closure with no cache"
    );
}
