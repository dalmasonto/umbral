//! HTTP-level test for the email-action throttle (verify-email,
//! resend-verification, password-forgot). Isolated into its own binary so the
//! tiny budget (max=1 / hour) does not affect `json_surface.rs`, which boots
//! with `disable_throttle()`. Each test binary has its own process-global
//! `AUTH_THROTTLE` `OnceLock`, so the budget installed here only applies to
//! this binary.
//!
//! Tests that a second `POST /api/auth/password-forgot` from the SAME
//! IP+email combination within the window returns `429 Too Many Requests`.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral_auth::mailer::{AuthMailError, AuthMailer, OutgoingMail};
use umbral_auth::{AuthPlugin, AuthUser};

// =========================================================================
// Silent mailer (we don't need to inspect mails here)
// =========================================================================

#[derive(Default, Clone)]
struct SilentMailer(Arc<Mutex<Vec<OutgoingMail>>>);

#[async_trait::async_trait]
impl AuthMailer for SilentMailer {
    async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError> {
        self.0.lock().unwrap().push(mail);
        Ok(())
    }
}

// =========================================================================
// One-time App boot — tight email-action budget (max=1), throttling ON
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();
static ROUTER: std::sync::OnceLock<Router> = std::sync::OnceLock::new();

async fn boot() -> &'static Router {
    BOOT.get_or_init(|| async {
        let mut settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");
        // audit_2 H9: per-IP throttling requires a trusted proxy — otherwise
        // `X-Forwarded-For` is client-forgeable and everyone shares one bucket.
        // These tests simulate a single reverse proxy in front of the app.
        settings.trusted_proxy_hops = 1;

        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_email_action_throttle.sqlite");
        std::mem::forget(tmp);

        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                    .busy_timeout(std::time::Duration::from_secs(30)),
            )
            .await
            .expect("sqlite tempfile pool");

        let mailer = SilentMailer::default();

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    .with_default_routes()
                    // email_action budget of 1: the 2nd request from the same
                    // IP+email in the 1-hour window must 429.
                    .email_action_throttle(1, std::time::Duration::from_secs(3600))
                    // leave login/register throttle at defaults (not relevant here)
                    .mailer(mailer),
            )
            .build()
            .expect("App::build should succeed");

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
// Helper: POST JSON body with an explicit X-Forwarded-For IP.
// =========================================================================

async fn post_json_from_ip(
    router: &Router,
    uri: &str,
    body: &str,
    ip: &str,
) -> axum::http::Response<Body> {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-forwarded-for", ip)
        .body(Body::from(body.to_string()))
        .unwrap();
    router.clone().oneshot(req).await.unwrap()
}

// =========================================================================
// Tests
// =========================================================================

/// The first password-forgot request returns 202; the second from the same
/// IP+email (within the 1-request budget) returns 429.
#[tokio::test]
async fn password_forgot_429_after_email_action_budget_exhausted() {
    let router = boot().await;
    let ip = "192.0.2.201"; // unique to this test
    let body = r#"{"email":"email-throttle-test@example.com"}"#;
    let prefix = format!("{}/auth", umbral::web::api_base());
    let uri = format!("{prefix}/password-forgot");

    // First request: budget allows it (202 Accepted regardless of account existence).
    let resp = post_json_from_ip(router, &uri, body, ip).await;
    assert_eq!(
        resp.status(),
        StatusCode::ACCEPTED,
        "first password-forgot within budget must be 202"
    );

    // Second request from the same IP+email: over budget → 429.
    let resp = post_json_from_ip(router, &uri, body, ip).await;
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "second password-forgot from same IP+email must be 429 when budget=1"
    );
}

/// A different IP is unaffected by another IP's exhausted budget.
#[tokio::test]
async fn password_forgot_different_ip_unaffected_by_other_ip_lockout() {
    let router = boot().await;
    let ip_a = "192.0.2.202";
    let ip_b = "192.0.2.203";
    let body = r#"{"email":"email-throttle-different-ip@example.com"}"#;
    let prefix = format!("{}/auth", umbral::web::api_base());
    let uri = format!("{prefix}/password-forgot");

    // Exhaust ip_a's budget.
    post_json_from_ip(router, &uri, body, ip_a).await;
    let resp_a2 = post_json_from_ip(router, &uri, body, ip_a).await;
    assert_eq!(
        resp_a2.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "ip_a must be throttled on the 2nd request"
    );

    // ip_b has its own independent budget.
    let resp_b = post_json_from_ip(router, &uri, body, ip_b).await;
    assert_eq!(
        resp_b.status(),
        StatusCode::ACCEPTED,
        "ip_b must not be affected by ip_a's exhausted budget"
    );
}
