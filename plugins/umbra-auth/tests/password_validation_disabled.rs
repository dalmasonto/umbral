//! Secure-by-default has an explicit escape hatch:
//! [`AuthPlugin::disable_password_validation`]. This test lives in its OWN
//! test binary (separate process ⇒ a fresh process-global `PASSWORD_POLICY`
//! `OnceLock`) so it can boot an `AuthPlugin` with validation turned off and
//! prove a weak password sails through `create_user`.
//!
//! It MUST be a separate file from `password_validation.rs`: that file boots
//! the default secure policy into the same `OnceLock`, and the first install
//! wins. Two policies can't coexist in one process.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbra_auth::{AuthPlugin, AuthUser, create_user};

/// With `disable_password_validation()`, the weak password `"a"` — which
/// every default validator rejects — is accepted and persisted. This is the
/// opt-OUT contract: an app that explicitly asks for no policy gets none.
#[tokio::test]
async fn disabled_validation_accepts_weak_password() {
    let settings = umbra::Settings::from_env().expect("figment defaults load in a test env");

    let tmp = tempfile::tempdir().expect("create tempdir for the test DB");
    let db_path = tmp.path().join("umbra_auth_pwdisabled.sqlite");
    std::mem::forget(tmp);
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .expect("sqlite should connect against the tempfile");

    umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        // The explicit opt-OUT: no password validation.
        .plugin(AuthPlugin::<AuthUser>::default().disable_password_validation())
        .build()
        .expect("App::build should succeed");

    let pool = umbra::db::pool();
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
            last_login TEXT
        )",
    )
    .execute(&pool)
    .await
    .expect("create auth_user table");

    let user = create_user("anyone", "anyone@example.com", "a")
        .await
        .expect("with validation disabled, even `a` must be accepted");
    assert_eq!(user.username, "anyone");
}
