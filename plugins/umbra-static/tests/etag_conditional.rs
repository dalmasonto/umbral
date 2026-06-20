//! Tests for ETag / conditional-GET on embedded static assets.
//!
//! Covers:
//! - GET an embedded asset → 200 AND an `ETag` response header present.
//! - GET the same asset with `If-None-Match: <that etag>` → 304 and empty body.
//! - GET with a non-matching `If-None-Match` value → 200 + full body.
//! - GET with `If-None-Match: *` (wildcard) → 304.
//! - ETag is stable across repeated requests for the same content.

use axum::body::Body;
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use include_dir::{Dir, include_dir};
use tower::ServiceExt;
use umbra::prelude::*;
use umbra_static::StaticPlugin;

/// The same fixture tree used by the embedded tests.
static FIXTURE: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures");

async fn body_bytes(resp: http::Response<Body>) -> Vec<u8> {
    resp.into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Send a GET (or HEAD) and return the full response.
async fn get(router: Router, uri: &str) -> http::Response<Body> {
    router
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Send a GET with an `If-None-Match` header.
async fn get_with_inm(router: Router, uri: &str, inm: &str) -> http::Response<Body> {
    router
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header(http::header::IF_NONE_MATCH, inm)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// A 200 response for an embedded asset must carry an `ETag` header.
#[tokio::test]
async fn embedded_200_carries_etag_header() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);
    let router = plugin.routes();
    let resp = get(router, "/static/sample.css").await;

    assert_eq!(resp.status(), StatusCode::OK, "expected 200");
    let etag = resp
        .headers()
        .get(http::header::ETAG)
        .expect("ETag header must be present on 200 response for embedded asset");
    let etag_str = etag.to_str().expect("ETag must be ASCII");
    assert!(
        etag_str.starts_with('"') && etag_str.ends_with('"'),
        "ETag must be a quoted-string per HTTP spec, got: {etag_str}"
    );
}

/// If-None-Match matching the asset's ETag → 304 with empty body.
#[tokio::test]
async fn embedded_if_none_match_hit_returns_304_empty_body() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);

    // Step 1: prime the ETag with a normal GET.
    let resp_200 = get(plugin.routes(), "/static/sample.css").await;
    assert_eq!(resp_200.status(), StatusCode::OK);
    let etag = resp_200
        .headers()
        .get(http::header::ETAG)
        .expect("ETag must be present on first GET")
        .to_str()
        .unwrap()
        .to_owned();

    // Step 2: conditional GET with the matching ETag.
    let resp_304 = get_with_inm(plugin.routes(), "/static/sample.css", &etag).await;
    assert_eq!(
        resp_304.status(),
        StatusCode::NOT_MODIFIED,
        "matching If-None-Match must produce 304, not {}",
        resp_304.status()
    );

    // The body of a 304 must be empty.
    let body = body_bytes(resp_304).await;
    assert!(
        body.is_empty(),
        "304 body must be empty, got {} bytes",
        body.len()
    );
}

/// If-None-Match with a different (non-matching) ETag → 200 + full body.
#[tokio::test]
async fn embedded_if_none_match_miss_returns_200_with_body() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);
    let router = plugin.routes();

    let resp = get_with_inm(router, "/static/sample.css", "\"deadbeefdeadbeef\"").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "non-matching If-None-Match must return 200"
    );
    let body = body_bytes(resp).await;
    assert!(!body.is_empty(), "200 body must not be empty");
}

/// If-None-Match: * (wildcard) → 304 for an existing embedded asset.
#[tokio::test]
async fn embedded_if_none_match_wildcard_returns_304() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);
    let router = plugin.routes();

    let resp = get_with_inm(router, "/static/sample.js", "*").await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_MODIFIED,
        "If-None-Match: * must return 304 for a known embedded asset"
    );
}

/// ETag is stable: two GETs of the same file produce the same ETag.
#[tokio::test]
async fn embedded_etag_is_stable_across_requests() {
    let plugin = StaticPlugin::embedded("/static", &FIXTURE);

    let etag1 = get(plugin.routes(), "/static/sample.css")
        .await
        .headers()
        .get(http::header::ETAG)
        .expect("ETag on first GET")
        .to_str()
        .unwrap()
        .to_owned();

    let etag2 = get(plugin.routes(), "/static/sample.css")
        .await
        .headers()
        .get(http::header::ETAG)
        .expect("ETag on second GET")
        .to_str()
        .unwrap()
        .to_owned();

    assert_eq!(
        etag1, etag2,
        "ETag must be deterministic for the same content"
    );
}

/// Different assets get different ETags.
#[tokio::test]
async fn embedded_different_assets_have_different_etags() {
    let plugin_css = StaticPlugin::embedded("/static", &FIXTURE);
    let etag_css = get(plugin_css.routes(), "/static/sample.css")
        .await
        .headers()
        .get(http::header::ETAG)
        .expect("ETag for css")
        .to_str()
        .unwrap()
        .to_owned();

    let plugin_js = StaticPlugin::embedded("/static", &FIXTURE);
    let etag_js = get(plugin_js.routes(), "/static/sample.js")
        .await
        .headers()
        .get(http::header::ETAG)
        .expect("ETag for js")
        .to_str()
        .unwrap()
        .to_owned();

    assert_ne!(
        etag_css, etag_js,
        "different files must have different ETags: css={etag_css} js={etag_js}"
    );
}
