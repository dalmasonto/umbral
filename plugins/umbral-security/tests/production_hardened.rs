//! audit_2 plugin-authz S1 — the security headers that matter most in prod
//! (HSTS, CSP, CORP) are off by default (dev-safe). `production_hardened()` is
//! the one-call preset that turns them on, so an operator doesn't hand-assemble
//! a `SecurityConfig` and risk forgetting one.

use axum::Router;
use axum::body::Body;
use axum::routing::get;
use http::{Method, Request, StatusCode};
use tower::ServiceExt;
use umbral::prelude::Plugin;
use umbral_security::SecurityPlugin;

fn hardened_app() -> Router {
    let inner = Router::new().route("/", get(|| async { "ok" }));
    SecurityPlugin::production_hardened().wrap_router(inner)
}

#[tokio::test]
async fn production_hardened_emits_hsts_csp_and_corp() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let resp = hardened_app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers();

    let sts = h
        .get("strict-transport-security")
        .expect("HSTS must be on under production_hardened")
        .to_str()
        .unwrap();
    assert!(
        sts.contains("max-age=") && sts.contains("preload"),
        "HSTS should include a max-age and preload, got: {sts}"
    );

    let csp = h
        .get("content-security-policy")
        .expect("CSP must be set under production_hardened")
        .to_str()
        .unwrap();
    assert!(
        csp.contains("default-src 'self'"),
        "CSP should have a strict default-src baseline, got: {csp}"
    );

    assert_eq!(
        h.get("cross-origin-resource-policy")
            .expect("CORP must be set")
            .to_str()
            .unwrap(),
        "same-origin"
    );
}

/// The preset must keep the dev-safe protections too (CSRF stays on) and mark
/// the CSRF cookie Secure (prod is HTTPS).
#[test]
fn production_hardened_config_keeps_csrf_and_marks_cookie_secure() {
    let cfg = umbral_security::SecurityConfig::production_hardened();
    assert!(cfg.csrf, "CSRF must stay on");
    assert!(cfg.hsts, "HSTS on");
    assert!(cfg.csrf_cookie_secure, "CSRF cookie Secure in prod");
    assert!(cfg.content_security_policy.is_some(), "CSP set");
}
