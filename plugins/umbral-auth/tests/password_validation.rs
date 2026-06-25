//! End-to-end coverage for the secure-by-default password-strength
//! validators (umbral's `AUTH_PASSWORD_VALIDATORS` equivalent).
//!
//! Enforcement lives at the **registration boundary** (the `register`
//! route), matching Django: `User.objects.create_user()` does NOT validate;
//! forms / views do. So this file covers two layers:
//!
//! 1. **Validator-level** — each of the four default validators rejects
//!    the canonical weak input and accepts a strong one, and
//!    `validate_password` aggregates multiple failures.
//! 2. **Route-level (real ORM + test DB)** — `POST <prefix>/register` with a
//!    weak password returns 400 carrying the reasons; with a strong password
//!    it creates the user (201). The low-level `create_user` helper, by
//!    contrast, accepts a weak password directly (it does NOT validate) — we
//!    assert that too, to document the Django-parity split.
//!
//! See `plugins/umbral-auth/src/password_validation.rs` for the surface and
//! `CLAUDE.md` "secure-by-default" for why this is on with no opt-in.

use axum::body::Body;
use axum::http::Request;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::prelude::Plugin;
use umbral_auth::{
    AuthPlugin, AuthUser, CommonPasswordValidator, MinLengthValidator, NumericPasswordValidator,
    PasswordContext, PasswordPolicy, PasswordValidator, UserAttributeSimilarityValidator,
    create_user, validate_password,
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
impl<T: umbral_auth::PasswordValidator> ValidateVia for T {
    fn validate_via(&self, password: &str) -> Result<(), String> {
        self.validate(password, &PasswordContext::empty())
    }
}

// --------------------------------------------------------------------- //
// Route-level + helper-level tests — real ORM + tempfile SQLite.          //
// Mirrors the boot harness in integration.rs; the router comes from the   //
// public `AuthPlugin::with_default_routes().routes()` surface and is      //
// driven with tower `oneshot`, exactly as `user_context_lazy.rs` does.    //
// --------------------------------------------------------------------- //

const PREFIX: &str = "/api/auth";

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("create tempdir for the test DB");
        let db_path = tmp.path().join("umbral_auth_pwvalidation.sqlite");
        std::mem::forget(tmp);
        let options = SqliteConnectOptions::new()
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
            // Default plugin → secure-by-default policy installed in on_ready,
            // and the built-in /api/auth routes mounted for the route tests.
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    .with_default_routes()
                    // Disable register throttling so the route tests can hammer
                    // /register from one (sentinel) IP without hitting a 429.
                    .disable_throttle(),
            )
            .build()
            .expect("App::build should succeed with AuthPlugin");

        let pool = umbral::db::pool();
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

/// Build the auth router from the public plugin surface. Each call returns a
/// fresh `axum::Router`, so a `oneshot` (which consumes the service) doesn't
/// disturb later requests.
fn auth_router() -> axum::Router {
    AuthPlugin::<AuthUser>::default()
        .with_default_routes()
        .routes()
}

/// `POST <prefix>/register` with a JSON body, returning `(status, body_bytes)`.
async fn post_register(json: &str) -> (http::StatusCode, Vec<u8>) {
    let resp = auth_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{PREFIX}/register"))
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(json.to_string()))
                .unwrap(),
        )
        .await
        .expect("register request must not panic");
    let status = resp.status();
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec();
    (status, body)
}

/// The load-bearing secure-by-default test, now at the right layer: the
/// `register` ROUTE rejects the weak password `"a"` with 400 and surfaces the
/// failure reasons. Registration accepting `"a"` is exactly the bug the
/// password policy closes — and the route is where Django enforces it.
#[tokio::test]
async fn register_route_rejects_weak_password() {
    boot().await;

    let (status, body) = post_register(
        r#"{"username":"weakling","email":"weak@example.com","password":"a"}"#,
    )
    .await;
    assert_eq!(
        status,
        http::StatusCode::BAD_REQUEST,
        "register with password `a` must be 400; body={}",
        String::from_utf8_lossy(&body),
    );
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("error body is JSON");
    assert_eq!(parsed["error"], "weak_password");
    assert!(
        parsed["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "the 400 must carry at least one human-readable reason; body={parsed}"
    );

    // And no row leaked into the DB — the route rejects BEFORE create_user.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM auth_user WHERE username = 'weakling'")
            .fetch_one(&umbral::db::pool())
            .await
            .expect("count query");
    assert_eq!(count, 0, "a rejected register must not write a row");
}

/// `POST <prefix>/register` with a strong password creates the user (201) and
/// writes a real row — proving the validator doesn't block legitimate
/// registrations.
#[tokio::test]
async fn register_route_accepts_strong_password() {
    boot().await;

    let (status, body) = post_register(
        r#"{"username":"stronguser","email":"strong@example.com","password":"Tr0ub4dour&3xpl"}"#,
    )
    .await;
    assert_eq!(
        status,
        http::StatusCode::CREATED,
        "register with a strong password must be 201; body={}",
        String::from_utf8_lossy(&body),
    );

    let row: (String, String) =
        sqlx::query_as("SELECT username, password_hash FROM auth_user WHERE username = ?")
            .bind("stronguser")
            .fetch_one(&umbral::db::pool())
            .await
            .expect("the stronguser row should exist after a successful register");
    assert_eq!(row.0, "stronguser");
    assert_ne!(
        row.1, "Tr0ub4dour&3xpl",
        "the stored value must be the hash, not the plaintext"
    );
}

/// The Django-parity split, asserted directly: the low-level `create_user`
/// helper is NON-validating. The same weak password the route rejects above
/// sails straight through `create_user` and persists a row. This is what lets
/// seed scripts / bulk imports / the workspace test suite create users with
/// deliberately-chosen passwords without tripping the policy.
#[tokio::test]
async fn create_user_helper_does_not_validate() {
    boot().await;

    // "a" is rejected by every default validator — yet create_user accepts it,
    // because validation lives at the register boundary, not in the helper.
    let user = create_user("lowlevel", "lowlevel@example.com", "a")
        .await
        .expect("create_user is low-level and must NOT validate the password");
    assert_eq!(user.username, "lowlevel");
    assert_ne!(
        user.password_hash, "a",
        "create_user must still hash, just not validate"
    );

    // A username-matching password (the kind the similarity validator flags)
    // also goes through, confirming no policy runs in the helper.
    let similar = create_user("bobby", "bobby@example.com", "bobby1234")
        .await
        .expect("create_user must not run the similarity validator");
    assert_eq!(similar.username, "bobby");
}
