//! Integration coverage for umbra-security. Exercises the CSRF
//! double-submit flow and the security-header bundle by running
//! requests through `Plugin::wrap_router` against a one-route
//! Router.

use axum::Router;
use axum::body::Body;
use axum::routing::{get, post};
use http::header::{COOKIE, HeaderValue, SET_COOKIE};
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbra::prelude::Plugin;
use umbra_security::{SecurityConfig, SecurityPlugin, generate_token};

fn app() -> Router {
    let inner = Router::new()
        .route("/", get(|| async { "ok-get" }))
        .route("/save", post(|| async { "ok-save" }));
    SecurityPlugin::new().wrap_router(inner)
}

async fn body_string(resp: http::Response<Body>) -> (StatusCode, http::HeaderMap, String) {
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn safe_method_without_cookie_gets_one_set() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = body_string(app().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok-get");
    let set_cookie = headers
        .get(SET_COOKIE)
        .expect("first GET should mint a CSRF cookie");
    let s = set_cookie.to_str().unwrap();
    assert!(s.starts_with("umbra_csrf_token="), "got: {s}");
    assert!(s.contains("Path=/"));
    assert!(s.contains("SameSite=Lax"));
}

#[tokio::test]
async fn safe_method_with_existing_cookie_does_not_re_set() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .header(COOKIE, "umbra_csrf_token=abcdef")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = body_string(app().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers.get(SET_COOKIE).is_none(),
        "cookie was already present"
    );
}

#[tokio::test]
async fn write_request_without_cookie_or_header_is_403() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/save")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = body_string(app().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("CSRF"));
}

#[tokio::test]
async fn write_request_with_cookie_but_no_header_is_403() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/save")
        .header(COOKIE, "umbra_csrf_token=tok-1")
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = body_string(app().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn write_request_with_mismatched_tokens_is_403() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/save")
        .header(COOKIE, "umbra_csrf_token=tok-1")
        .header("x-csrf-token", "tok-2")
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = body_string(app().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn write_request_with_matching_tokens_passes() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/save")
        .header(COOKIE, "umbra_csrf_token=matching")
        .header("x-csrf-token", "matching")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = body_string(app().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok-save");
}

#[tokio::test]
async fn default_security_headers_are_set() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = body_string(app().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("x-content-type-options"),
        Some(&HeaderValue::from_static("nosniff"))
    );
    assert_eq!(
        headers.get("x-frame-options"),
        Some(&HeaderValue::from_static("DENY"))
    );
    assert_eq!(
        headers.get("referrer-policy"),
        Some(&HeaderValue::from_static("strict-origin-when-cross-origin"))
    );
    assert!(
        headers.get("strict-transport-security").is_none(),
        "HSTS should be off by default"
    );
}

#[tokio::test]
async fn hsts_header_appears_when_opted_in() {
    let inner = Router::new().route("/", get(|| async { "ok" }));
    let router = SecurityPlugin::new().with_hsts(true).wrap_router(inner);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert!(headers.get("strict-transport-security").is_some());
}

#[tokio::test]
async fn xss_protection_default_is_disabled() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = body_string(app().oneshot(req).await.unwrap()).await;
    assert_eq!(
        headers.get("x-xss-protection"),
        Some(&HeaderValue::from_static("0")),
        "modern guidance disables the legacy XSS auditor"
    );
}

#[tokio::test]
async fn opt_in_headers_appear_when_configured() {
    let inner = Router::new().route("/", get(|| async { "ok" }));
    let router = SecurityPlugin::with_config(SecurityConfig {
        content_security_policy: Some("default-src 'self'".into()),
        permissions_policy: Some("geolocation=()".into()),
        server_header: Some("umbra".into()),
        ..Default::default()
    })
    .wrap_router(inner);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(
        headers.get("content-security-policy"),
        Some(&HeaderValue::from_static("default-src 'self'"))
    );
    assert_eq!(
        headers.get("permissions-policy"),
        Some(&HeaderValue::from_static("geolocation=()"))
    );
    assert_eq!(
        headers.get("server"),
        Some(&HeaderValue::from_static("umbra"))
    );
}

