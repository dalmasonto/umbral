//! TDD: JSON surface — verify-email, resend-verification, password-forgot, password-reset.
//!
//! Boots a real App with `AuthPlugin::with_default_routes()` and a recording
//! mailer, then drives the four new endpoints via `tower::ServiceExt::oneshot`.
//! Default prefix resolves to `/api/auth` (api_base() = "/api").
//!
//! Boot pattern mirrors `verify_email.rs` and `password_reset.rs`: one shared
//! tempfile DB via a `tokio::sync::OnceCell`, raw DDL for all four tables,
//! the Router extracted via `App::into_router()` and stashed in a static.

use std::sync::{Arc, Mutex};

use axum::Router;
use tokio::sync::OnceCell;
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

impl Recorder {
    /// Most-recently-captured mail, or None if nothing sent yet.
    fn last(&self) -> Option<OutgoingMail> {
        self.0.lock().unwrap().last().cloned()
    }
}

// =========================================================================
// One-time App boot — OnceLocks can only be set once per binary.
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();
static RECORDER: std::sync::OnceLock<Recorder> = std::sync::OnceLock::new();
static ROUTER: std::sync::OnceLock<Router> = std::sync::OnceLock::new();

async fn boot_app_with_recorder() -> (Router, Recorder) {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        // Tempfile DB — every pool connection shares one on-disk file.
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_json_surface.sqlite");
        std::mem::forget(tmp); // keep file alive for the binary's lifetime

        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        let rec = Recorder::default();
        RECORDER.set(rec.clone()).ok();

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    // Keep default password policy so "G00d$Pass!" is validated normally.
                    .with_default_routes()
                    .disable_throttle()
                    .mailer(rec),
            )
            .build()
            .expect("App::build should succeed with AuthPlugin + Recorder mailer");

        // Extract the router (consumes App; ambient OnceLocks already set).
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

    let router = ROUTER.get().expect("router set during boot").clone();
    let rec = RECORDER.get().expect("recorder set during boot").clone();
    (router, rec)
}

// =========================================================================
// Helper
// =========================================================================

async fn post(router: &Router, uri: &str, body: &str) -> axum::http::StatusCode {
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    router.clone().oneshot(req).await.unwrap().status()
}

// =========================================================================
// Tests
// =========================================================================

#[tokio::test]
async fn json_verify_and_reset_endpoints() {
    let (router, rec) = boot_app_with_recorder().await;

    // Register via the JSON route.
    assert_eq!(
        post(
            &router,
            "/api/auth/register",
            r#"{"username":"dan","email":"dan@example.com","password":"G00d$Pass!"}"#
        )
        .await,
        axum::http::StatusCode::CREATED
    );

    // Resend verification: always 202, generic.
    assert_eq!(
        post(
            &router,
            "/api/auth/resend-verification",
            r#"{"email":"dan@example.com"}"#
        )
        .await,
        axum::http::StatusCode::ACCEPTED
    );
    let code: String = rec
        .last()
        .unwrap()
        .text
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();

    // Wrong code → 400 generic; right code → 204.
    assert_eq!(
        post(
            &router,
            "/api/auth/verify-email",
            r#"{"email":"dan@example.com","code":"000000"}"#
        )
        .await,
        axum::http::StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(
            &router,
            "/api/auth/verify-email",
            &format!(r#"{{"email":"dan@example.com","code":"{code}"}}"#)
        )
        .await,
        axum::http::StatusCode::NO_CONTENT
    );

    // Forgot is always 202 even for unknown emails.
    assert_eq!(
        post(
            &router,
            "/api/auth/password-forgot",
            r#"{"email":"ghost@example.com"}"#
        )
        .await,
        axum::http::StatusCode::ACCEPTED
    );
}
