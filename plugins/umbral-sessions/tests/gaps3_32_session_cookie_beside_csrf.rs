//! gaps3 #32 — the session layer must emit its `Set-Cookie` even when the
//! response already carries an UNRELATED cookie (the CSRF token).
//!
//! A fresh (cookieless) request that writes the session via `set_data`
//! materialises a row, and the layer's exit hook is supposed to hand the
//! client the session cookie so subsequent requests find that row. The old
//! guard was `!response.headers().contains_key(SET_COOKIE)` — "emit only if
//! NOTHING already set a cookie". But the CSRF layer sets `umbral_csrf_token`
//! on the very first request, so that guard saw a `Set-Cookie` and bailed:
//! the session row was orphaned and the client never got the session cookie.
//!
//! This is exactly what broke OAuth social login from a cross-origin SPA
//! (web3clubs_fc): `GET /oauth/{p}/login` does `set_data(token, flow)`, the
//! response carries only `umbral_csrf_token`, and the callback can't find the
//! flow → "no oauth flow in progress". It "worked" only for clients that
//! already held a session cookie (e.g. an admin already logged in).
//!
//! The existing `state_csrf.rs` test missed this because it wraps ONLY the
//! session layer (no CSRF cookie in the response). Here an inner layer
//! injects a `umbral_csrf_token` cookie — reproducing the real stack — and we
//! assert BOTH cookies come back.

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

use umbral::plugin::Plugin;
use umbral::web::header;
use umbral_sessions::{COOKIE_NAME, SessionToken, SessionsPlugin, read_session, set_data};

async fn boot() {
    let settings = umbral::Settings::from_env().expect("figment defaults load");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("gaps3_32.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("sqlite tempfile pool");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(SessionsPlugin::default())
        .build()
        .expect("App::build with SessionsPlugin");

    umbral::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema");
}

/// Stands in for OAuth `begin_flow`: writes the session out-of-band via the
/// free `set_data(token, …)` function (materialising a fresh row).
async fn writer(Extension(SessionToken(token)): Extension<SessionToken>) -> impl IntoResponse {
    set_data(&token, "oauth_flow", &"state-abc")
        .await
        .expect("set_data");
    "started"
}

/// Inner layer that sets the CSRF token cookie on the way out — as the real
/// CSRF middleware does on the first request. It runs *inside* the session
/// layer, so its `Set-Cookie` is present when the session layer's exit hook
/// checks the response.
async fn add_csrf_cookie(mut resp: Response<Body>) -> Response<Body> {
    resp.headers_mut().append(
        header::SET_COOKIE,
        "umbral_csrf_token=csrf-value; Path=/; SameSite=Lax"
            .parse()
            .unwrap(),
    );
    resp
}

fn all_set_cookies(resp: &Response<Body>) -> Vec<String> {
    resp.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn fresh_session_cookie_is_emitted_beside_the_csrf_cookie() {
    boot().await;

    let inner = Router::new()
        .route("/oauth/login", get(writer))
        .layer(axum::middleware::map_response(add_csrf_cookie));
    let router = SessionsPlugin::default().wrap_router(inner);

    // Cookieless client, exactly like a fresh browser hitting the SPA's
    // "sign in with Google" for the first time.
    let req = Request::builder()
        .uri("/oauth/login")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);

    let cookies = all_set_cookies(&resp);

    // The CSRF cookie the inner layer set must still be there — the fix
    // appends the session cookie, it does not clobber the CSRF one.
    assert!(
        cookies.iter().any(|c| c.starts_with("umbral_csrf_token=")),
        "the CSRF cookie must survive; got {cookies:?}",
    );

    // The bug: the session cookie must be emitted despite the CSRF cookie.
    let session_cookie = cookies
        .iter()
        .find(|c| c.starts_with(&format!("{COOKIE_NAME}=")))
        .unwrap_or_else(|| panic!("session Set-Cookie missing (gaps3 #32); got {cookies:?}"));

    // And it names the row that `set_data` actually wrote, so a callback that
    // reads the cookie back finds the flow.
    let token = session_cookie
        .strip_prefix(&format!("{COOKIE_NAME}="))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let record = read_session(token)
        .await
        .expect("read")
        .expect("the emitted session cookie must name a materialised row");
    let flow: Option<String> = umbral_sessions::get_data(&record, "oauth_flow").expect("get_data");
    assert_eq!(flow.as_deref(), Some("state-abc"));
}
