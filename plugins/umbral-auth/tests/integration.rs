//! End-to-end coverage for the M9 v1 umbral-auth plugin.
//!
//! Boots a real `App` with [`AuthPlugin`] registered, derives the schema from
//! the registered models against a temp SQLite pool, and
//! exercises the M9 helper surface ([`hash_password`], [`verify_password`],
//! [`create_user`], [`authenticate`], [`set_password`]) against that pool.
//!
//! `umbral-core`'s ambient state (db pool, settings, migrate registry) is
//! published into process-wide `OnceLock`s by `App::build()`, so every
//! test in this file shares one boot driven through a
//! `tokio::sync::OnceCell`. The pattern mirrors
//! `crates/umbral-core/tests/plugin_contract.rs` and
//! `crates/umbral-core/tests/migrate.rs`.
//!
//! The schema is derived from the MODELS by `migrate::create_tables_for_tests`,
//! so it cannot drift from them. It used to be a hand-written `CREATE TABLE`,
//! which had quietly omitted the `UNIQUE` that `AuthUser::email` declares — the
//! suite was proving things against a schema laxer than production's.
//!
//! See `plugins/umbral-auth/src/lib.rs` for the surface under test and
//! `docs/specs/02-plugin-contract.md` "What shipped at M7 v1" for the
//! plugin contract this boot exercises.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral_auth::{
    AuthError, AuthPlugin, AuthUser, authenticate, create_user, hash_password,
    random_password_hash, set_password, verify_password,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

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
        let db_path = tmp.path().join("umbral_auth_integration.sqlite");
        // Leak the TempDir so its Drop doesn't fire mid-test and
        // delete the file under us. Test-only; the OS cleans /tmp
        // between boots.
        std::mem::forget(tmp);
        let options = SqliteConnectOptions::new()
            .busy_timeout(std::time::Duration::from_secs(5))
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

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

/// Handle to the ambient SQLite pool. Every helper writes through this
/// pool; the tests below read it back to assert the side effects.
fn pool() -> sqlx::SqlitePool {
    umbral::db::pool()
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

    let user = create_user("alice", "alice@example.com", "Tr0ub4dour&3xpl")
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

    let created = create_user("bob", "bob@example.com", "Zephyr!Qu14-Knight")
        .await
        .expect("create_user should succeed for bob");

    let found = authenticate::<AuthUser>("bob", "Zephyr!Qu14-Knight")
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

    let result = authenticate::<AuthUser>("carol", "wrongpass").await;
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

    let result = authenticate::<AuthUser>("ghost", "anything").await;
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

    create_user("dave", "dave@example.com", "Br1ghtMoon#0723")
        .await
        .expect("create_user should succeed for dave");

    // Soft-disable the account directly through SQL so the test
    // doesn't depend on an unbuilt admin disable helper.
    sqlx::query("UPDATE auth_user SET is_active = 0 WHERE username = ?")
        .bind("dave")
        .execute(&pool())
        .await
        .expect("deactivation update should succeed");

    let result = authenticate::<AuthUser>("dave", "Br1ghtMoon#0723").await;
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

    let mut user = create_user("erin", "erin@example.com", "Stout$Wombat-58")
        .await
        .expect("create_user should succeed for erin");
    let original_hash = user.password_hash.clone();

    set_password(&mut user, "Clay#Harbor-90")
        .await
        .expect("set_password should rotate the hash");

    assert_ne!(
        user.password_hash, original_hash,
        "set_password must update the in-place hash, but it stayed {original_hash}",
    );

    authenticate::<AuthUser>("erin", "Clay#Harbor-90")
        .await
        .expect("the new password must authenticate after set_password");

    let stale = authenticate::<AuthUser>("erin", "Stout$Wombat-58").await;
    assert!(
        matches!(stale, Err(AuthError::InvalidCredentials)),
        "the old password must stop working after set_password; got {stale:?}",
    );
}

