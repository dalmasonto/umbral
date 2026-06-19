//! Security regression tests for cache-key isolation in `cache_page`.
//!
//! These tests verify two cache-poisoning vectors are closed:
//!
//! 1. **Host isolation** — two requests for the same path that differ only in
//!    their `Host` header must NOT share a cached body.  Without this a
//!    multi-tenant app would serve tenant A's HTML to tenant B.
//!
//! 2. **Session-cookie bypass** — a request carrying an `umbra_session` cookie
//!    must be passed straight through to the handler (never served from cache
//!    and never written to cache).  Without this a logged-in user's response
//!    could poison the anonymous cache, or an anonymous entry could be served
//!    to a logged-in user.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::get;
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbra_cache::{Cache, cache_page::cache_page};

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn body_string(resp: axum::http::Response<Body>) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Build a router whose single handler increments `counter` and returns the
/// current count in the body.  The `cache_page` layer is injected with an
/// explicit in-memory cache so the ambient OnceLock is not required.
fn counting_router(counter: Arc<AtomicU32>, cache: Cache) -> Router {
    let c = counter.clone();
    Router::new()
        .route(
            "/page",
            get(move || {
                let cc = c.clone();
                async move {
                    let n = cc.fetch_add(1, Ordering::SeqCst) + 1;
                    format!("hit #{n}")
                }
            }),
        )
        .layer(cache_page(Duration::from_secs(60)).with_cache(cache))
}

fn get_with_host(uri: &str, host: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Host", host)
        .body(Body::empty())
        .unwrap()
}

fn get_with_session_cookie(uri: &str, session_value: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Cookie", format!("umbra_session={session_value}"))
        .body(Body::empty())
        .unwrap()
}

fn get_with_cookie_and_host(uri: &str, cookie_header: &str, host: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Host", host)
        .header("Cookie", cookie_header)
        .body(Body::empty())
        .unwrap()
}

// ── Host isolation ────────────────────────────────────────────────────────────

