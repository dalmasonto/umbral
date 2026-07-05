//! BUG-5 from bugs/tests/testBugs.md: `#[umbral(auto_now)]` and
//! `#[umbral(auto_now_add)]` columns auto-populate with `Utc::now()`
//! on REST writes when the body omits them. Created/updated
//! timestamps no longer need to be hand-set in the request body.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{AllowAny, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Item {
    id: i64,
    name: String,
    /// Frozen at create time. Updates leave it alone.
    #[umbral(auto_now_add)]
    created_at: DateTime<Utc>,
    /// Refreshes on every write (create + update).
    #[umbral(auto_now)]
    updated_at: DateTime<Utc>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("rest_auto_now.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Item>()
            .plugin(RestPlugin::default().default_permission(AllowAny))
            .build()
            .expect("App::build with RestPlugin");

        let pool = umbral::db::pool();
        let meta = umbral::migrate::ModelMeta::for_::<Item>();
        let op = umbral::migrate::Operation::CreateTable {
            table: "item".to_string(),
            columns: meta.fields.clone(),
            unique_together: Vec::new(),
            indexes: Vec::new(),
        };
        for stmt in umbral::migrate::render_operation_for(&op, "sqlite") {
            sqlx::query(&stmt)
                .execute(&pool)
                .await
                .expect("apply create item");
        }
        app.into_router()
    })
    .await
}

#[tokio::test]
async fn rest_post_omitting_auto_now_columns_auto_populates_them() {
    let router = boot().await.clone();

    // POST with neither timestamp — both should land via auto-populate.
    let before = Utc::now();
    let req = Request::builder()
        .method("POST")
        .uri("/api/item/")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"name":"widget"}"#))
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let after = Utc::now();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "POST omitting auto_now / auto_now_add cols should still 201 (BUG-5)",
    );
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let created_at: DateTime<Utc> =
        serde_json::from_value(created["created_at"].clone()).expect("created_at parses");
    let updated_at: DateTime<Utc> =
        serde_json::from_value(created["updated_at"].clone()).expect("updated_at parses");
    assert!(
        created_at >= before && created_at <= after,
        "created_at should be inside the request window: before={before:?} created_at={created_at:?} after={after:?}",
    );
    assert!(
        updated_at >= before && updated_at <= after,
        "updated_at should be inside the request window on create",
    );

    let id = created["id"].as_i64().expect("id");

    // Sleep a hair so updated_at can move while created_at stays.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // PATCH the name. updated_at refreshes; created_at stays frozen.
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/api/item/{id}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"name":"renamed"}"#))
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    assert!(
        resp.status().is_success(),
        "PATCH should succeed; got {}",
        resp.status(),
    );

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/item/{id}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let fetched: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let new_created_at: DateTime<Utc> =
        serde_json::from_value(fetched["created_at"].clone()).expect("created_at parses");
    let new_updated_at: DateTime<Utc> =
        serde_json::from_value(fetched["updated_at"].clone()).expect("updated_at parses");
    assert_eq!(
        new_created_at, created_at,
        "auto_now_add (created_at) must stay frozen across the PATCH",
    );
    assert!(
        new_updated_at > updated_at,
        "auto_now (updated_at) must refresh on PATCH; was {updated_at:?}, now {new_updated_at:?}",
    );
}
