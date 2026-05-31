//! Tests for [`umbra_core::errors`] — the 404 / 500 helper layer.
//!
//! Exercises the fallback handlers + panic-catch layer directly
//! (same approach as the slash-redirect tests — bypasses `App::build`
//! since its OnceLock only lets one App boot per test binary).

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::get;
use tower::ServiceExt;
use umbra_core::errors::{not_found_fallback, render_not_found, server_error_panic_handler};

async fn oneshot(router: Router, method: Method, path: &str) -> axum::http::Response<Body> {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    router.oneshot(req).await.unwrap()
}

async fn read_body(resp: axum::http::Response<Body>) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

// =====================================================================
// render_not_found — the core helper.
// =====================================================================

#[test]
fn render_not_found_returns_404_plain_text_without_template() {
    let resp = render_not_found(None, "/missing");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
    assert!(
        ct.to_str().unwrap().starts_with("text/plain"),
        "expected text/plain content-type when no template; got {ct:?}"
    );
}

// =====================================================================
// not_found_fallback — installed when slash_redirect is Off but
// not_found_template is set.
// =====================================================================

#[tokio::test]
async fn not_found_fallback_returns_404_for_unmatched_path() {
    let router = Router::new()
        .route("/", get(|| async { "home" }))
        .fallback(not_found_fallback(None));
    let resp = oneshot(router, Method::GET, "/totally-fake").await;
    let (status, body) = read_body(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "Not Found");
}

#[tokio::test]
async fn not_found_fallback_passes_matched_routes_through() {
    let router = Router::new()
        .route("/", get(|| async { "home" }))
        .fallback(not_found_fallback(None));
    let resp = oneshot(router, Method::GET, "/").await;
    let (status, body) = read_body(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "home");
}

// =====================================================================
// server_error_panic_handler — composed with CatchPanicLayer.
// =====================================================================

#[tokio::test]
async fn panic_handler_converts_panic_to_500_with_default_body() {
    let handler = server_error_panic_handler(None);
    let router = Router::new()
        .route(
            "/boom",
            get(|| async {
                panic!("intentional panic for testing");
                #[allow(unreachable_code)]
                ""
            }),
        )
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(handler));
    let resp = oneshot(router, Method::GET, "/boom").await;
    let (status, body) = read_body(resp).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, "Internal Server Error");
}

#[tokio::test]
async fn panic_handler_lets_non_panicking_handlers_through() {
    let handler = server_error_panic_handler(None);
    let router = Router::new()
        .route("/ok", get(|| async { "all good" }))
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(handler));
    let resp = oneshot(router, Method::GET, "/ok").await;
    let (status, body) = read_body(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "all good");
}