/// A request for `/page` on `tenant-a.example.com` caches a response. A
/// subsequent request for the same path on `tenant-b.example.com` must NOT
/// receive that cached body — the handler must fire again.
#[tokio::test]
async fn different_hosts_do_not_share_cache_entries() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let router = counting_router(counter.clone(), cache);

    // First request — tenant A.  This should be a cache miss, so the handler
    // fires once and caches the response under the tenant-A key.
    let (s1, b1) = body_string(
        router
            .clone()
            .oneshot(get_with_host("/page", "tenant-a.example.com"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(b1, "hit #1");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "handler fired once (tenant A)"
    );

    // Second request — different host.  Must NOT be served from the tenant-A
    // cache entry; handler fires a second time.
    let (s2, b2) = body_string(
        router
            .oneshot(get_with_host("/page", "tenant-b.example.com"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(
        b2, "hit #2",
        "tenant-B request must NOT get tenant-A's cached body"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "handler must fire again for a different Host"
    );
}

/// Same-host requests DO share a cache entry (sanity check that the Host-key
/// change didn't break the ordinary same-host hit path).
#[tokio::test]
async fn same_host_requests_still_share_cache() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let router = counting_router(counter.clone(), cache);

    let (_, b1) = body_string(
        router
            .clone()
            .oneshot(get_with_host("/page", "www.example.com"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(b1, "hit #1");

    let (_, b2) = body_string(
        router
            .oneshot(get_with_host("/page", "www.example.com"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        b2, "hit #1",
        "same-host second request must come from cache"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "handler must NOT fire for same-host second request"
    );
}

// ── Session-cookie bypass ─────────────────────────────────────────────────────

/// A request that carries `umbra_session=<value>` bypasses the page cache
/// entirely — the handler fires on every such request regardless of whether an
/// anonymous cached entry exists.
#[tokio::test]
async fn session_cookie_request_is_not_served_from_cache() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let router = counting_router(counter.clone(), cache);

    // Populate the anonymous cache entry first.
    let req_anon = Request::builder()
        .method(Method::GET)
        .uri("/page")
        .body(Body::empty())
        .unwrap();
    let (_, b0) = body_string(router.clone().oneshot(req_anon).await.unwrap()).await;
    assert_eq!(b0, "hit #1", "anonymous request populates cache");

    // Now send a request bearing an umbra_session cookie.  Even though an
    // anonymous entry exists for the same path, the middleware must bypass
    // the cache and call the handler.
    let (s1, b1) = body_string(
        router
            .clone()
            .oneshot(get_with_session_cookie("/page", "abc123"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(
        b1, "hit #2",
        "session-cookie request must NOT receive the anonymous cached body"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "handler must fire for session-cookie request"
    );
}

/// A response served to a session-cookie-bearing request is not written to
/// the cache.  A subsequent anonymous request must still hit the handler (or
/// see the previously-anonymous cached entry, but must NOT see the
/// session-scoped response).
#[tokio::test]
async fn session_cookie_response_is_not_written_to_cache() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let router = counting_router(counter.clone(), cache);

    // First request — authenticated (session cookie present).  This MUST NOT
    // be stored in the cache.
    let (s1, b1) = body_string(
        router
            .clone()
            .oneshot(get_with_session_cookie("/page", "user-session-xyz"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(b1, "hit #1");

    // Second request — anonymous (no cookie).  If the first response had been
    // written to the cache the handler would NOT fire a second time and we'd
    // see "hit #1" again.  The handler must fire.
    let req_anon = Request::builder()
        .method(Method::GET)
        .uri("/page")
        .body(Body::empty())
        .unwrap();
    let (s2, b2) = body_string(router.clone().oneshot(req_anon).await.unwrap()).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(
        b2, "hit #2",
        "anonymous request after a session-cookie request must hit the handler, \
         not receive the session response"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);

    // Third anonymous request: now there IS an anonymous cache entry from
    // request #2 — it should be served from cache.
    let req_anon2 = Request::builder()
        .method(Method::GET)
        .uri("/page")
        .body(Body::empty())
        .unwrap();
    let (_, b3) = body_string(router.oneshot(req_anon2).await.unwrap()).await;
    assert_eq!(
        b3, "hit #2",
        "third anonymous request must be served from cache (the anonymous entry)"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "handler must NOT fire again"
    );
}

/// A session cookie among multiple cookies in the Cookie header is still
/// detected correctly (e.g. `Cookie: pref=dark; umbra_session=tok; lang=en`).
#[tokio::test]
async fn session_cookie_detected_among_multiple_cookies() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let router = counting_router(counter.clone(), cache);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/page")
        .header("Cookie", "pref=dark; umbra_session=tok42; lang=en")
        .body(Body::empty())
        .unwrap();

    let (s, _) = body_string(router.clone().oneshot(req).await.unwrap()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(counter.load(Ordering::SeqCst), 1, "handler must fire");

    // Second identical request — still bypasses cache because it carries a session cookie.
    let req2 = Request::builder()
        .method(Method::GET)
        .uri("/page")
        .header("Cookie", "pref=dark; umbra_session=tok42; lang=en")
        .body(Body::empty())
        .unwrap();
    let (_, b2) = body_string(router.oneshot(req2).await.unwrap()).await;
    assert_eq!(b2, "hit #2", "still bypasses cache on repeat");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

/// A cookie whose name merely contains `umbra_session` as a substring but is
/// not exactly `umbra_session=` must NOT trigger the bypass.
#[tokio::test]
async fn non_session_cookie_does_not_bypass_cache() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let router = counting_router(counter.clone(), cache);

    // `not_umbra_session=abc` is a different cookie; must not bypass.
    let req1 = Request::builder()
        .method(Method::GET)
        .uri("/page")
        .header("Cookie", "not_umbra_session=abc")
        .body(Body::empty())
        .unwrap();
    let (_, b1) = body_string(router.clone().oneshot(req1).await.unwrap()).await;
    assert_eq!(b1, "hit #1");

    // Repeat with the same cookie — should come from cache.
    let req2 = Request::builder()
        .method(Method::GET)
        .uri("/page")
        .header("Cookie", "not_umbra_session=abc")
        .body(Body::empty())
        .unwrap();
    let (_, b2) = body_string(router.oneshot(req2).await.unwrap()).await;
    assert_eq!(b2, "hit #1", "non-session cookie must not disable caching");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

/// Regression for the combined case: different Host AND session cookie together.
/// The session-cookie bypass wins; the handler fires even if an anonymous
/// same-host entry already exists.
#[tokio::test]
async fn session_cookie_bypass_independent_of_host() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let router = counting_router(counter.clone(), cache);

    // Populate an anonymous entry for host A.
    let (_, b1) = body_string(
        router
            .clone()
            .oneshot(get_with_host("/page", "a.example.com"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(b1, "hit #1");

    // Authenticated request on the same host — must bypass cache.
    let (_, b2) = body_string(
        router
            .clone()
            .oneshot(get_with_cookie_and_host(
                "/page",
                "umbra_session=logged-in",
                "a.example.com",
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        b2, "hit #2",
        "session-cookie bypass wins over same-host cache hit"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);

    // Anonymous request on same host afterwards — still served from the
    // anonymous cache entry (the authenticated response was not stored).
    let (_, b3) = body_string(
        router
            .oneshot(get_with_host("/page", "a.example.com"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        b3, "hit #1",
        "anonymous cache entry for host A is still intact"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "handler must not fire again"
    );
}
