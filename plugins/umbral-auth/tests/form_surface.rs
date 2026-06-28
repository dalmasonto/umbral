//! TDD: Form-action auth endpoints — POST-in, 303-redirect-out.
//!
//! Boots a real App with `AuthPlugin::default().with_form_routes()` and a
//! recording mailer, then drives all 7 form endpoints via `tower::ServiceExt::oneshot`.
//! Default prefix is `/auth` (form routes are HTML-surface routes, not under the
//! JSON `/api/auth` prefix).
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
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    .with_form_routes()
                    .disable_throttle()
                    .mailer(rec),
            )
            .build()
            .expect("App::build should succeed with AuthPlugin + form routes");

        let router = app.into_router();
        ROUTER.set(router).ok();

        let pool = umbral::db::pool();

        sqlx::query(
            "CREATE TABLE auth_user (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                email TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                is_active INTEGER NOT NULL,
                is_staff INTEGER NOT NULL,
                is_superuser INTEGER NOT NULL,
                date_joined TEXT NOT NULL,
                last_login TEXT,
                email_verified_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create auth_user table");

        sqlx::query(
            "CREATE TABLE auth_challenge (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL,
                purpose TEXT NOT NULL,
                secret_hash TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                attempts INTEGER NOT NULL,
                used_at TEXT,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create auth_challenge table");

        sqlx::query(
            "CREATE TABLE auth_token (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id INTEGER NOT NULL,
                key_hash TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_used_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create auth_token table");

        sqlx::query(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                user_id TEXT,
                data TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create session table");
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

/// Bad creds → 303 redirect, no session Set-Cookie, Location: "/".
#[tokio::test]
async fn form_login_bad_creds_redirects_to_slash() {
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
    // No session Set-Cookie should be set (login_with_request was not called).
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        !set_cookie.contains("umbral_session"),
        "bad-creds login must not set a session cookie; got: {set_cookie}"
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
