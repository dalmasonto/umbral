//! End-to-end tests for [`umbra_core::slash`].
//!
//! These tests bypass `App::build` (which initialises a process-wide
//! settings OnceLock and so only runs once per test binary) and
//! exercise the slash-redirect fallback handler directly. The handler
//! is the same one App::build installs, so the integration shape is
//! preserved.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::{get, post};
use tower::ServiceExt;
use umbra_core::slash::{SlashRedirect, slash_redirect_fallback};

/// Build a router with the slash-redirect fallback installed,
/// matching the wiring App::build does at Phase 5.6.
fn router_with_fallback(router: Router, policy: SlashRedirect) -> Router {
    if policy == SlashRedirect::Off {
        return router;
    }
    let snapshot = router.clone();
    router.fallback(slash_redirect_fallback(snapshot, policy, None))
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
// SlashRedirect::Append — Django default. `/foo` → 308 → `/foo/`.
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
