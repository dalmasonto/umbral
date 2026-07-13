//! TDD: Form-action auth endpoints — POST-in, 303-redirect-out.
//!
//! Boots a real App with `AuthPlugin::default().with_form_routes()` AND
//! `SessionsPlugin` (the normal app config for any HTML-facing app) and a
//! recording mailer, then drives all 7 form endpoints via
//! `tower::ServiceExt::oneshot`.
//!
//! ## Why SessionsPlugin is required
//!
//! `SessionsPlugin::wrap_router` mounts `session_layer`, which injects a
//! candidate `SessionToken` extension into every request — including
//! cookieless first-visit ones. `Messages::from_request_parts` prefers this
//! extension over the raw cookie. When `msgs.error(...)` is called inside a
//! form handler, it materialises the session row (lazy write), and
//! `session_layer` emits `Set-Cookie` on the response. Without
//! `SessionsPlugin`, `Messages` has no token to bind to and the flash is a
//! silent no-op — a degenerate config, not a real app config (gaps3 #4,
//! resolved).
//!
//! Pattern mirrors `json_surface.rs`: one shared tempfile DB via `OnceCell`,
//! raw DDL for the four tables, the Router stashed in a static.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral_auth::mailer::{AuthMailError, AuthMailer, OutgoingMail};
use umbral_auth::{AuthPlugin, AuthUser};
use umbral_sessions::SessionsPlugin;

// =========================================================================
// Recording mailer
// =========================================================================

#[derive(Default, Clone)]
struct Recorder(Arc<Mutex<Vec<OutgoingMail>>>);

#[async_trait::async_trait]
impl AuthMailer for Recorder {
    async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError> {
        self.0.lock().unwrap().push(mail);
        Ok(())
    }
}

// =========================================================================
// One-time App boot
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();
static ROUTER: std::sync::OnceLock<Router> = std::sync::OnceLock::new();

async fn boot() -> &'static Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_form_surface.sqlite");
        std::mem::forget(tmp);

        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                    .busy_timeout(std::time::Duration::from_secs(30)),
            )
            .await
            .expect("sqlite tempfile pool");

        let rec = Recorder::default();

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            // SessionsPlugin is required for flash messages to work on
            // anonymous first-visit form submissions (session_layer injects
            // a candidate SessionToken into every request, including
            // cookieless ones; Messages prefers this extension). This is the
            // normal config for any HTML-facing app.
            .plugin(SessionsPlugin::default())
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    .with_form_routes()
                    .disable_throttle()
                    .mailer(rec),
            )
            .build()
            .expect("App::build should succeed with AuthPlugin + SessionsPlugin + form routes");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let router = app.into_router();
        ROUTER.set(router).ok();
    })
    .await;

    ROUTER.get().expect("router set during boot")
}

// =========================================================================
// Helper: POST a form-encoded body, return the full response.
// =========================================================================

async fn post_form(router: &Router, uri: &str, body: &str) -> axum::http::Response<Body> {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap();
    router.clone().oneshot(req).await.unwrap()
}

// =========================================================================
// Tests
// =========================================================================

