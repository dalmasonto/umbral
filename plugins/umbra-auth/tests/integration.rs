//! End-to-end coverage for the M9 v1 umbra-auth plugin.
//!
//! Boots a real `App` with [`AuthPlugin`] registered, applies the
//! `auth_user` table directly against an in-memory SQLite pool, and
//! exercises the M9 helper surface ([`hash_password`], [`verify_password`],
//! [`create_user`], [`authenticate`], [`set_password`]) against that pool.
//!
//! `umbra-core`'s ambient state (db pool, settings, migrate registry) is
//! published into process-wide `OnceLock`s by `App::build()`, so every
//! test in this file shares one boot driven through a
//! `tokio::sync::OnceCell`. The pattern mirrors
//! `crates/umbra-core/tests/plugin_contract.rs` and
//! `crates/umbra-core/tests/migrate.rs`.
//!
//! The `auth_user` table is created with a raw `CREATE TABLE`. The M7
//! `make_in` / `run_in` loop also handles this, but the helpers are what
//! these tests pin; a raw DDL keeps the setup tight and the assertions
//! focused on the helpers' behaviour.
//!
//! See `plugins/umbra-auth/src/lib.rs` for the surface under test and
//! `docs/specs/02-plugin-contract.md` "What shipped at M7 v1" for the
//! plugin contract this boot exercises.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbra_auth::{
    AuthError, AuthPlugin, AuthUser, authenticate, create_user, hash_password, set_password,
    verify_password,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");

        // sqlx's in-memory SQLite is per-connection: each connection
        // in the pool gets its own empty DB, so a CREATE TABLE on
        // one connection is invisible to queries that land on
        // another. Working around it with `cache=shared` or a
        // 1-connection pool turned out to be flaky under tokio's
        // multi-task parallelism (connection recycling tore down the
        // shared cache). A tempfile is the deterministic fix: every
        // pool connection sees the same on-disk file, and the OS
        // cleans it up when the TempDir drops. The file lives for
        // the test-binary's lifetime, which matches the shared-state
        // scope the auth helpers need.
        let tmp = tempfile::tempdir().expect("create tempdir for the test DB");
        let db_path = tmp.path().join("umbra_auth_integration.sqlite");
        // Leak the TempDir so its Drop doesn't fire mid-test and
        // delete the file under us. Test-only; the OS cleans /tmp
        // between boots.
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
            .plugin(AuthPlugin)
            .build()
            .expect("App::build should succeed with AuthPlugin");

        // Create the auth_user table directly. M7's migrate engine
        // would do this via `make_in` + `run_in` against a tempdir,
        // but the auth tests are testing the helpers, not the
        // migration loop. Raw CREATE TABLE keeps the setup tight.
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
    })
    .await;
}

/// Handle to the ambient SQLite pool. Every helper writes through this
/// pool; the tests below read it back to assert the side effects.
fn pool() -> sqlx::SqlitePool {
    umbra::db::pool()
}

// --------------------------------------------------------------------- //
// Hash / verify: pure tests. No DB, no boot.                             //
// --------------------------------------------------------------------- //

/// argon2 salts every call, so two hashes of the same plaintext must
/// differ. If they ever match, the salt isn't doing its job and stored
/// hashes leak whether two users share a password.
#[test]
fn hash_password_produces_different_hashes_for_same_plaintext() {
    let a = hash_password("hunter2").expect("argon2 should hash without erroring");
    let b = hash_password("hunter2").expect("argon2 should hash without erroring");
    assert_ne!(
        a, b,
        "argon2 must salt every call, but two hashes of `hunter2` matched: {a}",
    );
}

/// `verify_password` is the inverse of `hash_password`: the same
/// plaintext returns `Ok(true)`, a different plaintext returns
/// `Ok(false)`, and a malformed PHC string returns `Err`. The bool /
/// error split lets callers distinguish "wrong password" from "stored
/// hash is corrupt".
#[test]
fn verify_password_round_trip() {
    let hash = hash_password("hunter2").expect("argon2 should hash without erroring");

    assert!(
        verify_password("hunter2", &hash).expect("verify against valid hash should not error"),
        "the same plaintext should verify against its own hash",
    );
    assert!(
        !verify_password("wrong", &hash).expect("verify against valid hash should not error"),
        "a wrong plaintext should return Ok(false), not match",
    );

    let bad = verify_password("hunter2", "not-a-phc-string");
    assert!(
        matches!(bad, Err(AuthError::PasswordHash(_))),
        "a malformed hash should surface as AuthError::PasswordHash; got {bad:?}",
    );
}

// --------------------------------------------------------------------- //
// DB-backed tests. All share the BOOT OnceCell + the ambient pool.       //
// --------------------------------------------------------------------- //

