//! Verify that a RestPlugin configured with a custom base path publishes that
//! path into `umbral::web::api_base()` during `App::build()`, specifically in
//! the `models()` phase (before router assembly), so that other plugins can
//! read it without a Cargo dependency on umbral-rest.
//!
//! This is its own test binary so the process-wide `API_BASE` OnceLock starts
//! fresh and the assertion is deterministic.

#![allow(dead_code, private_interfaces)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Thing {
    id: i64,
    name: String,
}

#[tokio::test]
async fn rest_publishes_custom_base_into_api_base() {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tmp");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                .filename(":memory:")
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    let _app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Thing>()
        .plugin(RestPlugin::default().at("/v2"))
        .build()
        .expect("build");

    assert_eq!(
        umbral::web::api_base(),
        "/v2",
        "REST should publish its base path into umbral::web::api_base()"
    );
}