/// The `AuthPlugin` registers `AuthUser` and `AuthToken` under the
/// `"auth"` plugin name when `U = AuthUser` (the default). Custom
/// user models (`AuthPlugin::<MyUser>::default()`) only get their
/// own user table — `AuthToken` is hard-bound to `AuthUser` and
/// brings its own bearer-auth backend.
#[tokio::test]
async fn auth_plugin_registers_the_authuser_model() {
    boot().await;

    let models = umbral::migrate::models_for_plugin("auth");
    let tables: Vec<&str> = models.iter().map(|m| m.table.as_str()).collect();
    assert!(
        tables.contains(&"auth_user"),
        "AuthPlugin must register auth_user; got {tables:?}",
    );
    assert!(
        tables.contains(&"auth_token"),
        "AuthPlugin must register auth_token alongside auth_user; got {tables:?}",
    );
    assert!(
        tables.contains(&"auth_challenge"),
        "AuthPlugin must register auth_challenge alongside auth_user; got {tables:?}",
    );
    assert_eq!(
        models.len(),
        3,
        "AuthPlugin contributes exactly three models (auth_user + auth_token + auth_challenge); got {models:?}",
    );

    // Sanity guard: all three types are exposed as Model so the assertion
    // above is hitting the same surface plugin authors see.
    let _from_user: umbral::migrate::ModelMeta = umbral::migrate::ModelMeta::for_::<AuthUser>();
    let _from_token: umbral::migrate::ModelMeta =
        umbral::migrate::ModelMeta::for_::<umbral_auth::AuthToken>();
    let _from_challenge: umbral::migrate::ModelMeta =
        umbral::migrate::ModelMeta::for_::<umbral_auth::AuthChallenge>();
}

/// End-to-end dispatch of `createsuperuser --noinput` through
/// `umbral::cli::dispatch`. Proves the `Plugin::commands()` hook
/// returns the `CreateSuperuserCommand`, that dispatch routes the
/// args to it, and that the resulting row carries the staff +
/// superuser flags.
///
/// Password comes from `UMBRAL_SUPERUSER_PASSWORD` so the test runs
/// without a TTY; username + email from `--username` / `--email`
/// for the same reason. Mirrors what a CI / container superuser
/// bootstrap would look like in production.
/// Serialises the two tests that mutate the process-global
/// `UMBRAL_SUPERUSER_PASSWORD` env var (gaps2 #52) so they can't race —
/// one sets it, the other asserts it's absent. Held for the whole
/// env-dependent body of each test.
static SUPERUSER_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn dispatch_routes_createsuperuser_command_with_noinput() {
    boot().await;
    let _env_guard = SUPERUSER_ENV_LOCK.lock().await;

    // Setup: the auth_user table was created earlier in `boot()`. We
    // need a clean slate so the username uniqueness constraint
    // doesn't trip from another test.
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM auth_user WHERE username = 'admin'")
        .execute(&pool)
        .await
        .expect("clean slate");

    // SAFETY: tests in this binary that touch this env var must
    // serialise; current test count is small enough that the noinput
    // path is the only env-driven test, so a plain set+remove is
    // safe.
    unsafe {
        std::env::set_var("UMBRAL_SUPERUSER_PASSWORD", "swordfish-9-9");
    }

    let plugins: Vec<Box<dyn umbral::prelude::Plugin>> = vec![Box::new(umbral_auth::AuthPlugin::<
        umbral_auth::AuthUser,
    >::default())];
    let outcome = umbral::cli::dispatch(
        &plugins,
        vec![
            "umbral-cli",
            "createsuperuser",
            "--username",
            "admin",
            "--email",
            "admin@example.com",
            "--noinput",
        ],
    )
    .await
    .expect("dispatch ok");
    match outcome {
        umbral::cli::DispatchOutcome::Matched(name) => {
            assert_eq!(name, "createsuperuser");
        }
        other => panic!("expected Matched(createsuperuser); got {other:?}"),
    }

    unsafe {
        std::env::remove_var("UMBRAL_SUPERUSER_PASSWORD");
    }

    // The user landed in the DB with the right flags. Read it back
    // through the authenticate helper so we also verify the password
    // hash round-trips.
    let user = umbral_auth::authenticate::<umbral_auth::AuthUser>("admin", "swordfish-9-9")
        .await
        .expect("authenticate");
    assert_eq!(user.username, "admin");
    assert_eq!(user.email, "admin@example.com");
    assert!(user.is_staff, "createsuperuser must set is_staff = true");
    assert!(
        user.is_superuser,
        "createsuperuser must set is_superuser = true"
    );
    assert!(user.is_active, "the new user should be active");
}

