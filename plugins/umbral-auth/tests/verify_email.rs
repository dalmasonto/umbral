//! TDD: email-verification core flow.
//!
//! Boots a real App with AuthPlugin wired to a recording mailer, creates
//! the auth tables directly via raw DDL (same pattern as integration.rs),
//! then drives the full `start_email_verification` → `verify_email` cycle.
//!
//! The Recorder captures every `OutgoingMail` sent during the test so we
//! can extract the 6-digit code from the rendered `.text` body and assert
//! the round-trip works end-to-end.

use std::sync::{Arc, Mutex};
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
    /// Return the most-recently-captured mail, or None if nothing sent yet.
    fn last(&self) -> Option<OutgoingMail> {
        self.0.lock().unwrap().last().cloned()
    }
}

// =========================================================================
// One-time App boot (OnceCell so the process-wide OnceLocks aren't
// re-set on the second test). The Recorder is shared across the cell init
// via a static Arc; tests access it through `RECORDER`.
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();
static RECORDER: std::sync::OnceLock<Recorder> = std::sync::OnceLock::new();

async fn boot_with_recorder() -> Recorder {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        // Tempfile DB so all pool connections share one on-disk file.
        let tmp = tempfile::tempdir().expect("create tempdir for verify_email test DB");
        let db_path = tmp.path().join("umbral_verify_email.sqlite");
        std::mem::forget(tmp); // keep file alive for the test binary's lifetime

        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let options = SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .expect("sqlite should connect against the tempfile");

        let rec = Recorder::default();
        RECORDER.set(rec.clone()).ok();

        // AuthPlugin templates_dirs() contributes the auth template directory
        // so the engine can resolve auth/email/verify_code.{html,txt}.
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    .disable_password_validation()
                    .disable_throttle()
                    .mailer(rec),
            )
            .build()
            .expect("App::build should succeed with AuthPlugin + Recorder mailer");

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
    })
    .await;

    RECORDER.get().expect("RECORDER set during boot").clone()
}

// =========================================================================
// Tests
// =========================================================================

#[tokio::test]
async fn verify_email_happy_path_and_wrong_code() {
    let rec = boot_with_recorder().await;
    let user = umbral_auth::create_user("bob", "bob@example.com", "Sup3r$ecret!")
        .await
        .unwrap();
    assert!(user.email_verified_at.is_none());

    umbral_auth::start_email_verification(&user).await.unwrap();

    // The recorder captured exactly one mail to bob; extract the 6-digit code.
    let mail = rec
        .last()
        .expect("a verification email should have been sent");
    assert_eq!(mail.to, "bob@example.com");
    let code: String = mail.text.chars().filter(|c| c.is_ascii_digit()).collect();
    assert_eq!(
        code.len(),
        6,
        "email body must contain exactly 6 ASCII digits (the verification code); body was: {}",
        mail.text
    );
    // A custom mailer also receives the semantic kind + raw code, so it can
    // build its own message instead of using the rendered body. The code in
    // `kind` must match the one in the rendered text.
    match &mail.kind {
        umbral_auth::MailKind::EmailVerification { code: kind_code } => {
            assert_eq!(
                *kind_code, code,
                "MailKind code must match the rendered code"
            );
        }
        other => panic!("expected MailKind::EmailVerification, got {other:?}"),
    }

    // Wrong code fails generically; correct code verifies.
    assert!(
        umbral_auth::verify_email("bob@example.com", "000000")
            .await
            .is_err()
            || code == "000000",
        "wrong code must return Err"
    );
    umbral_auth::verify_email("bob@example.com", &code)
        .await
        .unwrap();

    let reloaded = umbral_auth::AuthUser::objects()
        .filter(umbral_auth::auth_user::EMAIL.eq("bob@example.com".to_string()))
        .first()
        .await
        .unwrap()
        .unwrap();
    assert!(
        reloaded.email_verified_at.is_some(),
        "email must be marked verified after a correct code is submitted"
    );

    // Single-use: the same code can't verify twice (challenge is marked used).
    assert!(
        umbral_auth::verify_email("bob@example.com", &code)
            .await
            .is_err(),
        "a used challenge must not be accepted a second time"
    );
}

#[tokio::test]
async fn verify_email_unknown_email_returns_err() {
    boot_with_recorder().await;

    let result = umbral_auth::verify_email("nobody@example.com", "123456").await;
    assert!(
        matches!(result, Err(umbral_auth::AuthError::InvalidChallenge)),
        "unknown email must return InvalidChallenge (no account enumeration); got {result:?}"
    );
}

#[tokio::test]
async fn verify_email_no_active_challenge_returns_err() {
    boot_with_recorder().await;

    // Create a user but do NOT issue a challenge.
    let _user = umbral_auth::create_user("carol", "carol@example.com", "Sup3r$ecret!")
        .await
        .unwrap();

    let result = umbral_auth::verify_email("carol@example.com", "123456").await;
    assert!(
        matches!(result, Err(umbral_auth::AuthError::InvalidChallenge)),
        "no active challenge must return InvalidChallenge (no account enumeration); got {result:?}"
    );
}

#[tokio::test]
async fn verify_email_brute_force_burns_challenge() {
    let rec = boot_with_recorder().await;

    let user = umbral_auth::create_user("dave", "dave@example.com", "Sup3r$ecret!")
        .await
        .unwrap();
    assert!(user.email_verified_at.is_none());

    umbral_auth::start_email_verification(&user).await.unwrap();

    // Extract the real code from the recorder.
    let mail = rec
        .last()
        .expect("a verification email should have been sent to dave");
    let code: String = mail.text.chars().filter(|c| c.is_ascii_digit()).collect();
    assert_eq!(
        code.len(),
        6,
        "email body must contain exactly 6 ASCII digits; body was: {}",
        mail.text
    );

    // Pick a wrong code that is definitely not the real code.
    let wrong_code = if code.as_str() != "000000" {
        "000000"
    } else {
        "111111"
    };

    // Submit 5 wrong codes — this must exhaust the attempt cap (MAX_CODE_ATTEMPTS = 5).
    for i in 0..5 {
        let err = umbral_auth::verify_email("dave@example.com", wrong_code)
            .await
            .unwrap_err();
        assert!(
            matches!(err, umbral_auth::AuthError::InvalidChallenge),
            "wrong-code attempt {i} must return InvalidChallenge; got {err:?}"
        );
    }

    // Now submit the CORRECT code. The challenge was burned by the attempt cap,
    // so verification must still fail — proving the brute-force guard works.
    let result = umbral_auth::verify_email("dave@example.com", &code).await;
    assert!(
        matches!(result, Err(umbral_auth::AuthError::InvalidChallenge)),
        "correct code after 5 failed attempts must still be InvalidChallenge \
         (challenge burned by attempt cap); got {result:?}"
    );

    // The user must remain unverified — email_verified_at must not have been stamped.
    let reloaded = umbral_auth::AuthUser::objects()
        .filter(umbral_auth::auth_user::EMAIL.eq("dave@example.com".to_string()))
        .first()
        .await
        .unwrap()
        .unwrap();
    assert!(
        reloaded.email_verified_at.is_none(),
        "email_verified_at must remain None after challenge was burned by brute-force guard"
    );
}
