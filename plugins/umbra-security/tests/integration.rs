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
use umbra_security::{SecurityPlugin, generate_token};

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
async fn generate_token_is_64_hex_chars_and_unique() {
    let a = generate_token();
    let b = generate_token();
    assert_eq!(a.len(), 64, "32 bytes hex-encoded = 64 chars");
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}