/// `--noinput` without `UMBRAL_SUPERUSER_PASSWORD` set fails loudly
/// rather than prompting (the whole point of the flag — CI safety).
#[tokio::test]
async fn createsuperuser_noinput_errors_without_password_env() {
    boot().await;
    let _env_guard = SUPERUSER_ENV_LOCK.lock().await;

    // Make sure the var isn't accidentally set from the previous test.
    unsafe {
        std::env::remove_var("UMBRAL_SUPERUSER_PASSWORD");
    }

    let plugins: Vec<Box<dyn umbral::prelude::Plugin>> = vec![Box::new(umbral_auth::AuthPlugin::<
        umbral_auth::AuthUser,
    >::default())];
    let result = umbral::cli::dispatch(
        &plugins,
        vec![
            "umbral-cli",
            "createsuperuser",
            "--username",
            "ghost",
            "--email",
            "ghost@example.com",
            "--noinput",
        ],
    )
    .await;
    let err = result.expect_err("dispatch should err when password isn't supplied");
    let msg = err.to_string();
    assert!(
        msg.contains("password not provided") || msg.contains("UMBRAL_SUPERUSER_PASSWORD"),
        "expected password-missing error; got: {msg}"
    );
}

// =========================================================================
// Case-insensitive identifiers: usernames + emails are stored and matched
// in a canonical (trimmed + lowercased) form, so `Dalmasonto` and
// `dalmasonto` are the SAME account and either case logs in. Framework fix
// requested 2026-07-07 — a user could previously register both
// dalmasogembo@gmail.com/dalmasonto AND Dalmasogembo@gmail.com/Dalmasonto.
// =========================================================================

/// `create_user` lowercases (and trims) both identifiers on write, so the
/// returned struct AND the persisted row are canonical.
#[tokio::test]
async fn create_user_normalizes_username_and_email_to_lowercase() {
    boot().await;

    // Its own email, not `dave@example.com`: the model declares
    // `#[umbral(unique)]` on `email`, and now that the test schema is derived from
    // the model that UNIQUE is actually enforced — `authenticate_rejects_inactive_user`
    // already owns `dave@example.com` in this shared database. The hand-written test
    // table used to omit the constraint, so this collision passed unnoticed against a
    // schema laxer than production's.
    let user = create_user(
        "  MixedCase_Dave  ",
        "MixedCase_Dave@Example.COM",
        "Tr0ub4dour&3xpl-dave",
    )
    .await
    .expect("create_user should succeed");

    assert_eq!(
        user.username, "mixedcase_dave",
        "username is trimmed + lowercased on the returned struct"
    );
    assert_eq!(
        user.email, "mixedcase_dave@example.com",
        "email is trimmed + lowercased on the returned struct"
    );

    // The persisted row is canonical too — a lookup by the lowercased key finds it.
    let row: (String, String) =
        sqlx::query_as("SELECT username, email FROM auth_user WHERE username = ?")
            .bind("mixedcase_dave")
            .fetch_one(&pool())
            .await
            .expect("row stored under the lowercased username");
    assert_eq!(row.0, "mixedcase_dave");
    assert_eq!(row.1, "mixedcase_dave@example.com");
}

/// A second signup that differs from an existing account only by case is
/// rejected by the `#[umbral(unique)]` constraint — because both rows
/// normalize to the same stored value. This is the reported bug.
#[tokio::test]
async fn duplicate_signup_differing_only_by_case_is_rejected() {
    boot().await;

    create_user(
        "dalmasonto",
        "dalmasogembo@gmail.com",
        "Zephyr!Qu14-Knight-1",
    )
    .await
    .expect("first signup succeeds");

    // Same identifiers, different case — must collide, not create a twin account.
    let dup = create_user(
        "Dalmasonto",
        "Dalmasogembo@Gmail.com",
        "Zephyr!Qu14-Knight-2",
    )
    .await;

    assert!(
        matches!(
            dup,
            Err(AuthError::Write(
                umbral::orm::write::WriteError::UniqueViolation { .. }
            ))
        ),
        "a case-only-different re-signup must be rejected as a uniqueness violation; got {dup:?}"
    );

    // And there is exactly ONE row, not two.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM auth_user WHERE username = 'dalmasonto' OR username = 'Dalmasonto'",
    )
    .fetch_one(&pool())
    .await
    .expect("count query");
    assert_eq!(count, 1, "only the canonical row exists");
}

