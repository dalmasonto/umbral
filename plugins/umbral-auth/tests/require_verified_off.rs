//! TDD: require_verified_email is OFF by default.
//!
//! Separate binary from `require_verified.rs` so the process-global
//! `REQUIRE_VERIFIED` OnceLock is fresh (false) for this binary. A second
//! App::build in the same process would silently no-op for the flag, leaving
//! the first-boot state in place — so each configuration lives in its own
//! integration-test file.
//!
//! Assertion: a freshly-registered user can log in without verifying their
//! email — login returns 200, not 403.

use axum::Router;
use tokio::sync::OnceCell;
use umbral_auth::{AuthPlugin, AuthUser};

// =========================================================================
// One-time boot (default plugin — no require_verified_email).
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();
static ROUTER: std::sync::OnceLock<Router> = std::sync::OnceLock::new();

async fn boot_app_default() -> Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("create tempdir for require_verified_off test DB");
        let db_path = tmp.path().join("umbral_require_verified_off.sqlite");
        std::mem::forget(tmp);

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

        // Default: require_verified_email is NOT called — unverified login must succeed.
        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(umbral_sessions::SessionsPlugin::default().without_auto_layer())
            .plugin(
                AuthPlugin::<AuthUser>::default()
                    .with_default_routes()
                    .disable_password_validation()
                    .disable_throttle(),
            )
            .build()
            .expect("App::build should succeed with default AuthPlugin");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let router = app.into_router();
        ROUTER.set(router).ok();
    })
    .await;

    ROUTER.get().expect("router set during boot").clone()
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

/// Default behavior: unverified users can log in (no 403 gate).
#[tokio::test]
async fn unverified_login_allowed_when_require_verified_email_off() {
    let router = boot_app_default().await;

    // Register.
    assert_eq!(
        post(
            &router,
            "/api/auth/register",
            r#"{"username":"offtest","email":"offtest@example.com","password":"G00d$Pass!"}"#,
        )
        .await,
        axum::http::StatusCode::CREATED,
        "register must return 201"
    );

    // Login WITHOUT verifying email → must be 200, not 403.
    // (require_verified_email was not called; gate is off by default.)
    assert_eq!(
        post(
            &router,
            "/api/auth/login",
            r#"{"username":"offtest","password":"G00d$Pass!"}"#,
        )
        .await,
        axum::http::StatusCode::OK,
        "login must return 200 (not 403) when require_verified_email is off (the default)"
    );
}
