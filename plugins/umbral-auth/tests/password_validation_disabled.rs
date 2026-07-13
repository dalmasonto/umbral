//! Secure-by-default has an explicit escape hatch:
//! [`AuthPlugin::disable_password_validation`]. This test lives in its OWN
//! test binary (separate process ⇒ a fresh process-global `PASSWORD_POLICY`
//! `OnceLock`) so it can boot an `AuthPlugin` with validation turned off and
//! prove a weak password sails through the `register` ROUTE.
//!
//! It MUST be a separate file from `password_validation.rs`: that file boots
//! the default secure policy into the same `OnceLock`, and the first install
//! wins. Two policies can't coexist in one process.
//!
//! Note the layer this tests: enforcement now lives at the registration
//! boundary (the `register` route), not in `create_user` — which never
//! validates regardless of the flag (by design). So the disabled-flag
//! contract is observed where it actually matters: the route that WOULD
//! reject `"a"` under the default policy now accepts it.

use axum::body::Body;
use axum::http::Request;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;
use umbral::prelude::Plugin;
use umbral_auth::{AuthPlugin, AuthUser};

/// With `disable_password_validation()`, the weak password `"a"` — which
/// every default validator rejects — is accepted by the `register` route and
/// persisted. This is the opt-OUT contract: an app that explicitly asks for no
/// policy gets none, even on its untrusted signup surface.
#[tokio::test]
async fn disabled_validation_accepts_weak_password_at_register_route() {
    let settings = umbral::Settings::from_env().expect("figment defaults load in a test env");

    let tmp = tempfile::tempdir().expect("create tempdir for the test DB");
    let db_path = tmp.path().join("umbral_auth_pwdisabled.sqlite");
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
        // The explicit opt-OUT: no password validation. `disable_throttle` so
        // the route test isn't rate-limited; `with_default_routes` mounts the
        // /api/auth surface we drive below.
        .plugin(
            AuthPlugin::<AuthUser>::default()
                .disable_password_validation()
                .disable_throttle(),
        )
        .build()
        .expect("App::build should succeed");

    umbral::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema");

    let pool = umbral::db::pool();

    // The disabled policy is installed ambiently in on_ready; the route reads
    // it via `validate_password`, so `"a"` passes the boundary check.
    let router = AuthPlugin::<AuthUser>::default()
        .with_default_routes()
        .routes();
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/register")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"username":"anyone","email":"anyone@example.com","password":"a"}"#
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("register request must not panic");
    let status = resp.status();
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("collect body")
        .to_bytes();
    assert_eq!(
        status,
        http::StatusCode::CREATED,
        "with validation disabled, even `a` must be accepted by /register; body={}",
        String::from_utf8_lossy(&body),
    );

    // And the row really landed.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_user WHERE username = 'anyone'")
        .fetch_one(&pool)
        .await
        .expect("count query");
    assert_eq!(
        count, 1,
        "the weak-password registration must persist a row"
    );
}
