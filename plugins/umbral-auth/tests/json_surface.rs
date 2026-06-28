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

    /// Most-recently-captured mail sent to `email`, or None.
    /// Safer than `last()` when multiple tests share the recorder: it scopes
    /// the lookup to the address under test so concurrent mails to other
    /// addresses don't interfere.
    fn last_to(&self, email: &str) -> Option<OutgoingMail> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m.to == email)
            .cloned()
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
        // WAL journal mode + 30s busy timeout: the three concurrent tokio tests
        // in this binary all share one SQLite file. Without WAL, concurrent
        // writers fail immediately with "database is locked"; with WAL + a
        // generous busy timeout they queue safely.
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

/// Fix 1: exercise POST /api/auth/password-reset end-to-end via HTTP.
///
/// Verifies:
/// - password-forgot → 202 for a known user.
/// - Extracted reset token has the expected prefix.
/// - password-reset with a weak password → 400 (default policy enforced;
///   this boot does NOT call disable_password_validation).
/// - password-reset with a strong password → 204.
/// - Replaying the same token → 400 (single-use).
#[tokio::test]
async fn json_password_reset_via_http() {
    let (router, rec) = boot_app_with_recorder().await;

    // Register a distinct user for this test (unique username/email).
    // NOTE: username "charlie" is chosen deliberately so "Br4nd-New$Pass" passes
    // the UserAttributeSimilarityValidator — only 3/7 of "charlie"'s distinct
    // chars (a, r, e) appear in "br4nd-new$pass", giving ~43% overlap, well
    // below the 70% rejection threshold. Usernames with chars that heavily
    // overlap the test password (e.g. "pruser" → p,r,s,e → 4/5 = 80%) would
    // be rejected by the validator, causing a false 400 at reset time.
    assert_eq!(
        post(
            &router,
            "/api/auth/register",
            r#"{"username":"charlie","email":"charlie@example.com","password":"G00d$Pass!"}"#
        )
        .await,
        axum::http::StatusCode::CREATED
    );

    // Trigger forgot-password → always 202.
    assert_eq!(
        post(
            &router,
            "/api/auth/password-forgot",
            r#"{"email":"charlie@example.com"}"#
        )
        .await,
        axum::http::StatusCode::ACCEPTED
    );

    // Extract the reset token from the rendered email body.
    // The test client sends no Host header, so reset_url_base falls back to
    // "/auth/reset", producing the text line:
    //   "Reset your password: /auth/reset?token=umbral_XXXXXX"
    let mail = rec
        .last_to("charlie@example.com")
        .expect("a reset email must have been sent to charlie@example.com");
    let token = mail
        .text
        .split("token=")
        .nth(1)
        .expect("reset link text body must contain 'token='")
        .split_whitespace()
        .next()
        .expect("token must be followed by whitespace or end-of-input")
        .to_string();
    assert!(
        token.starts_with("umbral_"),
        "extracted reset token must have the 'umbral_' prefix; got {token:?}"
    );

    // Weak password → 400 (default password policy is active; this boot does
    // NOT call disable_password_validation).
    let weak_body = format!(r#"{{"token":"{token}","new_password":"123"}}"#);
    assert_eq!(
        post(&router, "/api/auth/password-reset", &weak_body).await,
        axum::http::StatusCode::BAD_REQUEST,
        "weak password must be rejected by the default policy"
    );

    // Strong password → 204 (success; challenge consumed).
    let strong_body = format!(r#"{{"token":"{token}","new_password":"Br4nd-New$Pass"}}"#);
    assert_eq!(
        post(&router, "/api/auth/password-reset", &strong_body).await,
        axum::http::StatusCode::NO_CONTENT,
        "valid strong password must be accepted and return 204"
    );

    // Single-use: replaying the same token (even with a strong password) → 400.
    assert_eq!(
        post(&router, "/api/auth/password-reset", &strong_body).await,
        axum::http::StatusCode::BAD_REQUEST,
        "a consumed reset token must not be accepted a second time"
    );
}

/// Fix 2: resend-verification must return 202 for an already-verified user
/// (anti-enumeration contract).
///
/// Flow: register → resend-verification (get code) → verify-email with correct
/// code → resend-verification again → must STILL be 202, not a status that
/// reveals whether the user is verified.
#[tokio::test]
async fn json_resend_verification_returns_202_for_verified_user() {
    let (router, rec) = boot_app_with_recorder().await;

    // Register a distinct user for this test.
    assert_eq!(
        post(
            &router,
            "/api/auth/register",
            r#"{"username":"rvuser","email":"rvuser@example.com","password":"G00d$Pass!"}"#
        )
        .await,
        axum::http::StatusCode::CREATED
    );

    // Resend verification while the user is still unverified → 202 + mail sent.
    assert_eq!(
        post(
            &router,
            "/api/auth/resend-verification",
            r#"{"email":"rvuser@example.com"}"#
        )
        .await,
        axum::http::StatusCode::ACCEPTED
    );

    // Extract the 6-digit code from the recorder (scoped to rvuser's address).
    let code: String = rec
        .last_to("rvuser@example.com")
        .expect("a verification email must have been sent to rvuser@example.com")
        .text
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();

    // Verify the email with the correct code → 204.
    assert_eq!(
        post(
            &router,
            "/api/auth/verify-email",
            &format!(r#"{{"email":"rvuser@example.com","code":"{code}"}}"#)
        )
        .await,
        axum::http::StatusCode::NO_CONTENT,
        "correct verification code must return 204"
    );

    // ANTI-ENUMERATION: resend-verification on an ALREADY-VERIFIED user must
    // still return 202. Any other status (400, 409, etc.) would reveal to an
    // attacker that the account exists and has been verified.
    assert_eq!(
        post(
            &router,
            "/api/auth/resend-verification",
            r#"{"email":"rvuser@example.com"}"#
        )
        .await,
        axum::http::StatusCode::ACCEPTED,
        "resend-verification must return 202 even when the user is already verified \
         (anti-enumeration: never reveal verified state)"
    );
}