/// Login is case-insensitive on the username: an account stored lowercased
/// authenticates when the caller types a different case.
#[tokio::test]
async fn authenticate_is_case_insensitive_on_username() {
    boot().await;

    let created = create_user("caseuser", "caseuser@example.com", "R1ght-Pass-Phrase!")
        .await
        .expect("create_user succeeds");

    // Type the username in a case that doesn't match the stored form.
    let found = authenticate::<AuthUser>("CaseUser", "R1ght-Pass-Phrase!")
        .await
        .expect("authenticate must match case-insensitively");

    assert_eq!(found.id, created.id, "same row found regardless of case");
    assert_eq!(found.username, "caseuser");

    // Leading/trailing whitespace in the typed username is also tolerated.
    let trimmed = authenticate::<AuthUser>("  caseuser ", "R1ght-Pass-Phrase!")
        .await
        .expect("authenticate trims the typed username");
    assert_eq!(trimmed.id, created.id);
}

// =========================================================================
// Login by email OR username (UserModel::login_columns)
//
// AuthUser overrides login_columns() to ["username", "email"], so a user can
// authenticate with either identifier. The match is case-insensitive (both
// columns are stored trimmed + lowercased and the identifier is normalized).
// =========================================================================

#[tokio::test]
async fn authenticate_accepts_either_username_or_email() {
    boot().await;

    let created = create_user("loginboth", "loginboth@example.com", "R1ght-Pass-Phrase!")
        .await
        .expect("create_user succeeds");

    // By username (historical path).
    let by_username = authenticate::<AuthUser>("loginboth", "R1ght-Pass-Phrase!")
        .await
        .expect("login by username");
    assert_eq!(by_username.id, created.id);

    // By email — the new path.
    let by_email = authenticate::<AuthUser>("loginboth@example.com", "R1ght-Pass-Phrase!")
        .await
        .expect("login by email must resolve the same row");
    assert_eq!(
        by_email.id, created.id,
        "email identifier finds the same user"
    );

    // Email match is case-insensitive (stored lowercased, identifier normalized).
    let by_email_cased = authenticate::<AuthUser>("LoginBoth@Example.COM", "R1ght-Pass-Phrase!")
        .await
        .expect("login by email is case-insensitive");
    assert_eq!(by_email_cased.id, created.id);

    // A wrong identifier that matches neither column still fails.
    let miss = authenticate::<AuthUser>("nobody@example.com", "R1ght-Pass-Phrase!").await;
    assert!(matches!(miss, Err(AuthError::InvalidCredentials)));
}

/// The active-flag guard survives the OR predicate: an inactive user must not
/// authenticate even when the identifier matches. Guards against a mis-grouped
/// `(username = X OR email = X) AND is_active` predicate degrading to
/// `username = X OR (email = X AND is_active)`.
#[tokio::test]
async fn authenticate_by_email_still_respects_inactive_flag() {
    boot().await;
    let pool = pool();

    create_user(
        "inactivemail",
        "inactivemail@example.com",
        "R1ght-Pass-Phrase!",
    )
    .await
    .expect("create");
    sqlx::query("UPDATE auth_user SET is_active = 0 WHERE username = 'inactivemail'")
        .execute(&pool)
        .await
        .expect("deactivate");

    // Neither identifier authenticates an inactive account.
    for ident in ["inactivemail", "inactivemail@example.com"] {
        let r = authenticate::<AuthUser>(ident, "R1ght-Pass-Phrase!").await;
        assert!(
            matches!(r, Err(AuthError::InvalidCredentials)),
            "inactive user must not authenticate via `{ident}`"
        );
    }
}

// =========================================================================
// random_password_hash — a valid, unknown PHC hash for passwordless accounts
// =========================================================================

#[tokio::test]
async fn random_password_hash_is_valid_unique_and_unguessable() {
    let a = random_password_hash().await.expect("hash a");
    let b = random_password_hash().await.expect("hash b");

    assert!(!a.is_empty(), "the hash must not be empty");
    assert!(
        a.starts_with("$argon2"),
        "must be a real argon2 PHC hash: {a}"
    );
    assert_ne!(a, b, "each call salts + randomizes independently");

    // It is a PARSEABLE hash: verifying a wrong password returns Ok(false),
    // NOT an error — the whole point over a `"!"` sentinel that fails to parse.
    assert!(
        !verify_password("anything", &a).expect("verify must not error on a valid hash"),
        "no plaintext should verify against a random hash"
    );
}
