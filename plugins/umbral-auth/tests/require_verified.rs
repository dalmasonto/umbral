//! TDD: require_verified_email enforcement — flag ON.
//!
//! Boots one App with `AuthPlugin::require_verified_email()` and a recording
//! mailer. Assertions:
//!   - register → 201 AND a verification mail is auto-sent.
//!   - login before verify → 403 (email_not_verified).
//!   - POST /api/auth/verify-email with the code from the recorder → 204.
//!   - login after verify → 200.
//!
//! Must live in its own test binary (separate from `require_verified_off.rs`)
//! because `REQUIRE_VERIFIED` is a process-global `OnceLock`: one App per
//! binary, first boot wins, subsequent `set` calls are silent no-ops.

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
    /// Most-recently-captured mail sent to `email`, or None.
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
// One-time App boot — process-global OnceLocks can only be set once.
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();
static RECORDER: std::sync::OnceLock<Recorder> = std::sync::OnceLock::new();
static ROUTER: std::sync::OnceLock<Router> = std::sync::OnceLock::new();

async fn boot_app_required() -> (Router, Recorder) {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("create tempdir for require_verified test DB");
        let db_path = tmp.path().join("umbral_require_verified.sqlite");
        std::mem::forget(tmp); // keep alive for the binary's lifetime

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
        RECORDER.set(rec.clone()).ok();

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(umbral_sessions::SessionsPlugin::default().without_auto_layer())
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    .with_default_routes()
                    .disable_password_validation()
                    .disable_throttle()
                    .mailer(rec)
                    .require_verified_email(),
            )
            .build()
            .expect("App::build should succeed with require_verified_email");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let router = app.into_router();
        ROUTER.set(router).ok();
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

/// Full verified-email gate cycle:
///   register → mail auto-sent → login blocked (403) → verify → login allowed (200).
#[tokio::test]
async fn unverified_login_blocked_and_unblocked_after_verify() {
    let (router, rec) = boot_app_required().await;

    // --- Register: must return 201 AND auto-send a verification mail. ---
    assert_eq!(
        post(
            &router,
            "/api/auth/register",
            r#"{"username":"verifytest","email":"verifytest@example.com","password":"G00d$Pass!"}"#,
        )
        .await,
        axum::http::StatusCode::CREATED,
        "register must return 201"
    );

    // The recorder must have captured a verification mail (auto-sent on register).
    let mail = rec
        .last_to("verifytest@example.com")
        .expect("register must auto-send a verification email when require_verified_email is on");
    let code: String = mail.text.chars().filter(|c| c.is_ascii_digit()).collect();
    assert_eq!(
        code.len(),
        6,
        "verification email must contain exactly 6 ASCII digits; body was: {}",
        mail.text
    );

    // --- Login BEFORE verifying → 403. ---
    assert_eq!(
        post(
            &router,
            "/api/auth/login",
            r#"{"username":"verifytest","password":"G00d$Pass!"}"#,
        )
        .await,
        axum::http::StatusCode::FORBIDDEN,
        "login before email verification must return 403 when require_verified_email is on"
    );

    // --- Verify email via the code captured by the recorder. ---
    assert_eq!(
        post(
            &router,
            "/api/auth/verify-email",
            &format!(r#"{{"email":"verifytest@example.com","code":"{code}"}}"#),
        )
        .await,
        axum::http::StatusCode::NO_CONTENT,
        "verify-email with the correct code must return 204"
    );

    // --- Login AFTER verifying → 200. ---
    assert_eq!(
        post(
            &router,
            "/api/auth/login",
            r#"{"username":"verifytest","password":"G00d$Pass!"}"#,
        )
        .await,
        axum::http::StatusCode::OK,
        "login after email verification must return 200"
    );
}
