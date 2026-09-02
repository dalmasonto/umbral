//! `.action_by(...)`'s column guard (gaps4 #80): the lookup column must be
//! `#[umbral(unique)]` (or the primary key), checked at boot — the same
//! "caught at boot, not in prod" posture the framework uses for backend/
//! field mismatches. A non-unique lookup column panics when the router is
//! assembled, rather than silently risking a multi-row match at request
//! time.
//!
//! Own test binary: the panic happens inside `RestPlugin::routes()`
//! (invoked from `app.into_router()`), which would otherwise poison the
//! process-wide settings/pool `OnceLock`s other tests in this crate share.

#![allow(dead_code, private_interfaces)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Community {
    id: i64,
    // NOT marked `#[umbral(unique)]` — the misconfiguration this test
    // guards against.
    category: String,
    name: String,
}

#[tokio::test]
#[should_panic(expected = "must be #[umbral(unique)]")]
async fn non_unique_lookup_column_panics_at_boot() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("action_by_guard.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    let resource = ResourceConfig::new("community").action_by(
        "category",
        http::Method::GET,
        |_ctx| async move { Ok(serde_json::json!({})) },
    );
    let rest = RestPlugin::default()
        .default_permission(AllowAny)
        .resource(resource);

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Community>()
        .plugin(rest)
        .build()
        .expect("App::build");

    // The guard panics here, while assembling the router.
    let _ = app.into_router();
}
