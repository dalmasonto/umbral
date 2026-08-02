//! End-to-end tests for [`umbral_core::slash`].
//!
//! These tests bypass `App::build` (which initialises a process-wide
//! settings OnceLock and so only runs once per test binary) and apply
//! the slash-redirect probe LAYER directly — the same wiring App::build
//! installs at Phase 5.6, so the integration shape is preserved.
//!
//! gaps4 #50: the probe is a layer over the whole router, not a
//! fallback, so it fires on 404s from MATCHED routes too (wildcard
//! captures, nested services) — the class of URL that used to ignore
//! the policy entirely.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::{get, post};
use tower::ServiceExt;
use umbral_core::slash::{SlashRedirect, slash_redirect_probe};

/// Apply the slash-redirect layer, matching the wiring App::build does
/// at Phase 5.6 (snapshot taken before the layer is applied).
fn router_with_fallback(router: Router, policy: SlashRedirect) -> Router {
    if policy == SlashRedirect::Off {
        return router;
    }
    let snapshot = router.clone();
    router.layer(axum::middleware::from_fn(slash_redirect_probe(
        snapshot, policy,
    )))
}

async fn oneshot(router: Router, method: Method, path: &str) -> axum::http::Response<Body> {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    router.oneshot(req).await.unwrap()
}

// =====================================================================
// SlashRedirect::Off — no redirects, default axum behaviour.
// =====================================================================

