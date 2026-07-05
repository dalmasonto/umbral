//! TDD: password forgot/reset core flow.
//!
//! `start_password_reset` issues a tokenized link (1-hour TTL), renders
//! `auth/email/reset_link.{html,txt}`, and sends via the ambient mailer.
//! `reset_password` consumes the token, enforces the password-strength
//! policy, atomically updates the hash + marks the challenge used, then
//! revokes bearer tokens and sessions best-effort after the commit.
//!
//! Boot creates auth_user, auth_challenge, auth_token, and session tables.
//! The Recorder captures every OutgoingMail so the test can extract the
//! token from the rendered body and assert end-to-end.

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
    /// Most-recently-captured mail, or None if nothing sent yet.
    fn last(&self) -> Option<OutgoingMail> {
        self.0.lock().unwrap().last().cloned()
    }

    /// All captured mails in order sent.
    fn all(&self) -> Vec<OutgoingMail> {
        self.0.lock().unwrap().clone()
    }
}

// =========================================================================
// One-time App boot (process-wide OnceLocks can only be set once per test
// binary — share via a static Arc so multiple test fns re-use the same boot).
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();
static RECORDER: std::sync::OnceLock<Recorder> = std::sync::OnceLock::new();

async fn boot_with_recorder() -> Recorder {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        // Tempfile DB — every pool connection sees the same on-disk file.
        // (In-memory SQLite is per-connection; a shared file is the safe fix.)
        let tmp = tempfile::tempdir().expect("create tempdir for password_reset test DB");
        let db_path = tmp.path().join("umbral_password_reset.sqlite");
        std::mem::forget(tmp); // keep the file alive for the test binary's lifetime

        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        let rec = Recorder::default();
        RECORDER.set(rec.clone()).ok();

        // Boot with default password policy (NOT disabled) so weak-password
        // rejection is exercised. Throttle off to avoid rate-limit interference.
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(
                AuthPlugin::<AuthUser>::default()
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

        // session table — required by umbral_sessions::revoke_user_sessions.
        sqlx::query(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                user_id TEXT,
                data TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create session table");
    })
    .await;

    RECORDER.get().expect("RECORDER set during boot").clone()
}

// =========================================================================
// Tests
// =========================================================================

#[tokio::test]
async fn reset_flow_changes_password_and_revokes_tokens() {
    let rec = boot_with_recorder().await;
    let user = umbral_auth::create_user("carol", "carol@example.com", "Old$Passw0rd")
        .await
        .unwrap();
    // Give her a bearer token and a session.
    let (_t, _pt) = umbral_auth::token::AuthToken::create_for(&user, "laptop")
        .await
        .unwrap();
    umbral_sessions::login_user_id(
        &http::HeaderMap::new(),
        &mut http::HeaderMap::new(),
        Some(user.id.to_string()),
    )
    .await
    .unwrap();

    // Unknown email is a silent success (no enumeration), sends nothing to it.
    umbral_auth::start_password_reset("nobody@example.com", "https://app/reset")
        .await
        .unwrap();
    assert!(
        rec.all().iter().all(|m| m.to != "nobody@example.com"),
        "start_password_reset for unknown email must not send any mail to that address"
    );

    umbral_auth::start_password_reset("carol@example.com", "https://app/reset")
        .await
        .unwrap();
    let mail = rec.last().expect("reset email should have been sent");
    assert_eq!(mail.to, "carol@example.com");
    // Extract token from the URL in the text body.
    let token = mail
        .text
        .split("token=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(
        token.starts_with("umbral_"),
        "extracted token must have the umbral_ prefix; got {token:?}"
    );
    // A custom mailer also receives the semantic kind + the full reset URL,
    // so it can build its own message (e.g. a provider template) per type.
    match &mail.kind {
        umbral_auth::MailKind::PasswordReset { reset_url } => {
            assert!(
                reset_url.starts_with("https://app/reset?token=") && reset_url.contains(&token),
                "MailKind reset_url must be the full tokenized link; got {reset_url:?}"
            );
        }
        other => panic!("expected MailKind::PasswordReset, got {other:?}"),
    }

    // Weak password is rejected.
    assert!(
        umbral_auth::reset_password(&token, "123").await.is_err(),
        "weak password must be rejected"
    );
    // Strong password succeeds.
    umbral_auth::reset_password(&token, "Br4nd-New$Pass")
        .await
        .unwrap();

    // New password authenticates; old does not.
    assert!(
        umbral_auth::authenticate::<AuthUser>("carol", "Br4nd-New$Pass")
            .await
            .is_ok(),
        "new password must authenticate"
    );
    assert!(
        umbral_auth::authenticate::<AuthUser>("carol", "Old$Passw0rd")
            .await
            .is_err(),
        "old password must no longer authenticate"
    );

    // Bearer tokens revoked.
    let tok_count = umbral_auth::token::AuthToken::objects()
        .filter(umbral_auth::token::auth_token::USER_ID.eq(user.id))
        .count()
        .await
        .unwrap();
    assert_eq!(
        tok_count, 0,
        "bearer tokens must be revoked on password reset"
    );

    // Single-use: token can't be reused.
    assert!(
        umbral_auth::reset_password(&token, "An0ther$Pass!")
            .await
            .is_err(),
        "a consumed reset token must not be accepted a second time"
    );
}