#[tokio::test]
async fn server_header_can_be_stripped() {
    let inner = Router::new().route(
        "/",
        get(|| async { ([(http::header::SERVER, "leaky/1.2.3")], "ok") }),
    );
    let router = SecurityPlugin::with_config(SecurityConfig {
        // Default sets `Server: umbra`; to strip, clear it and ask to hide.
        server_header: None,
        hide_server_header: true,
        ..Default::default()
    })
    .wrap_router(inner);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert!(
        headers.get("server").is_none(),
        "Server header should be stripped"
    );
}

#[tokio::test]
async fn server_and_coop_headers_are_on_by_default() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = body_string(app().oneshot(req).await.unwrap()).await;
    assert_eq!(
        headers.get("server"),
        Some(&HeaderValue::from_static("umbra")),
        "umbra advertises a Server header by default (like Django's daphne)"
    );
    assert_eq!(
        headers.get("cross-origin-opener-policy"),
        Some(&HeaderValue::from_static("same-origin")),
        "COOP on by default, matching Django 4.0+"
    );
}

#[tokio::test]
async fn exempt_path_skips_csrf_on_writes() {
    let inner = Router::new().route("/api/save", post(|| async { "ok-api" }));
    let router = SecurityPlugin::with_config(SecurityConfig {
        csrf_exempt_paths: vec!["/api".into()],
        ..Default::default()
    })
    .wrap_router(inner);

    // A cookieless write (bearer-auth API client) that would 403 under CSRF
    // passes because /api is exempt.
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/save")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok-api");
}

#[tokio::test]
async fn middleware_token_wins_over_handler_minted_cookie() {
    // The middleware is the only mint (docs/decisions/
    // 2026-06-10-automatic-csrf.md): `ensure_csrf_cookie` is gone and a
    // handler that still sets its own CSRF cookie no longer wins. The
    // middleware APPENDS its cookie after the handler's, so the browser
    // (last-wins for same-name cookies) keeps the middleware's token —
    // which is also the ambient token templates render. The two stay
    // consistent without any deference logic; the handler's cookie is
    // appended-around, not clobbered.
    let inner = Router::new().route(
        "/form",
        get(|| async {
            (
                [(
                    SET_COOKIE,
                    "umbra_csrf_token=handler-minted; Path=/; SameSite=Lax",
                )],
                // What `{{ csrf_token }}` would render into the form.
                umbra::templates::current_csrf().unwrap_or_default(),
            )
        }),
    );
    let router = SecurityPlugin::new().wrap_router(inner);
    let req = Request::builder()
        .method(Method::GET)
        .uri("/form")
        .body(Body::empty())
        .unwrap();
    let (_, headers, body) = body_string(router.oneshot(req).await.unwrap()).await;
    let cookies: Vec<&str> = headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    let last_csrf = cookies
        .iter()
        .filter(|c| c.starts_with("umbra_csrf_token="))
        .last()
        .expect("middleware must append its cookie")
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("umbra_csrf_token=");
    assert_ne!(
        last_csrf, "handler-minted",
        "middleware's token must be the browser-effective (last) cookie"
    );
    assert_eq!(
        body, last_csrf,
        "ambient token rendered into forms must match the effective cookie"
    );
    assert!(
        cookies.iter().any(|c| c.contains("handler-minted")),
        "append must not destroy the handler's header, got: {cookies:?}"
    );
}

#[tokio::test]
async fn request_body_limit_rejects_oversize_body() {
    let inner = Router::new().route("/save", post(|| async { "ok" }));
    let router = SecurityPlugin::with_config(SecurityConfig {
        csrf: false, // isolate the body-limit behaviour from CSRF
        request_body_limit: Some(8),
        ..Default::default()
    })
    .wrap_router(inner);

    let body = "this body is definitely longer than eight bytes";
    let req = Request::builder()
        .method(Method::POST)
        .uri("/save")
        // A declared Content-Length over the cap trips the layer's immediate
        // 413 short-circuit (the realistic path; clients send Content-Length).
        .header(http::header::CONTENT_LENGTH, body.len())
        .body(Body::from(body))
        .unwrap();
    let (status, _, _) = body_string(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn generate_token_is_64_hex_chars_and_unique() {
    let a = generate_token();
    let b = generate_token();
    assert_eq!(a.len(), 64, "32 bytes hex-encoded = 64 chars");
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}
