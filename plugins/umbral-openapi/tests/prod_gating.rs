//! audit_2 plugin-observability #1 (H12): the OpenAPI spec + Swagger UI must
//! NOT mount in `Environment::Prod` unless explicitly opted in — an
//! unauthenticated full-API-surface disclosure otherwise. Own test binary so
//! the process-global `UMBRAL_ENVIRONMENT=Prod` is set before settings init.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

use umbral_openapi::OpenApiPlugin;
use umbral_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Note {
    id: i64,
    title: String,
}

async fn boot_prod() -> axum::Router {
    // Must be set before `Settings::from_env()`. Prod boot hard-fails on the
    // insecure dev SECRET_KEY, so give it a real one to reach the routes() gate.
    unsafe {
        std::env::set_var("UMBRAL_ENVIRONMENT", "Prod");
        std::env::set_var(
            "UMBRAL_SECRET_KEY",
            "prod-gating-test-secret-key-0123456789abcdef",
        );
    }
    let settings = umbral::Settings::from_env().expect("settings");
    assert!(
        matches!(settings.environment, umbral::Environment::Prod),
        "test harness must run in Prod"
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("openapi_prod.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Note>()
        .plugin(RestPlugin::default())
        .plugin(OpenApiPlugin::default())
        .build()
        .expect("App::build")
        .into_router()
}

async fn status(router: &axum::Router, uri: &str) -> StatusCode {
    // Prod enforces ALLOWED_HOSTS (default localhost/127.0.0.1); send a valid
    // Host so we test the route gate, not the host guard.
    let req = Request::builder()
        .uri(uri)
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    router.clone().oneshot(req).await.expect("oneshot").status()
}

#[tokio::test]
async fn openapi_not_mounted_in_prod_by_default() {
    let router = boot_prod().await;

    assert_eq!(
        status(&router, "/openapi/openapi.json").await,
        StatusCode::NOT_FOUND,
        "the OpenAPI JSON spec must not be served in Prod without opt-in"
    );
    assert_eq!(
        status(&router, "/openapi/").await,
        StatusCode::NOT_FOUND,
        "the Swagger UI must not be served in Prod without opt-in"
    );
}
