//! BUG-1 from bugs/tests/testBugs.md: POST to a REST endpoint with
//! a JSON boolean used to land as TEXT in a SQLite BOOLEAN column,
//! breaking decode on the subsequent SELECT. This test boots a fresh
//! Product model with a `is_featured: bool` column, POSTs through
//! the REST plugin with `"is_featured": false`, then reads the row
//! back to verify the round-trip.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{AllowAny, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Product {
    id: i64,
    name: String,
    is_featured: bool,
    in_stock: bool,
    /// Nullable boolean — exercises the SeaValue::Bool(None) path
    /// on POSTs that omit the field.
    is_archived: Option<bool>,
    /// Boolean with a DDL default — exercises the path where the
    /// caller omits the field and the default kicks in. Closes the
    /// gap where the OLD `DEFAULT 'true'` rendering stored TEXT on
    /// SQLite (closed by IMP-2 in the previous commit).
    #[umbral(default = "true")]
    is_visible: bool,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("rest_boolean_round_trip.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
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
            .model::<Product>()
            .plugin(RestPlugin::default().default_permission(AllowAny))
            .build()
            .expect("App::build with RestPlugin");

        // Use the migration engine's own DDL rendering rather than
        // a hand-written CREATE TABLE so the test catches column-
        // type differences between the renderer and what we type
        // by hand.
        let pool = umbral::db::pool();
        let meta = umbral::migrate::ModelMeta::for_::<Product>();
        let op = umbral::migrate::Operation::CreateTable {
            table: "product".to_string(),
            columns: meta.fields.clone(),
            unique_together: Vec::new(),
            indexes: Vec::new(),
        };
        for stmt in umbral::migrate::render_operation_for(&op, "sqlite") {
            sqlx::query(&stmt)
                .execute(&pool)
                .await
                .expect("apply create product");
        }

        app.into_router()
    })
    .await
}

#[tokio::test]
async fn rest_post_with_boolean_json_round_trips_through_sqlite() {
    let router = boot().await.clone();

    // POST with two booleans — one true, one false. is_archived
    // (nullable) is omitted; is_visible (bool with DEFAULT true) is
    // also omitted so the DDL default kicks in.
    let req = Request::builder()
        .method("POST")
        .uri("/api/product/")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"name":"widget","is_featured":false,"in_stock":true}"#,
        ))
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED, "POST should 201");
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("response body is json");
    assert_eq!(created["name"], "widget");
    assert_eq!(
        created["is_featured"], false,
        "RETURNING / re-fetched row must report is_featured as JSON false; got: {created}",
    );
    assert_eq!(created["in_stock"], true);
    assert!(
        created["is_archived"].is_null(),
        "omitted nullable bool should decode as JSON null; got: {created}",
    );
    assert_eq!(
        created["is_visible"], true,
        "DDL DEFAULT true on a bool column should round-trip as JSON true (IMP-2 fix); got: {created}",
    );

    // Read it back via GET — the path that previously failed with
    // "Rust type `bool` is not compatible with SQL type `TEXT`".
    let req = Request::builder()
        .method("GET")
        .uri("/api/product/")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "GET /api/product/ after a POST with bool should not 500 on decode",
    );
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let listed: serde_json::Value = serde_json::from_slice(&body_bytes).expect("body json");
    let results = listed["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "list should contain the row we inserted"
    );
    let first = &results[0];
    assert_eq!(first["is_featured"], false);
    assert_eq!(first["in_stock"], true);
    assert!(first["is_archived"].is_null());
    assert_eq!(first["is_visible"], true);
}
