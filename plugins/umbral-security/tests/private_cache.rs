//! gaps3 #44 — authenticated responses must not be storable by a shared cache.
//!
//! The reported symptom was a CSRF token visible in the admin's page source:
//! `<body hx-headers='{"X-CSRF-Token": "d88e…"}'>`. That part is by design.
//! umbral uses a double-submit scheme: the token is *also* readable by JS from
//! a non-`HttpOnly` cookie, because htmx has to put it on the wire. Its safety
//! rests on same-origin, not on confidentiality — a cross-origin page can
//! neither read the cookie nor the body.
//!
//! The real defect the screenshot points at is that nothing told any cache the
//! page was private. An admin page carries the viewer's CSRF token and their
//! data, and it shipped as a bare `200 text/html` with no `Cache-Control`. A
//! CDN, a corporate proxy, or the browser's own back/forward cache could store
//! it and hand one user's token-bearing page to the next. umbral's own
//! `cache_page` middleware already bypasses on the session cookie, so this is
//! about every cache umbral does *not* control.
//!
//! The rule pinned here: a *personalised* request — one carrying a session
//! cookie or an `Authorization` header — gets `Cache-Control: no-store,
//! private`. An anonymous request does not, so public marketing pages stay
//! cacheable. A handler that sets its own `Cache-Control` always wins.

use axum::Router;
use axum::body::Body;
use axum::routing::get;
use http::header::{AUTHORIZATION, CACHE_CONTROL, COOKIE};
use http::{Request, StatusCode};
use tower::ServiceExt;
use umbral::prelude::Plugin;
use umbral_security::{SecurityConfig, SecurityPlugin};

fn app() -> Router {
    SecurityPlugin::new().wrap_router(routes())
}

fn routes() -> Router {
    Router::new()
        .route("/", get(|| async { "public" }))
        .route("/admin/", get(|| async { "admin" }))
        // A handler that has already made its own caching decision.
        .route(
            "/assets/app.css",
            get(|| async { ([(CACHE_CONTROL, "public, max-age=86400")], "css") }),
        )
}

async fn cache_control(app: Router, req: Request<Body>) -> Option<String> {
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()
        .get(CACHE_CONTROL)
        .map(|v| v.to_str().unwrap().to_string())
}

fn get_with(uri: &str, header: Option<(http::HeaderName, &str)>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some((name, value)) = header {
        b = b.header(name, value);
    }
    b.body(Body::empty()).unwrap()
}

/// The reported case: an authenticated admin page must be unstorable. Both
/// directives matter — `private` stops shared caches, `no-store` stops the
/// browser's disk cache and back/forward cache from replaying it after logout.
#[tokio::test]
async fn session_cookie_request_is_no_store_and_private() {
    let req = get_with(
        "/admin/",
        Some((COOKIE, "umbral_session=abc123; umbral_csrf_token=deadbeef")),
    );
    let cc = cache_control(app(), req)
        .await
        .expect("an authenticated response must carry Cache-Control");

    assert!(cc.contains("no-store"), "expected no-store, got `{cc}`");
    assert!(cc.contains("private"), "expected private, got `{cc}`");
}

/// A bearer-token API request is personalised too, and carries no cookie.
#[tokio::test]
async fn authorization_header_request_is_no_store() {
    let req = get_with("/admin/", Some((AUTHORIZATION, "Bearer sometoken")));
    let cc = cache_control(app(), req)
        .await
        .expect("a bearer-authenticated response must carry Cache-Control");

    assert!(cc.contains("no-store"), "expected no-store, got `{cc}`");
}

/// An anonymous request stays cacheable. This is the whole reason the guard is
/// keyed on personalisation rather than blanket-applied: a marketing page or a
/// blog post served to a logged-out visitor should still hit the CDN. Note the
/// CSRF cookie alone does NOT make a request personalised — every first-time
/// anonymous visitor is minted one, so keying on it would mark the entire
/// public site `no-store`.
#[tokio::test]
async fn anonymous_request_keeps_no_cache_control() {
    let req = get_with("/", Some((COOKIE, "umbral_csrf_token=deadbeef")));
    assert_eq!(
        cache_control(app(), req).await,
        None,
        "an anonymous page must stay cacheable",
    );
}

/// A handler that set its own `Cache-Control` keeps it. The layer is
/// `if_not_present`, not an override: an authenticated request for a
/// fingerprinted static asset should still be cacheable.
#[tokio::test]
async fn handler_set_cache_control_wins() {
    let req = get_with("/assets/app.css", Some((COOKIE, "umbral_session=abc123")));
    assert_eq!(
        cache_control(app(), req).await.as_deref(),
        Some("public, max-age=86400"),
        "the handler's own Cache-Control must not be clobbered",
    );
}

/// Opt-out for an app that fronts umbral with a cache it trusts to key on the
/// session cookie itself.
#[tokio::test]
async fn private_cache_can_be_disabled() {
    let plugin = SecurityPlugin::with_config(SecurityConfig {
        private_cache: false,
        ..SecurityConfig::default()
    });
    let req = get_with("/admin/", Some((COOKIE, "umbral_session=abc123")));
    assert_eq!(
        cache_control(plugin.wrap_router(routes()), req).await,
        None,
        "private_cache = false disables the header",
    );
}

/// The session cookie name is matched exactly. A cookie that merely *contains*
/// the name as a substring (`not_umbral_session`, or a value that happens to
/// spell it) is not a session.
#[tokio::test]
async fn lookalike_cookie_is_not_a_session() {
    let req = get_with(
        "/",
        Some((COOKIE, "not_umbral_session=abc; x=umbral_session")),
    );
    assert_eq!(
        cache_control(app(), req).await,
        None,
        "only a real `umbral_session=` cookie marks the request personalised",
    );
}
