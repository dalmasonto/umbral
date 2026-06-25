//! Integration tests for the opt-in HTTP layers on `CachePlugin`:
//!   - `with_compression()` → `CompressionLayer` (gzip/br/… negotiation)
//!   - `cache_control("…")` → `Cache-Control` response header
//!   - `vary("…")` → `Vary` response header
//!
//! Each test mounts a minimal axum router, applies the layer(s) under test,
//! and drives a single request via `tower::ServiceExt::oneshot`.
//!
//! Default (no opt-in) tests confirm the behavior is unchanged when the
//! builder methods are not called.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::get;
use flate2::read::GzDecoder;
use http_body_util::BodyExt;
use std::io::Read;
use tower::ServiceExt;
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn make_get_with_accept_encoding(uri: &str, encoding: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Accept-Encoding", encoding)
        .body(Body::empty())
        .unwrap()
}

async fn collect_body(resp: axum::response::Response) -> (StatusCode, Vec<u8>) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, bytes)
}

/// A router with a single `/hello` GET route that returns a plaintext body.
/// Mirrored across every test so the layer under test is the only variable.
///
/// The body is intentionally ≥ 32 bytes so `CompressionLayer`'s default
/// `SizeAbove(32)` predicate doesn't suppress compression for the small-body
/// case. The predicate skips compression when `Content-Length` is known and
/// below the threshold.
fn base_router() -> Router {
    Router::new().route(
        "/hello",
        get(|| async {
            "Hello, umbral! This response is long enough for the compression predicate."
        }),
    )
}

// ── compression ───────────────────────────────────────────────────────────────

/// With `CompressionLayer` applied + `Accept-Encoding: gzip`, the response
/// carries `Content-Encoding: gzip` and the decompressed body is the original.
#[tokio::test]
async fn compression_layer_gzip_encodes_response() {
    let router = base_router().layer(CompressionLayer::new());

    let req = make_get_with_accept_encoding("/hello", "gzip");
    let resp = router.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let content_encoding = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        content_encoding.as_deref(),
        Some("gzip"),
        "CompressionLayer + Accept-Encoding: gzip must produce Content-Encoding: gzip"
    );

    let (_, body_bytes) = collect_body(resp).await;
    let mut decoder = GzDecoder::new(body_bytes.as_slice());
    let mut decoded = String::new();
    decoder.read_to_string(&mut decoded).unwrap();
    assert!(
        decoded.starts_with("Hello, umbral!"),
        "decoded body must start with the expected content; got: {decoded:?}"
    );
}

/// Without `Accept-Encoding`, no `Content-Encoding` header is added even with
/// `CompressionLayer` present — the layer respects content negotiation.
#[tokio::test]
async fn compression_layer_no_accept_encoding_no_compression() {
    let router = base_router().layer(CompressionLayer::new());

    let req = make_get("/hello");
    let resp = router.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("content-encoding").is_none(),
        "no Accept-Encoding on request → no Content-Encoding on response"
    );

    let (_, body_bytes) = collect_body(resp).await;
    assert!(
        String::from_utf8(body_bytes)
            .unwrap()
            .starts_with("Hello, umbral!")
    );
}

/// **Default (no compression opted in)**: no `Content-Encoding` header even
/// with an `Accept-Encoding: gzip` request.
#[tokio::test]
async fn default_no_compression_header() {
    // No CompressionLayer applied.
    let router = base_router();

    let req = make_get_with_accept_encoding("/hello", "gzip");
    let resp = router.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("content-encoding").is_none(),
        "without with_compression(), Content-Encoding must not be added"
    );
}

// ── cache-control header ──────────────────────────────────────────────────────

/// With `SetResponseHeaderLayer::overriding(CACHE_CONTROL, …)` applied, the
/// response carries the configured `Cache-Control` value.
#[tokio::test]
async fn cache_control_header_is_set() {
    use http::header::CACHE_CONTROL;
    use http::HeaderValue;

    let cc_value = "public, max-age=60";
    let router = base_router().layer(SetResponseHeaderLayer::overriding(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    ));

    let resp = router.oneshot(make_get("/hello")).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let actual = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        actual,
        Some(cc_value),
        "cache_control() must emit the configured Cache-Control header"
    );
}

/// **Default (no cache_control opted in)**: no `Cache-Control` header on plain
/// text response.
#[tokio::test]
async fn default_no_cache_control_header() {
    // No SetResponseHeaderLayer applied.
    let router = base_router();

    let resp = router.oneshot(make_get("/hello")).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("cache-control").is_none(),
        "without cache_control(), Cache-Control must not be added"
    );
}

// ── vary header ───────────────────────────────────────────────────────────────

/// With `SetResponseHeaderLayer::overriding(VARY, …)` applied, the response
/// carries the configured `Vary` value.
#[tokio::test]
async fn vary_header_is_set() {
    use http::header::VARY;
    use http::HeaderValue;

    let router = base_router().layer(SetResponseHeaderLayer::overriding(
        VARY,
        HeaderValue::from_static("Accept-Encoding"),
    ));

    let resp = router.oneshot(make_get("/hello")).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let actual = resp
        .headers()
        .get("vary")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        actual,
        Some("Accept-Encoding"),
        "vary() must emit the configured Vary header"
    );
}

/// **Default**: no `Vary` header.
#[tokio::test]
async fn default_no_vary_header() {
    let router = base_router();

    let resp = router.oneshot(make_get("/hello")).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("vary").is_none(),
        "without vary(), Vary must not be added"
    );
}

// ── builder API smoke-test (CachePlugin builder methods) ─────────────────────

/// Smoke-test that the `CachePlugin` builder chain compiles and that
/// `wrap_router` applies both layers end-to-end: the response carries
/// `Cache-Control` AND `Content-Encoding: gzip` when both are opted in.
#[tokio::test]
async fn cacheplugin_builder_compression_and_cache_control() {
    use umbral::prelude::Plugin;
    use umbral_cache::{Cache, CachePlugin};

    let plugin = CachePlugin::new(Cache::memory())
        .with_compression()
        .cache_control("public, max-age=3600")
        .vary("Accept-Encoding");

    // Apply the plugin's wrap_router just as the framework would.
    let router = plugin.wrap_router(base_router());

    let req = make_get_with_accept_encoding("/hello", "gzip");
    let resp = router.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(
        resp.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "with_compression() must produce Content-Encoding: gzip"
    );
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=3600"),
        "cache_control() must produce Cache-Control header"
    );
    assert_eq!(
        resp.headers()
            .get("vary")
            .and_then(|v| v.to_str().ok()),
        Some("Accept-Encoding"),
        "vary() must produce Vary header"
    );
}

/// Smoke-test: a default (no opt-in) `CachePlugin` applies no compression or
/// cache headers — confirmed via `wrap_router` not changing response headers.
#[tokio::test]
async fn cacheplugin_default_no_headers_added() {
    use umbral::prelude::Plugin;
    use umbral_cache::{Cache, CachePlugin};

    let plugin = CachePlugin::new(Cache::memory()); // no with_compression / cache_control

    let router = plugin.wrap_router(base_router());

    let req = make_get_with_accept_encoding("/hello", "gzip");
    let resp = router.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("content-encoding").is_none(),
        "default CachePlugin must not add Content-Encoding"
    );
    assert!(
        resp.headers().get("cache-control").is_none(),
        "default CachePlugin must not add Cache-Control"
    );
    assert!(
        resp.headers().get("vary").is_none(),
        "default CachePlugin must not add Vary"
    );
}
