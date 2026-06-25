//! OAuth `state` CSRF defense (gaps2 #85).
//!
//! The callback at `GET /oauth/{provider}/callback` rejects any request
//! whose `state` query parameter does not match the one-time token the
//! login handler persisted in the session before the provider redirect
//! (`routes.rs`, the `if !state_ok || flow.provider != provider` guard).
//! This is the CSRF defense: an attacker who tricks a victim into hitting
//! the callback with a forged `code`/`state` must NOT be able to complete
//! a sign-in, because they cannot know the server-minted state bound to
//! the victim's session.
//!
//! Driven through the REAL mounted routes + REAL session layer:
//!   1. `GET /oauth/google/login` starts a flow → persists `state` in the
//!      session, hands back a `Set-Cookie`.
//!   2. A callback carrying that cookie but a DIFFERENT `state` is rejected
//!      with `400 "oauth state mismatch"`; no `SocialAccount` is written.
//!   3. Positive control: a callback carrying the SAME cookie and the
//!      genuine state proceeds PAST the state check (it then fails later on
//!      the missing `code` with a distinct `400 "missing authorization
//!      code"`, never the state-mismatch error). That proves the matching
//!      state was accepted without needing a live network exchange.
//!
//! Its own test binary so the single-row session read sees a clean DB.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;
use umbral::plugin::Plugin;
use umbral::web::header;
use umbral_oauth::OAuthPlugin;
use umbral_oauth::SocialAccount;
use umbral_oauth::providers::GoogleProvider;
use umbral_sessions::SessionsPlugin;

/// `App::build()` publishes process-wide ambient state (settings + pool
/// `OnceLock`), so it runs exactly once per test binary. Both scenarios
/// boot through this `OnceCell` and then share the one DB.
static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(init).await;
}

async fn init() {
    let settings = umbral::Settings::from_env().expect("figment defaults load");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("state_csrf.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
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

    let pool = umbral::db::pool();
    sqlx::query(
        "CREATE TABLE session (\
            id TEXT PRIMARY KEY,\
            user_id TEXT,\
            data TEXT NOT NULL,\
            created_at TEXT NOT NULL,\
            expires_at TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("create session");
    // The callback's create-or-link policy would write here on success;
    // we assert it stays empty on a rejected callback. The table carries
    // the plugin prefix (`oauth_`), matching what the migration engine
    // generates for `SocialAccount`. Only enough columns for `COUNT(*)`.
    sqlx::query(
        "CREATE TABLE oauth_social_account (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            user_id INTEGER NOT NULL,\
            provider TEXT NOT NULL,\
            provider_uid TEXT NOT NULL,\
            provider_email TEXT,\
            email_verified INTEGER NOT NULL DEFAULT 0,\
            access_token TEXT NOT NULL DEFAULT '',\
            refresh_token TEXT,\
            scopes TEXT NOT NULL DEFAULT '',\
            expires_at TEXT,\
            created_at TEXT NOT NULL DEFAULT '',\
            updated_at TEXT NOT NULL DEFAULT ''\
         )",
    )
    .execute(&pool)
    .await
    .expect("create oauth_social_account");
}

/// Build a fresh wrapped router + drive a login to obtain a real session
/// cookie AND the genuine state that the flow persisted. Returns
/// `(router, cookie, genuine_state)`.
async fn start_flow() -> (axum::Router, String, String) {
    let oauth = OAuthPlugin::new("https://app.example.com")
        .provider(GoogleProvider::new("client123", "secret"));
    let router = SessionsPlugin::default().wrap_router(oauth.routes());

    let req = Request::builder()
        .uri("/oauth/google/login")
        .body(Body::empty())
        .unwrap();
    let resp = router
        .clone()
        .oneshot(req)
        .await
        .expect("oneshot login");
    assert!(
        resp.status().is_redirection(),
        "login must redirect to the provider, got {}",
        resp.status()
    );

    // The Set-Cookie names the freshly-minted session whose row holds the
    // persisted flow state.
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("login issues a session Set-Cookie")
        .to_str()
        .expect("ascii Set-Cookie");
    // The cookie header we echo back is just `name=value` (the first pair).
    let cookie = set_cookie
        .split(';')
        .next()
        .expect("cookie name=value pair")
        .to_string();

    // The genuine state is the `state` query param on the redirect URL,
    // which equals the value persisted in the session.
    let location = resp
        .headers()
        .get(header::LOCATION)
        .expect("Location header")
        .to_str()
        .expect("ascii Location");
    let url = url::Url::parse(location).expect("absolute Location");
    let genuine_state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .expect("state on the redirect");

    (router, cookie, genuine_state)
}

async fn social_account_count() -> i64 {
    SocialAccount::objects()
        .count()
        .await
        .expect("count social_account")
}

#[tokio::test]
async fn callback_with_mismatched_state_is_rejected_and_writes_nothing() {
    boot().await;
    let (router, cookie, genuine_state) = start_flow().await;

    // A forged callback: the attacker carries the victim's session cookie
    // but a state they invented (NOT the one bound to the session).
    let forged_state = "attacker-controlled-state-value";
    assert_ne!(
        forged_state, genuine_state,
        "the test's forged state must differ from the genuine one"
    );

    let before = social_account_count().await;

    let req = Request::builder()
        .uri(format!(
            "/oauth/google/callback?state={forged_state}&code=stolen-code"
        ))
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot callback");

    assert_eq!(
        resp.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "a mismatched state must be rejected with 400"
    );
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let text = String::from_utf8_lossy(&body);
    assert_eq!(
        text, "oauth state mismatch",
        "rejection must be the state-mismatch error, got {text:?}"
    );

    // No session was established (no fresh login Set-Cookie beyond the
    // flow's) and — critically — no SocialAccount was created.
    assert_eq!(
        social_account_count().await,
        before,
        "a rejected callback must not create a SocialAccount"
    );
}

#[tokio::test]
async fn callback_with_matching_state_passes_the_csrf_check() {
    boot().await;
    let (router, cookie, genuine_state) = start_flow().await;

    // The genuine callback: same cookie, the real state — but we omit the
    // `code` so the flow can't reach the live Google token exchange. The
    // point is to prove the state check ACCEPTED the matching value: the
    // error is the DISTINCT "missing authorization code", never the
    // state-mismatch error. If the state check had rejected, we'd see the
    // mismatch error instead and never reach the code check.
    let req = Request::builder()
        .uri(format!("/oauth/google/callback?state={genuine_state}"))
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot callback");

    assert_eq!(
        resp.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "the missing-code branch also 400s, but for a different reason"
    );
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let text = String::from_utf8_lossy(&body);
    assert_eq!(
        text, "missing authorization code",
        "a matching state must pass the CSRF check and fail later on the \
         missing code; got {text:?}"
    );
    assert_ne!(
        text, "oauth state mismatch",
        "a matching state must NOT be rejected by the CSRF check"
    );
}
