//! Task 10: end-to-end enforcement for `media_access_cached` through the
//! real media GET route (not `resolve_access()` in isolation, like
//! `media_access_cached.rs` — this drives actual `oneshot` requests). The
//! caching lives INSIDE the `MediaAccessFn` the builder returns, so this
//! exercises `media_gate` unchanged: a denied caller never sees bytes, an
//! allowed caller does, and a repeat (caller, key) is served from the
//! ambient cache without re-running the closure.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use tower::ServiceExt;
use umbral::auth::{FnAuthentication, Identity, set_default_authentication};
use umbral::prelude::Plugin;
use umbral_storage::{Decision, MediaCaller, StoragePlugin};

/// Installed once per test binary (OnceLock); reads caller shape off
/// `x-user`/`x-staff`/`x-superuser` headers, same convention as
/// `media_role_presets.rs`.
fn install_stub_auth() {
    set_default_authentication(Arc::new(FnAuthentication::new(
        |headers: HeaderMap| async move {
            let uid = headers.get("x-user")?.to_str().ok()?.to_string();
            let is_staff = headers.get("x-staff").is_some();
            let is_superuser = headers.get("x-superuser").is_some();
            Some(
                Identity::user(uid)
                    .with_staff(is_staff)
                    .with_superuser(is_superuser),
            )
        },
    )));
    umbral::cache::set_ambient_tagged_cache(Arc::new(umbral_cache::Cache::memory()));
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

/// The headline path: denied → 403 with no bytes, allowed → 200 with the
/// bytes, and a second identical (caller, key) request is served from the
/// ambient cache — the closure runs exactly once, proven with a counter,
/// exactly as `media_access_cached.rs` proves it at the `resolve_access()`
/// level, but here through the real FS-backed GET route.
#[tokio::test]
async fn fs_guard_denies_allows_and_caches_across_the_real_route() {
    install_stub_auth();
    let dir = tempfile::tempdir().expect("tmp dir");
    fs::write(dir.path().join("invoice.pdf"), b"PRIVATE-PDF").unwrap();

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

    // A different caller than the one the closure allows → 403, no bytes.
    let denied = app
        .clone()
        .oneshot(get_as("/media/invoice.pdf", Some("stranger")))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let body = body_of(denied).await;
    assert!(
        !body.windows(11).any(|w| w == b"PRIVATE-PDF"),
        "denied response must not leak the file"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "closure ran for the miss");

    // The owner → 200 + bytes; closure runs again (different cache key: the
    // caller id is part of the cache key).
    let allowed = app
        .clone()
        .oneshot(get_as("/media/invoice.pdf", Some("owner")))
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(body_of(allowed).await, b"PRIVATE-PDF");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Same owner, same key again → served from cache, closure does NOT run.
    let allowed_again = app
        .clone()
        .oneshot(get_as("/media/invoice.pdf", Some("owner")))
        .await
        .unwrap();
    assert_eq!(allowed_again.status(), StatusCode::OK);
    assert_eq!(body_of(allowed_again).await, b"PRIVATE-PDF");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "a repeat (caller, key) request must be served from cache, not re-run the closure"
    );

    // Same for the stranger: the denial is cached too.
    let denied_again = app
        .oneshot(get_as("/media/invoice.pdf", Some("stranger")))
        .await
        .unwrap();
    assert_eq!(denied_again.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "a cached denial must also skip the closure"
    );
}

/// `Decision::allow()` for a superuser and `Decision::deny()` for an
/// anonymous caller both behave correctly through the real route, and both
/// get cached (the counter proves a repeat request skips the closure).
#[tokio::test]
async fn superuser_allow_and_anonymous_deny_through_the_real_route() {
    install_stub_auth();
    let dir = tempfile::tempdir().expect("tmp dir");
    fs::write(dir.path().join("admin-only.txt"), b"ADMIN-BYTES").unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let app = StoragePlugin::new()
        .media("/media", dir.path())
        .media_access_cached(move |caller: MediaCaller, _key: &str| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                if caller.is_superuser {
                    Decision::allow()
                } else if caller.user_id().is_none() {
                    Decision::deny()
                } else {
                    Decision::of(false)
                }
            }
        })
        .routes();

    // Superuser (any x-user, plus x-superuser) → allowed and served.
    let mut req = Request::builder().uri("/media/admin-only.txt");
    req = req.header("x-user", "9").header("x-superuser", "1");
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "Decision::allow() serves");
    assert_eq!(body_of(resp).await, b"ADMIN-BYTES");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Anonymous caller (no x-user at all) → denied, no bytes.
    let resp = app
        .clone()
        .oneshot(get_as("/media/admin-only.txt", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "Decision::deny() 403s");
    let body = body_of(resp).await;
    assert!(!body.windows(11).any(|w| w == b"ADMIN-BYTES"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Repeat both — cached, no further closure runs.
    let mut req = Request::builder().uri("/media/admin-only.txt");
    req = req.header("x-user", "9").header("x-superuser", "1");
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(get_as("/media/admin-only.txt", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "repeat superuser-allow and anon-deny must both be served from cache"
    );
}
