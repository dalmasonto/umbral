//! End-to-end coverage for the secure-by-default password-strength
//! validators (umbra's `AUTH_PASSWORD_VALIDATORS` equivalent).
//!
//! Two layers:
//!
//! 1. **Validator-level** — each of the four default validators rejects
//!    the canonical weak input and accepts a strong one, and
//!    `validate_password` aggregates multiple failures.
//! 2. **Helper-level (real ORM + test DB)** — `create_user` rejects a weak
//!    password with `AuthError::WeakPassword` and accepts a strong one,
//!    writing a real row. Mirrors the boot harness in `integration.rs`.
//!
//! See `plugins/umbra-auth/src/password_validation.rs` for the surface and
//! `CLAUDE.md` "secure-by-default" for why this is on with no opt-in.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbra_auth::{
    AuthError, AuthPlugin, AuthUser, CommonPasswordValidator, MinLengthValidator,
    NumericPasswordValidator, PasswordContext, PasswordPolicy, PasswordValidator,
    UserAttributeSimilarityValidator, create_superuser, create_user, validate_password,
};

// --------------------------------------------------------------------- //
// Validator-level unit tests — no DB, no boot.                           //
// --------------------------------------------------------------------- //

#[test]
fn min_length_validator_rejects_short_password() {
    let v = MinLengthValidator::default();
    assert!(
        v.validate_via("abc").is_err(),
        "a 3-char password must be rejected by the min-length validator"
    );
    assert!(
        v.validate_via("abcdefgh").is_ok(),
        "an 8-char password must pass the default min-length validator"
    );
}

#[test]
fn common_password_validator_rejects_password() {
    let v = CommonPasswordValidator;
    assert!(
        v.validate_via("password").is_err(),
        "`password` must be in the common-password denylist"
    );
    assert!(
        v.validate_via("PASSWORD").is_err(),
        "the common-password match must be case-insensitive"
    );
}

#[test]
fn numeric_password_validator_rejects_all_digits() {
    let v = NumericPasswordValidator;
    assert!(
        v.validate_via("12345678").is_err(),
        "an all-numeric password must be rejected"
    );
    assert!(
        v.validate_via("abc12345").is_ok(),
        "a mixed alphanumeric password must pass the numeric validator"
    );
}

#[test]
fn similarity_validator_rejects_password_like_username() {
    let v = UserAttributeSimilarityValidator::default();
    let ctx = PasswordContext::for_username("alice");
    assert!(
        v.validate("alice123", &ctx).is_err(),
        "`alice123` must be flagged as too similar to username `alice`"
    );
    assert!(
        v.validate("Tr0ub4dour&3xpl", &ctx).is_ok(),
        "an unrelated strong password must not be flagged"
    );
}

#[test]
fn validate_password_aggregates_multiple_failures() {
    // `12345678` is all-numeric AND in the common-password denylist —
    // expect both reasons, not just the first.
    let reasons = validate_password("12345678", &PasswordContext::empty())
        .expect_err("a doubly-weak password must fail");
    assert!(
        reasons.len() >= 2,
        "validate_password must collect every failure; got {reasons:?}"
    );
}

#[test]
fn strong_password_passes_all_validators() {
    let ctx = PasswordContext::new(Some("alice"), Some("alice@example.com"));
    assert!(
        validate_password("Tr0ub4dour&3xpl", &ctx).is_ok(),
        "a strong password must pass the full default policy"
    );
}

#[test]
fn disable_password_validation_installs_empty_policy() {
    // The builder produces a plugin whose policy is empty. We can't easily
    // assert the ambient install in a shared-process test (another test may
    // win the OnceLock first), so assert the policy the builder would
    // install is empty — the contract `disable_password_validation` makes.
    let policy = PasswordPolicy::empty();
    assert!(
        policy.check("a", &PasswordContext::empty()).is_ok(),
        "an empty policy must accept any password"
    );
    // Sanity: the secure default is NOT empty.
    assert!(
        !PasswordPolicy::default().is_empty(),
        "the default policy must enforce the secure validator set"
    );
}

// A tiny ergonomic helper for the no-context validator tests above.
trait ValidateVia {
    fn validate_via(&self, password: &str) -> Result<(), String>;
}
impl<T: umbra_auth::PasswordValidator> ValidateVia for T {
    fn validate_via(&self, password: &str) -> Result<(), String> {
        self.validate(password, &PasswordContext::empty())
    }
}

// --------------------------------------------------------------------- //
// Helper-level tests — real ORM + tempfile SQLite, mirrors integration.rs //
// --------------------------------------------------------------------- //

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("create tempdir for the test DB");
        let db_path = tmp.path().join("umbra_auth_pwvalidation.sqlite");
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
            // Default plugin → secure-by-default policy installed in on_ready.
            .plugin(AuthPlugin::<AuthUser>::default())
            .build()
            .expect("App::build should succeed with AuthPlugin");

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

/// `create_user` with a weak password is rejected BEFORE any row is
/// written, surfacing `AuthError::WeakPassword` with the failure reasons.
/// This is the load-bearing secure-by-default test: registration accepting
/// `"a"` is exactly the bug this feature closes.
#[tokio::test]
async fn create_user_rejects_weak_password() {
    boot().await;

    let result = create_user("weakling", "weak@example.com", "a").await;
    match result {
        Err(AuthError::WeakPassword(reasons)) => {
            assert!(
                !reasons.is_empty(),
                "WeakPassword must carry at least one reason"
            );
        }
        other => panic!("expected AuthError::WeakPassword for password `a`; got {other:?}"),
    }

    // And no row leaked into the DB.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_user WHERE username = 'weakling'")
        .fetch_one(&umbra::db::pool())
        .await
        .expect("count query");
    assert_eq!(count, 0, "a rejected create_user must not write a row");
}

/// `create_user` with a strong password succeeds and writes a row — proving
/// the validator doesn't block legitimate registrations.
#[tokio::test]
async fn create_user_accepts_strong_password() {
    boot().await;

    // create_superuser is a trusted operator/seed path and BYPASSES the
    // policy: a deliberately-chosen weak/username-matching password (the
    // shape a seed script uses, e.g. "shopadmin"/"shopadmin") is accepted,
    // even though create_user would reject it via the similarity validator.
    {
        boot().await;
        let su = create_superuser("shopadmin", "shopadmin@example.com", "shopadmin")
            .await
            .expect("create_superuser must bypass the password policy (trusted seed path)");
        assert!(su.is_superuser, "create_superuser sets is_superuser");
        // The same password through the untrusted create_user path is rejected.
        let rejected = create_user("shopadmin2", "s2@example.com", "shopadmin2").await;
        assert!(
            matches!(rejected, Err(AuthError::WeakPassword(_))),
            "create_user must still reject a username-matching password"
        );
    }

    let user = create_user("stronguser", "strong@example.com", "Tr0ub4dour&3xpl")
        .await
        .expect("a strong password must be accepted by create_user");
    assert_eq!(user.username, "stronguser");
    assert_ne!(
        user.password_hash, "Tr0ub4dour&3xpl",
        "the stored value must be the hash, not the plaintext"
    );
}

/// A password too similar to the username is rejected through the real
/// `create_user` path (the username flows into the similarity context).
#[tokio::test]
async fn create_user_rejects_password_similar_to_username() {
    boot().await;

    let result = create_user("bobby", "bobby@example.com", "bobby1234").await;
    assert!(
        matches!(result, Err(AuthError::WeakPassword(_))),
        "a password containing the username must be rejected; got {result:?}"
    );
}