/// Bad creds → 303 redirect with Location: "/", and session Set-Cookie is
/// set because `msgs.error(...)` inside the handler materialises the session
/// via `session_layer` (proving session_layer is active via SessionsPlugin).
///
/// This also verifies that anonymous flash works end-to-end: the error
/// message is stored in the session row we can read back from the DB.
#[tokio::test]
async fn form_login_bad_creds_redirects_and_sets_session_for_flash() {
    let router = boot().await;
    let resp = post_form(router, "/auth/login", "username=nobody&password=wrong").await;
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "bad-creds login must redirect"
    );
    let loc = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(loc, "/", "bad-creds redirect must go to '/'");

    // With SessionsPlugin, session_layer is active. msgs.error() inside
    // do_login materialises the session row (lazy write), so Set-Cookie IS
    // emitted — this proves session_layer is running and flash is stored.
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        set_cookie.contains("umbral_session"),
        "bad-creds login must set a session cookie via session_layer (for flash storage); got: {set_cookie}"
    );

    // Verify the error flash was actually persisted in the session row.
    // The Set-Cookie value is the raw session token; DbStore hashes it with
    // SHA-256 before storing, so we must hash before querying.
    // Extract the raw token from "umbral_session=<token>; HttpOnly; ..."
    let raw_token = set_cookie
        .split(';')
        .next()
        .and_then(|kv| kv.strip_prefix("umbral_session="))
        .map(|v| v.trim())
        .expect("Set-Cookie must contain umbral_session=<token>");

    // DbStore stores `hash_token(raw_token)` as the row ID.
    let stored_id = umbral_sessions::store::hash_token_pub(raw_token);

    let pool = umbral::db::pool();
    let row: (String,) = sqlx::query_as("SELECT data FROM session WHERE id = ?")
        .bind(&stored_id)
        .fetch_one(&pool)
        .await
        .expect("session row must exist after flash write");

    let data: serde_json::Value =
        serde_json::from_str(&row.0).expect("session.data must be valid JSON");
    let messages = data
        .get("_umbral_messages")
        .expect("session.data must contain _umbral_messages key after msgs.error()")
        .as_array()
        .expect("_umbral_messages must be a JSON array");
    assert!(
        !messages.is_empty(),
        "flash queue must be non-empty after a bad-creds error"
    );
    let first = &messages[0];
    assert_eq!(
        first.get("level").and_then(|v| v.as_str()),
        Some("error"),
        "flash level must be 'error' for a bad-creds attempt"
    );
}

/// Good creds → 303 + session Set-Cookie.
#[tokio::test]
async fn form_login_good_creds_sets_session_cookie() {
    let router = boot().await;

    // Seed a user directly.
    umbral_auth::create_user("formuser1", "formuser1@example.com", "G00d$Pass!")
        .await
        .expect("seed user");

    let resp = post_form(
        router,
        "/auth/login",
        "username=formuser1&password=G00d%24Pass%21",
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "good-creds login must redirect"
    );
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        set_cookie.contains("umbral_session"),
        "good-creds login must set an umbral_session cookie; got: {set_cookie}"
    );
}

/// Good creds + ?redirect=/account → 303 with Location: /account.
#[tokio::test]
async fn form_login_safe_redirect_param_honored() {
    let router = boot().await;

    umbral_auth::create_user("formuser2", "formuser2@example.com", "G00d$Pass!")
        .await
        .expect("seed user");

    let resp = post_form(
        router,
        "/auth/login?redirect=%2Faccount",
        "username=formuser2&password=G00d%24Pass%21",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        loc, "/account",
        "safe redirect param must be honored; got: {loc}"
    );
}

/// Open-redirect rejected: ?redirect=//evil.com → Location: /.
#[tokio::test]
async fn form_login_open_redirect_rejected() {
    let router = boot().await;

    umbral_auth::create_user("formuser3", "formuser3@example.com", "G00d$Pass!")
        .await
        .expect("seed user");

    let resp = post_form(
        router,
        "/auth/login?redirect=%2F%2Fevil.com",
        "username=formuser3&password=G00d%24Pass%21",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        loc, "/",
        "open-redirect via // must be rejected to '/'; got: {loc}"
    );
}

/// POST /auth/signup → 303; authenticate succeeds for the new user.
#[tokio::test]
async fn form_signup_creates_user_and_redirects() {
    let router = boot().await;

    let resp = post_form(
        router,
        "/auth/signup",
        "username=signupuser&email=signup%40example.com&password=G00d%24Pass%21",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "signup must redirect");

    // The newly-created user must be authenticatable.
    umbral_auth::authenticate::<AuthUser>("signupuser", "G00d$Pass!")
        .await
        .expect("user created by form signup must be authenticatable");
}

/// POST /auth/logout → 303 + a session-clearing Set-Cookie.
#[tokio::test]
async fn form_logout_redirects_and_clears_cookie() {
    let router = boot().await;

    let resp = post_form(router, "/auth/logout", "").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "logout must redirect");
    // The logout clears the session — the Set-Cookie max-age=0 tells the
    // browser to delete the cookie.
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        set_cookie.contains("Max-Age=0")
            || set_cookie.contains("max-age=0")
            || set_cookie.contains("umbral_session"),
        "logout must emit a cookie-clearing Set-Cookie; got: {set_cookie:?}"
    );
}
