//! Integration tests for the `cache_page` tower layer.
//!
//! Each test builds a small axum Router, wraps a subtree with
//! `cache_page(ttl).with_cache(cache)` (explicit injection so the
//! ambient OnceLock doesn't need initialising), and drives requests
//! through `tower::ServiceExt::oneshot`.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode};
use axum::routing::get;
use http::HeaderValue;
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbra_cache::{Cache, cache_page::cache_page};

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn body_string(resp: Response<Body>) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn build_counting_router(counter: Arc<AtomicU32>, cache: Cache) -> Router {
    let counter_clone = counter.clone();
    Router::new()
        .route(
            "/page",
            get(move || {
                let c = counter_clone.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                    format!("hit #{n}")
                }
            }),
        )
        .layer(cache_page(Duration::from_secs(60)).with_cache(cache))
}

fn make_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn make_post(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Second GET to the same URI returns the cached response without calling
/// the handler a second time.
#[tokio::test]
async fn second_get_returns_cached_response() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let router = build_counting_router(counter.clone(), cache);

    let (s1, b1) = body_string(router.clone().oneshot(make_get("/page")).await.unwrap()).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(b1, "hit #1");
    assert_eq!(counter.load(Ordering::SeqCst), 1, "handler fired once");

    let (s2, b2) = body_string(router.oneshot(make_get("/page")).await.unwrap()).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2, "hit #1", "cached body, not a new hit");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "handler must NOT fire again"
    );
}

/// POST requests bypass the cache — the handler fires every time and
/// POST responses are never stored.
#[tokio::test]
async fn post_bypasses_cache() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let router = Router::new()
        .route(
            "/page",
            axum::routing::post(move || {
                let c = counter.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                    format!("post #{n}")
                }
            }),
        )
        .layer(cache_page(Duration::from_secs(60)).with_cache(cache));

    let (s1, b1) = body_string(router.clone().oneshot(make_post("/page")).await.unwrap()).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(b1, "post #1");

    let (s2, b2) = body_string(router.oneshot(make_post("/page")).await.unwrap()).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2, "post #2", "POST must not be served from cache");
}

/// A response with `Cache-Control: no-store` is never cached — the second
/// GET still calls the handler.
#[tokio::test]
async fn no_store_bypasses_cache() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let counter_clone = counter.clone();

    let router = Router::new()
        .route(
            "/ns",
            get(move || {
                let c = counter_clone.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                    let body = format!("no-store hit #{n}");
                    Response::builder()
                        .header("Cache-Control", "no-store")
                        .body(Body::from(body))
                        .unwrap()
                }
            }),
        )
        .layer(cache_page(Duration::from_secs(60)).with_cache(cache));

    let (_, b1) = body_string(router.clone().oneshot(make_get("/ns")).await.unwrap()).await;
    assert_eq!(b1, "no-store hit #1");

    let (_, b2) = body_string(router.oneshot(make_get("/ns")).await.unwrap()).await;
    assert_eq!(b2, "no-store hit #2", "no-store must prevent caching");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

/// A response with `Set-Cookie` is never cached.
#[tokio::test]
async fn set_cookie_bypasses_cache() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let counter_clone = counter.clone();

    let router = Router::new()
        .route(
            "/sc",
            get(move || {
                let c = counter_clone.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                    let body = format!("cookie hit #{n}");
                    Response::builder()
                        .header("Set-Cookie", "session=abc; HttpOnly")
                        .body(Body::from(body))
                        .unwrap()
                }
            }),
        )
        .layer(cache_page(Duration::from_secs(60)).with_cache(cache));

    let (_, b1) = body_string(router.clone().oneshot(make_get("/sc")).await.unwrap()).await;
    assert_eq!(b1, "cookie hit #1");

    let (_, b2) = body_string(router.oneshot(make_get("/sc")).await.unwrap()).await;
    assert_eq!(b2, "cookie hit #2", "Set-Cookie must prevent caching");
}

/// Different query strings produce different cache keys.
#[tokio::test]
async fn different_query_strings_are_separate_cache_entries() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let counter_clone = counter.clone();

    let router = Router::new()
        .route(
            "/q",
            get(move || {
                let c = counter_clone.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                    format!("q hit #{n}")
                }
            }),
        )
        .layer(cache_page(Duration::from_secs(60)).with_cache(cache));

    let (_, b1) = body_string(router.clone().oneshot(make_get("/q?page=1")).await.unwrap()).await;
    assert_eq!(b1, "q hit #1");

    let (_, b2) = body_string(router.clone().oneshot(make_get("/q?page=2")).await.unwrap()).await;
    assert_eq!(b2, "q hit #2", "different query = different cache entry");

    // Repeat page=1 — should come from cache
    let (_, b3) = body_string(router.oneshot(make_get("/q?page=1")).await.unwrap()).await;
    assert_eq!(b3, "q hit #1", "page=1 should be served from cache");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "only two handler calls total"
    );
}

/// Non-200 responses are not cached. A 404 must call the handler each time.
#[tokio::test]
async fn non_200_responses_are_not_cached() {
    let counter = Arc::new(AtomicU32::new(0));
    let cache = Cache::memory();
    let counter_clone = counter.clone();

    let router = Router::new()
        .route(
            "/missing",
            get(move || {
                let c = counter_clone.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::NOT_FOUND, "not found")
                }
            }),
        )
        .layer(cache_page(Duration::from_secs(60)).with_cache(cache));

    let (s1, _) = body_string(router.clone().oneshot(make_get("/missing")).await.unwrap()).await;
    assert_eq!(s1, StatusCode::NOT_FOUND);

    let (s2, _) = body_string(router.oneshot(make_get("/missing")).await.unwrap()).await;
    assert_eq!(s2, StatusCode::NOT_FOUND);
    assert_eq!(counter.load(Ordering::SeqCst), 2, "404 must not be cached");
}

/// `with_cache` override is exercised by every test above.  This test
/// confirms the explicit-injection path preserves response headers.
#[tokio::test]
async fn cached_response_preserves_content_type_header() {
    let cache = Cache::memory();

    let router = Router::new()
        .route(
            "/json",
            get(|| async {
                Response::builder()
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"ok":true}"#))
                    .unwrap()
            }),
        )
        .layer(cache_page(Duration::from_secs(60)).with_cache(cache));

    let resp1 = router.clone().oneshot(make_get("/json")).await.unwrap();
    assert_eq!(
        resp1.headers().get("Content-Type"),
        Some(&HeaderValue::from_static("application/json"))
    );

    // Second request from cache
    let resp2 = router.oneshot(make_get("/json")).await.unwrap();
    assert_eq!(
        resp2.headers().get("Content-Type"),
        Some(&HeaderValue::from_static("application/json")),
        "Content-Type must survive round-trip through cache"
    );
}
