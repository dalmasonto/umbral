//! Security regression test: `password_hash` is NEVER returned in a REST
//! response, even when the developer calls `.expose(["auth_user"])` on the
//! plugin and forgets to pair it with `.hide("password_hash")`.
//!
//! gaps2 #75: the previous behaviour let `.expose(...)` override the
//! block-list but NOT the per-field hide; a developer who forgot
//! `.hide("password_hash")` would silently leak every user's argon2 hash.
//! The fix adds a HARD_DENIED_FIELDS constant that is applied AFTER all
//! configurable hide / expose / transform logic, making the strip
//! un-overridable.
//!
//! This test drives the exact attack surface:
//!   1. A model with a `password_hash` field.
//!   2. `.expose([table])` to opt the table out of the default block-list
//!      (simulating the developer who opts in to serving auth_user).
//!   3. NO `.hide("password_hash")` call — the bug scenario.
//!   4. Serialize a real row through the full REST stack.
//!   5. Assert `password_hash` is absent from the response.
//!   6. Assert a normal field (`username`) IS present, proving the
//!      response isn't just empty.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{AllowAny, RestPlugin};

/// Stands in for AuthUser: has a `password_hash` column and a normal column.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct AuthUserStub {
    id: i64,
    username: String,
    password_hash: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("pw_hash_never_exposed.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        // THE BUG SCENARIO: expose the table, but do NOT call .hide("password_hash").
        // Without the hard denylist this would leak the hash; with it the strip is
        // unconditional.
        let rest = RestPlugin::default()
            .default_permission(AllowAny)
            // Opt the table out of the block-list (simulates the developer who
            // wants auth_user data via REST).
            .expose(["auth_user_stub"]);
        // Intentionally no .hide("password_hash") here.

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<AuthUserStub>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        sqlx::query(
            "INSERT INTO auth_user_stub (username, password_hash) \
             VALUES ('alice', '$argon2id$v=19$m=65536,t=3,p=4$SECRET')",
        )
        .execute(&pool)
        .await
        .expect("seed row");

        app.into_router()
    })
    .await
}

async fn get_json(router: axum::Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, parsed)
}

/// Retrieve a single row: `password_hash` must be absent even though no
/// `.hide()` was configured and the table was explicitly `.expose()`d.
#[tokio::test]
async fn retrieve_never_exposes_password_hash() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/auth_user_stub/1").await;
    assert_eq!(status, StatusCode::OK, "retrieve must succeed: {body}");
    assert!(
        body.get("password_hash").is_none(),
        "password_hash must be stripped even without an explicit .hide() — got: {body}"
    );
    // Normal field is still present, proving the response isn't just empty.
    assert_eq!(
        body.get("username").and_then(|v| v.as_str()),
        Some("alice"),
        "non-sensitive fields must still be present: {body}"
    );
}

/// List endpoint: same guarantee across the results envelope.
#[tokio::test]
async fn list_never_exposes_password_hash() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/auth_user_stub/").await;
    assert_eq!(status, StatusCode::OK, "list must succeed: {body}");
    let row = &body["results"][0];
    assert!(
        row.get("password_hash").is_none(),
        "password_hash must be stripped from list results — got: {row}"
    );
    assert_eq!(
        row.get("username").and_then(|v| v.as_str()),
        Some("alice"),
        "non-sensitive fields must survive in list results: {row}"
    );
}

/// `is_hidden` must also report `true` for `password_hash` so OpenAPI
/// consumers (e.g. umbral-openapi) never advertise the field in the spec.
#[tokio::test]
async fn is_hidden_reports_true_for_hard_denied_password_hash() {
    boot().await; // ensure CONFIG is populated
    assert!(
        umbral_rest::is_hidden("auth_user_stub", "password_hash"),
        "is_hidden must be true for password_hash on any table — the hard denylist must \
         be reflected in the public API that OpenAPI reads"
    );
}
