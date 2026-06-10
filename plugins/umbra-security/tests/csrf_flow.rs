//! End-to-end CSRF middleware flow against a real axum Router:
//! first-visit mint is visible to the handler (pre-handler mint),
//! Set-Cookie is appended (a handler's session cookie survives), the
//! POST re-render path has the token in scope, and rotation replaces
//! a cookie token that can't pass signed-mode validation.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbra_security::test_support::wrap_with_csrf;

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn app(signed: bool, secret: Option<&str>) -> Router {
    let routes = Router::new()
        .route(
            "/form",
            get(|| async { umbra::templates::current_csrf().unwrap_or_default() })
                .post(|| async { umbra::templates::current_csrf().unwrap_or_default() }),
        )
        .route(
            "/with-session-cookie",
            get(|| async { ([(header::SET_COOKIE, "umbra_session=abc; Path=/")], "ok") }),
        );
    wrap_with_csrf(routes, signed, secret.map(str::to_string))
}

fn cookie_token(resp: &axum::response::Response) -> Option<String> {
    resp.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .find_map(|v| {
            let s = v.to_str().ok()?;
            let rest = s.strip_prefix("umbra_csrf_token=")?;
            Some(rest.split(';').next().unwrap_or("").to_string())
        })
}

#[tokio::test]
async fn first_visit_handler_sees_the_minted_token() {
    let app = app(false, None);
    let resp = app
        .oneshot(Request::get("/form").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let minted = cookie_token(&resp).expect("first visit must set the csrf cookie");
    let seen = body_string(resp).await;
    assert_eq!(seen, minted, "handler-visible token must equal the cookie");
    assert!(!seen.is_empty());
}

#[tokio::test]
async fn set_cookie_is_appended_not_replaced() {
    let app = app(false, None);
    let resp = app
        .oneshot(
            Request::get("/with-session-cookie")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookies: Vec<String> = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert!(
        cookies.iter().any(|c| c.starts_with("umbra_session=")),
        "handler cookie clobbered: {cookies:?}"
    );
    assert!(
        cookies.iter().any(|c| c.starts_with("umbra_csrf_token=")),
        "csrf cookie missing: {cookies:?}"
    );
}

#[tokio::test]
async fn valid_post_passes_and_has_token_in_scope_for_rerenders() {
    let app = app(false, None);
    let tok = "a".repeat(64);
    let resp = app
        .oneshot(
            Request::post("/form")
                .header(header::COOKIE, format!("umbra_csrf_token={tok}"))
                .header("x-csrf-token", tok.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        body_string(resp).await,
        tok,
        "POST re-render must see the token"
    );
}

#[tokio::test]
async fn mismatched_post_is_403() {
    let app = app(false, None);
    let resp = app
        .oneshot(
            Request::post("/form")
                .header(
                    header::COOKIE,
                    format!("umbra_csrf_token={}", "a".repeat(64)),
                )
                .header("x-csrf-token", "b".repeat(64))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn stale_unsigned_cookie_rotates_under_signed_mode() {
    let app = app(true, Some("app-secret"));
    let resp = app
        .oneshot(
            Request::get("/form")
                .header(
                    header::COOKIE,
                    format!("umbra_csrf_token={}", "a".repeat(64)),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let rotated = cookie_token(&resp).expect("unsigned cookie must be re-minted");
    assert!(
        rotated.contains('.'),
        "rotated token must be signed: {rotated}"
    );
    let seen = body_string(resp).await;
    assert_eq!(
        seen, rotated,
        "handler must see the rotated token, not the stale one"
    );
}

#[tokio::test]
async fn valid_signed_cookie_is_not_rotated() {
    let app = app(true, Some("app-secret"));
    // Mint via a first request, then replay the minted cookie.
    let first = app
        .clone()
        .oneshot(Request::get("/form").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let minted = cookie_token(&first).unwrap();
    let second = app
        .oneshot(
            Request::get("/form")
                .header(header::COOKIE, format!("umbra_csrf_token={minted}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        cookie_token(&second).is_none(),
        "valid signed cookie must not re-mint"
    );
    assert_eq!(body_string(second).await, minted);
}
