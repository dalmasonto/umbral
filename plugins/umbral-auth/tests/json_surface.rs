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
            .plugin(umbral_sessions::SessionsPlugin::default().without_auto_layer())
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    // Keep default password policy so "G00d$Pass!" is validated normally.
                    .with_default_routes()
                    .disable_throttle()
                    .mailer(rec),
            )
            .build()
            .expect("App::build should succeed with AuthPlugin + Recorder mailer");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // Extract the router (consumes App; ambient OnceLocks already set).
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

/// Like [`post`] but returns both the status and the response body as a String.
async fn post_full(router: &Router, uri: &str, body: &str) -> (axum::http::StatusCode, String) {
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// POST an `application/x-www-form-urlencoded` body (an HTML `<form>` submit).
async fn post_form(router: &Router, uri: &str, body: &str) -> axum::http::StatusCode {
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    router.clone().oneshot(req).await.unwrap().status()
}

// =========================================================================
// Tests
// =========================================================================

/// gaps5 #15: the built-in auth endpoints accept an HTML `<form>` POST
/// (`application/x-www-form-urlencoded`), not just JSON — so a plain server-
/// rendered form works against the same route a REST client uses.
#[tokio::test]
async fn auth_endpoints_accept_form_encoded_bodies() {
    let (router, _rec) = boot_app_with_recorder().await;

    // Register via a urlencoded form body.
    assert_eq!(
        post_form(
            &router,
            "/api/auth/register",
            "username=formuser&email=form@example.test&password=G00d%24Pass%21",
        )
        .await,
        axum::http::StatusCode::CREATED,
        "register must accept a form body"
    );

    // Log in via a form body → 200 (same handler a JSON client hits).
    assert_eq!(
        post_form(
            &router,
            "/api/auth/login",
            "username=formuser&password=G00d%24Pass%21",
        )
        .await,
        axum::http::StatusCode::OK,
        "login must accept a form body"
    );

    // A JSON body still works on the same endpoint (default content type).
    assert_eq!(
        post(
            &router,
            "/api/auth/login",
            r#"{"username":"formuser","password":"G00d$Pass!"}"#,
        )
        .await,
        axum::http::StatusCode::OK,
        "JSON must still work after adding form support"
    );
}

/// gaps3 #11: every auth route resolves at BOTH the bare and trailing-slash
/// form, so a client that follows the REST plugin's trailing-slash convention
/// (or whose HTTP client auto-appends) doesn't 404 on login.
#[tokio::test]
async fn both_slash_forms_of_login_resolve() {
    let (router, _rec) = boot_app_with_recorder().await;

    let reg = r#"{"username":"slashuser","email":"slash@example.test","password":"G00d$Pass!"}"#;
    assert_eq!(
        post(&router, "/api/auth/register", reg).await,
        axum::http::StatusCode::CREATED,
        "register the fixture user"
    );

    let creds = r#"{"username":"slashuser","password":"G00d$Pass!"}"#;
    let bare = post(&router, "/api/auth/login", creds).await;
    let slash = post(&router, "/api/auth/login/", creds).await;
    assert_ne!(
        slash,
        axum::http::StatusCode::NOT_FOUND,
        "the trailing-slash login form must not 404"
    );
    assert_eq!(
        bare, slash,
        "both slash forms resolve to the same login handler"
    );
}

/// Audit plugin-auth #5: a duplicate-username register must NOT echo the raw
/// DB / sqlx error (which leaks driver / schema / column names) in the JSON
/// `detail`. It should return the static generic message, while the status
/// still signals the conflict.
#[tokio::test]
async fn register_duplicate_does_not_leak_raw_db_error() {
    let (router, _rec) = boot_app_with_recorder().await;

    let body = r#"{"username":"leaky","email":"leaky@example.com","password":"G00d$Pass!"}"#;
    assert_eq!(
        post(&router, "/api/auth/register", body).await,
        axum::http::StatusCode::CREATED,
        "first register of a fresh user must succeed"
    );

    // Second register with the same username/email trips the UNIQUE constraint.
    let (status, resp_body) = post_full(&router, "/api/auth/register", body).await;
    assert_eq!(
        status,
        axum::http::StatusCode::CONFLICT,
        "a duplicate register still signals a conflict via status"
    );
    // The body must carry only the static generic detail — no raw error text.
    assert!(
        resp_body.contains("could not create account"),
        "detail must be the static generic message; got {resp_body}"
    );
    let lowered = resp_body.to_lowercase();
    for leaked in ["unique", "constraint", "sqlx", "auth_user", "column"] {
        assert!(
            !lowered.contains(leaked),
            "response body must not leak internal error token {leaked:?}; got {resp_body}"
        );
    }
}

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

    // Forgot is always 202 even for unknown emails — and gaps4 #33: the 202
    // carries a `{"detail": ...}` body (a bare 202 read like a broken
    // endpoint), IDENTICAL for known and unknown emails (anti-enumeration).
    let (unknown_status, unknown_body) = post_full(
        &router,
        "/api/auth/password-forgot",
        r#"{"email":"ghost@example.com"}"#,
    )
    .await;
    assert_eq!(unknown_status, axum::http::StatusCode::ACCEPTED);
    let (known_status, known_body) = post_full(
        &router,
        "/api/auth/password-forgot",
        r#"{"email":"dan@example.com"}"#,
    )
    .await;
    assert_eq!(known_status, axum::http::StatusCode::ACCEPTED);
    assert_eq!(
        unknown_body, known_body,
        "the 202 body must not distinguish known from unknown emails"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&unknown_body).expect("password-forgot 202 must carry a JSON body");
    assert!(
        parsed["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "the 202 body carries a non-empty `detail` message; got: {unknown_body}"
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

/// gaps4 #32: `POST /api/auth/logout` must revoke the bearer token the
/// request presented. Before the fix, logout only destroyed the session
/// row, so the 204 was a lie for token clients — the same token kept
/// resolving `/me` forever.
#[tokio::test]
async fn logout_revokes_the_presented_bearer_token() {
    use tower::ServiceExt;
    let (router, _rec) = boot_app_with_recorder().await;

    async fn with_bearer(
        router: &Router,
        method: &str,
        uri: &str,
        token: &str,
    ) -> axum::http::StatusCode {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        router.clone().oneshot(req).await.unwrap().status()
    }

    let reg = r#"{"username":"tokuser","email":"tok@example.test","password":"G00d$Pass!"}"#;
    assert_eq!(
        post(&router, "/api/auth/register", reg).await,
        axum::http::StatusCode::CREATED,
        "register the fixture user"
    );

    let (status, body) = post_full(
        &router,
        "/api/auth/login",
        r#"{"username":"tokuser","password":"G00d$Pass!"}"#,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "login: {body}");
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("LoginOut json");
    let token = parsed["token"].as_str().expect("login returns the token");

    assert_eq!(
        with_bearer(&router, "GET", "/api/auth/me", token).await,
        axum::http::StatusCode::OK,
        "the fresh token must resolve /me"
    );

    assert_eq!(
        with_bearer(&router, "POST", "/api/auth/logout", token).await,
        axum::http::StatusCode::NO_CONTENT,
        "logout with the bearer token returns 204"
    );

    assert_eq!(
        with_bearer(&router, "GET", "/api/auth/me", token).await,
        axum::http::StatusCode::UNAUTHORIZED,
        "the revoked token must no longer resolve /me — logout revokes the presented bearer token"
    );
}

/// gaps4 #35 (same class as the CLI fix): the register boundary rejects a
/// malformed email with a 400 naming the address, and the AuthUser email
/// column carries the `email` text-format marker so the dynamic write
/// paths (admin forms, REST resources) validate it too.
#[tokio::test]
async fn register_rejects_an_invalid_email() {
    let (router, _rec) = boot_app_with_recorder().await;

    let (status, body) = post_full(
        &router,
        "/api/auth/register",
        r#"{"username":"nomail","email":"admin","password":"G00d$Pass!"}"#,
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
        "an email with no @ must be a 400; body: {body}"
    );
    assert!(
        body.contains("not a valid email address"),
        "the error detail names the problem; got: {body}"
    );

    // The column-level marker: single source of truth for every dynamic
    // write path.
    use umbral::prelude::Model;
    let email_field = umbral_auth::AuthUser::FIELDS
        .iter()
        .find(|f| f.name == "email")
        .expect("AuthUser has an email field");
    assert_eq!(
        email_field.text_format,
        Some("email"),
        "AuthUser.email must carry the email text-format marker"
    );
}