/// `create_user` returns a populated [`AuthUser`] and writes a row to the
/// `auth_user` table. The returned struct must reflect the defaults from
/// the M9 spec: `is_active = true`, `is_staff = false`,
/// `is_superuser = false`, `last_login = None`, and a hash that is not
/// the plaintext password.
#[tokio::test]
async fn create_user_writes_to_the_database() {
    boot().await;

    let user = create_user("alice", "alice@example.com", "hunter2")
        .await
        .expect("create_user should succeed against the fresh auth_user table");

    assert_eq!(user.username, "alice");
    assert_eq!(user.email, "alice@example.com");
    assert_ne!(
        user.password_hash, "hunter2",
        "the stored hash must not equal the plaintext password",
    );
    assert!(
        !user.password_hash.is_empty(),
        "password_hash should be populated"
    );
    assert!(user.is_active, "new users default to is_active = true");
    assert!(!user.is_staff, "new users default to is_staff = false");
    assert!(
        !user.is_superuser,
        "new users default to is_superuser = false"
    );
    assert!(user.last_login.is_none(), "new users have no last_login");

    let row: (String, String, i64, i64, i64) = sqlx::query_as(
        "SELECT username, email, is_active, is_staff, is_superuser FROM auth_user WHERE username = ?",
    )
    .bind("alice")
    .fetch_one(&pool())
    .await
    .expect("the alice row should exist after create_user");
    assert_eq!(row.0, "alice");
    assert_eq!(row.1, "alice@example.com");
    assert_eq!(row.2, 1, "is_active should serialize to 1 in SQLite");
    assert_eq!(row.3, 0, "is_staff should serialize to 0 in SQLite");
    assert_eq!(row.4, 0, "is_superuser should serialize to 0 in SQLite");
}

/// `authenticate` returns the user on a username + correct-password
/// match. Uses a fresh `bob` user to keep this test independent of the
/// alice fixture in `create_user_writes_to_the_database`.
#[tokio::test]
async fn authenticate_returns_the_user_for_valid_credentials() {
    boot().await;

    let created = create_user("bob", "bob@example.com", "secret123")
        .await
        .expect("create_user should succeed for bob");

    let found = authenticate("bob", "secret123")
        .await
        .expect("authenticate should succeed for matching credentials");

    assert_eq!(
        found.id, created.id,
        "authenticate should return the same row"
    );
    assert_eq!(found.username, "bob");
    assert_eq!(found.email, "bob@example.com");
}

/// A wrong password against an existing user surfaces as
/// `AuthError::InvalidCredentials`. The error variant is the same as
/// `authenticate_rejects_unknown_username`; the caller can't tell from
/// the error which leg of the check failed.
#[tokio::test]
async fn authenticate_rejects_wrong_password() {
    boot().await;

    create_user("carol", "carol@example.com", "rightpass")
        .await
        .expect("create_user should succeed for carol");

    let result = authenticate("carol", "wrongpass").await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "wrong password must surface as InvalidCredentials; got {result:?}",
    );
}

/// An unknown username surfaces the same `AuthError::InvalidCredentials`
/// as a wrong password. The shared variant is intentional: a caller
/// can't enumerate accounts off the error alone.
#[tokio::test]
async fn authenticate_rejects_unknown_username() {
    boot().await;

    let result = authenticate("ghost", "anything").await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "unknown username must surface as InvalidCredentials; got {result:?}",
    );
}

/// `is_active = false` makes the user unauthenticatable even with the
/// right password. The spec's `authenticate` is a gate on active users
/// only; deactivated rows are filtered out so they can't log in.
#[tokio::test]
async fn authenticate_rejects_inactive_user() {
    boot().await;

    create_user("dave", "dave@example.com", "hunter2")
        .await
        .expect("create_user should succeed for dave");

    // Soft-disable the account directly through SQL so the test
    // doesn't depend on an unbuilt admin disable helper.
    sqlx::query("UPDATE auth_user SET is_active = 0 WHERE username = ?")
        .bind("dave")
        .execute(&pool())
        .await
        .expect("deactivation update should succeed");

    let result = authenticate("dave", "hunter2").await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "an inactive user must not authenticate; got {result:?}",
    );
}

/// `set_password` rotates the stored hash in place. The struct the
/// caller still holds reflects the new hash on success, the new
/// plaintext authenticates, and the old plaintext no longer does. The
/// in-place update means callers don't have to refetch.
#[tokio::test]
async fn set_password_updates_the_hash() {
    boot().await;

    let mut user = create_user("erin", "erin@example.com", "oldpass")
        .await
        .expect("create_user should succeed for erin");
    let original_hash = user.password_hash.clone();

    set_password(&mut user, "newpass")
        .await
        .expect("set_password should rotate the hash");

    assert_ne!(
        user.password_hash, original_hash,
        "set_password must update the in-place hash, but it stayed {original_hash}",
    );

    authenticate("erin", "newpass")
        .await
        .expect("the new password must authenticate after set_password");

    let stale = authenticate("erin", "oldpass").await;
    assert!(
        matches!(stale, Err(AuthError::InvalidCredentials)),
        "the old password must stop working after set_password; got {stale:?}",
    );
}

/// The `AuthPlugin` registers exactly one model (`AuthUser`) under the
/// `"auth"` plugin name. Proves the M7 per-plugin model walk picked up
/// what `AuthPlugin::models()` returned and routed it to the right slot
/// in the registry.
#[tokio::test]
async fn auth_plugin_registers_the_authuser_model() {
    boot().await;

    let models = umbra::migrate::models_for_plugin("auth");
    assert_eq!(
        models.len(),
        1,
        "AuthPlugin contributes exactly one model; got {models:?}",
    );
    assert_eq!(models[0].name, "AuthUser");
    assert_eq!(models[0].table, "auth_user");

    // Sanity guard: AuthUser is exposed as Model so the assertion
    // above is hitting the same surface plugin authors see.
    let _from_model: umbra::migrate::ModelMeta = umbra::migrate::ModelMeta::for_::<AuthUser>();
}