#[tokio::test]
async fn off_policy_returns_plain_404_for_slashed_variant() {
    let router = router_with_fallback(
        Router::new().route("/articles", get(|| async { "articles ok" })),
        SlashRedirect::Off,
    );
    let resp = oneshot(router, Method::GET, "/articles/").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn off_policy_does_not_redirect_slashless_to_slashed() {
    let router = router_with_fallback(
        Router::new().route("/articles/", get(|| async { "articles slash ok" })),
        SlashRedirect::Off,
    );
    let resp = oneshot(router, Method::GET, "/articles").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// =====================================================================
// SlashRedirect::Append. `/foo` → 308 → `/foo/`.
// =====================================================================

#[tokio::test]
async fn append_policy_redirects_slashless_to_slashed_when_slashed_exists() {
    let router = router_with_fallback(
        Router::new().route("/articles/", get(|| async { "articles slash ok" })),
        SlashRedirect::Append,
    );
    let resp = oneshot(router, Method::GET, "/articles").await;
    assert_eq!(
        resp.status(),
        StatusCode::PERMANENT_REDIRECT,
        "Append policy should 308 when alternate matches"
    );
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok());
    assert_eq!(location, Some("/articles/"));
}

#[tokio::test]
async fn append_policy_passes_through_matching_slashless_route() {
    let router = router_with_fallback(
        Router::new().route("/articles", get(|| async { "articles ok" })),
        SlashRedirect::Append,
    );
    let resp = oneshot(router, Method::GET, "/articles").await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn append_policy_does_not_redirect_already_slashed() {
    let router = router_with_fallback(
        Router::new().route("/articles/", get(|| async { "articles slash ok" })),
        SlashRedirect::Append,
    );
    let resp = oneshot(router, Method::GET, "/articles/").await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn append_policy_returns_404_when_neither_form_matches() {
    let router = router_with_fallback(
        Router::new().route("/articles", get(|| async { "articles ok" })),
        SlashRedirect::Append,
    );
    let resp = oneshot(router, Method::GET, "/totally-fake").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn append_policy_preserves_query_string_on_redirect() {
    let router = router_with_fallback(
        Router::new().route("/articles/", get(|| async { "ok" })),
        SlashRedirect::Append,
    );
    let resp = oneshot(router, Method::GET, "/articles?page=2&sort=date").await;
    assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert_eq!(location, "/articles/?page=2&sort=date");
}

// =====================================================================
// SlashRedirect::Strip — REST convention. `/foo/` → 308 → `/foo`.
// =====================================================================

#[tokio::test]
async fn strip_policy_redirects_slashed_to_slashless_when_slashless_exists() {
    let router = router_with_fallback(
        Router::new().route("/articles", get(|| async { "ok" })),
        SlashRedirect::Strip,
    );
    let resp = oneshot(router, Method::GET, "/articles/").await;
    assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok());
    assert_eq!(location, Some("/articles"));
}

#[tokio::test]
async fn strip_policy_passes_through_matching_slashed_route() {
    let router = router_with_fallback(
        Router::new().route("/articles/", get(|| async { "ok" })),
        SlashRedirect::Strip,
    );
    let resp = oneshot(router, Method::GET, "/articles/").await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// =====================================================================
// 308 (not 301) preserves method — POST → POST after redirect.
// =====================================================================

#[tokio::test]
async fn append_redirect_uses_308_so_post_method_preserves() {
    let router = router_with_fallback(
        Router::new().route("/api/users/", post(|| async { "created" })),
        SlashRedirect::Append,
    );
    let resp = oneshot(router, Method::POST, "/api/users").await;
    assert_eq!(
        resp.status(),
        StatusCode::PERMANENT_REDIRECT,
        "expected 308 (not 301) so POST method is preserved on redirect"
    );
}

// =====================================================================
// gaps4 #50 — 404s from MATCHED routes redirect too. The fallback-based
// implementation never saw these: a wildcard capture like REST's
// `/api/{table}/` matched the request and 404ed from inside the
// handler, so "slash redirect is on but some URLs will not redirect".
// =====================================================================

/// The REST-shadow shape: `/api/{table}/` matches `/api/docs/` and
/// 404s ("unknown resource"), while the REAL `/api/docs` route exists
/// in the slashless form. Strip policy must rescue it.
#[tokio::test]
async fn strip_policy_redirects_a_404_from_a_matched_wildcard_route() {
    let router = router_with_fallback(
        Router::new()
            .route("/api/docs", get(|| async { "the docs" }))
            .route(
                "/api/{table}/",
                get(|| async { (StatusCode::NOT_FOUND, "unknown resource") }),
            ),
        SlashRedirect::Strip,
    );
    let resp = oneshot(router, Method::GET, "/api/docs/").await;
    assert_eq!(
        resp.status(),
        StatusCode::PERMANENT_REDIRECT,
        "a 404 produced by a MATCHED wildcard route must still probe the alternate"
    );
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok());
    assert_eq!(location, Some("/api/docs"));
}

/// The playground shape under Append: the bare wildcard matches
/// `/api/console` and 404s, while `/api/console/` is a real route.
#[tokio::test]
async fn append_policy_redirects_a_404_from_a_matched_wildcard_route() {
    let router = router_with_fallback(
        Router::new()
            .route("/api/console/", get(|| async { "the console" }))
            .route(
                "/api/{table}",
                get(|| async { (StatusCode::NOT_FOUND, "unknown resource") }),
            ),
        SlashRedirect::Append,
    );
    let resp = oneshot(router, Method::GET, "/api/console").await;
    assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok());
    assert_eq!(location, Some("/api/console/"));
}

/// A deliberate 404 whose alternate form ALSO 404s (missing row with
/// both slash forms registered, out-of-scope row) keeps its ORIGINAL
/// body — the probe must never replace an API's 404 payload.
#[tokio::test]
async fn a_404_with_no_answering_alternate_keeps_its_original_body() {
    let router = router_with_fallback(
        Router::new()
            .route(
                "/api/task/{id}",
                get(|| async { (StatusCode::NOT_FOUND, r#"{"detail":"no row"}"#) }),
            )
            .route(
                "/api/task/{id}/",
                get(|| async { (StatusCode::NOT_FOUND, r#"{"detail":"no row"}"#) }),
            ),
        SlashRedirect::Append,
    );
    let resp = oneshot(router, Method::GET, "/api/task/999").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    assert_eq!(
        &body[..],
        br#"{"detail":"no row"}"#,
        "the handler's own 404 body must pass through untouched"
    );
}

/// Non-404 responses are never touched — the layer is a pure 404 probe.
#[tokio::test]
async fn non_404_responses_pass_through_untouched() {
    let router = router_with_fallback(
        Router::new().route(
            "/api/thing",
            get(|| async { (StatusCode::IM_A_TEAPOT, "teapot") }),
        ),
        SlashRedirect::Append,
    );
    let resp = oneshot(router, Method::GET, "/api/thing").await;
    assert_eq!(resp.status(), StatusCode::IM_A_TEAPOT);
}
