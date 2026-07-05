//! TDD: lifecycle tests for the AuthChallenge helper surface.
//!
//! Boots a real in-memory SQLite pool (via a tempfile so pool connections share
//! one on-disk file), creates the `auth_user` and `auth_challenge` tables
//! directly with raw DDL (mirroring `integration.rs` boot), and exercises
//! `AuthChallenge::issue` → `find_active_for_user` → `bump_attempts` →
//! `mark_used` end-to-end.
//!
//! Unit assertions for `generate_code` and `generate_reset_token` (which are
//! `pub(crate)`) live inside `challenge.rs` in a `#[cfg(test)] mod tests`
//! block — integration tests can't see `pub(crate)` items.

use std::time::Duration;
use tokio::sync::OnceCell;
use umbral_auth::{AuthChallenge, AuthPlugin, AuthUser, challenge::PURPOSE_EMAIL_VERIFY};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        // Tempfile so every pool connection sees the same on-disk DB.
        // (In-memory SQLite is per-connection; a shared file avoids
        // cross-connection invisibility. See integration.rs for the
        // longer rationale.)
        let tmp = tempfile::tempdir().expect("create tempdir for the test DB");
        let db_path = tmp.path().join("umbral_challenge_lifecycle.sqlite");
        // Leak TempDir so the OS file stays alive for the test binary's
        // lifetime. OS cleans /tmp between boots.
        std::mem::forget(tmp);

        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let options = SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .expect("sqlite should connect against the tempfile");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .build()
            .expect("App::build should succeed with AuthPlugin");

        let pool = umbral::db::pool();

        // auth_user table — identical shape to integration.rs.
        sqlx::query(
            "CREATE TABLE auth_user (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                email TEXT NOT NULL,
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

        // auth_challenge table — columns match the AuthChallenge struct.
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
}

/// Insert a minimal AuthUser row and return its id.
async fn seed_user(username: &str, email: &str) -> i64 {
    let pool = umbral::db::pool();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query_scalar(
        "INSERT INTO auth_user
            (username, email, password_hash, is_active, is_staff, is_superuser, date_joined)
            VALUES (?, ?, ?, 1, 0, 0, ?)
            RETURNING id",
    )
    .bind(username)
    .bind(email)
    .bind("$argon2id$v=19$m=19456,t=2,p=1$hash_placeholder")
    .bind(&now)
    .fetch_one(&pool)
    .await
    .expect("seed_user: insert should succeed")
}

/// Full issue → find → bump_attempts → mark_used lifecycle.
#[tokio::test]
async fn issue_then_find_then_consume_code() {
    boot().await;
    let user_id = seed_user("alice", "alice@example.com").await;

    // Issue a code-style challenge with a known plaintext.
    let _c = AuthChallenge::issue(
        user_id,
        PURPOSE_EMAIL_VERIFY,
        "483920",
        Duration::from_secs(900),
    )
    .await
    .unwrap();

    // Found by (user, purpose) and live.
    let found = AuthChallenge::find_active_for_user(user_id, PURPOSE_EMAIL_VERIFY)
        .await
        .unwrap()
        .expect("should find the issued challenge");
    assert!(
        found.is_live(),
        "challenge should be live right after issue"
    );
    assert_eq!(found.attempts, 0);

    // Wrong guess bumps attempts.
    found.bump_attempts().await.unwrap();
    let again = AuthChallenge::find_active_for_user(user_id, PURPOSE_EMAIL_VERIFY)
        .await
        .unwrap()
        .expect("should still find challenge after bump_attempts");
    assert_eq!(again.attempts, 1);

    // Consume: marked used → no longer live / not returned as active.
    again.mark_used().await.unwrap();
    let gone = AuthChallenge::find_active_for_user(user_id, PURPOSE_EMAIL_VERIFY)
        .await
        .unwrap();
    assert!(
        gone.is_none(),
        "after mark_used the challenge must not appear as active"
    );
}

/// Lookup by secret hash: `find_active_by_secret` must return the row when
/// the plaintext matches and return None after `mark_used`.
#[tokio::test]
async fn find_active_by_secret_matches_and_disappears_after_consume() {
    boot().await;
    let user_id = seed_user("bob", "bob@example.com").await;
    let plaintext = "999111";
    AuthChallenge::issue(
        user_id,
        PURPOSE_EMAIL_VERIFY,
        plaintext,
        Duration::from_secs(300),
    )
    .await
    .unwrap();

    // Correct plaintext finds the row.
    let found = AuthChallenge::find_active_by_secret(plaintext, PURPOSE_EMAIL_VERIFY)
        .await
        .unwrap()
        .expect("find_active_by_secret must return the row for a correct plaintext");
    assert!(found.is_live());

    // Wrong plaintext returns None.
    let nope = AuthChallenge::find_active_by_secret("000000", PURPOSE_EMAIL_VERIFY)
        .await
        .unwrap();
    assert!(nope.is_none(), "wrong plaintext must not match");

    // After mark_used, the correct plaintext also returns None.
    found.mark_used().await.unwrap();
    let gone = AuthChallenge::find_active_by_secret(plaintext, PURPOSE_EMAIL_VERIFY)
        .await
        .unwrap();
    assert!(
        gone.is_none(),
        "find_active_by_secret must return None after mark_used"
    );
}
