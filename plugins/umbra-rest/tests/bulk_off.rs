//! Bulk endpoints are OFF by default (gaps2 #82). A resource WITHOUT
//! `.bulk()` keeps the original behaviour: a POST of a JSON array is
//! rejected (it isn't a single-object body), and the collection-level
//! PATCH / DELETE return 404 (the route mounts, but the handler self-gates
//! on the per-table opt-in).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

use tokio::sync::OnceCell;
use umbra_rest::{AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Gadget {
    id: i64,
    name: String,
}

// One App::build per binary; share the router across tests.
static ROUTER: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> axum::Router {
    ROUTER.get_or_init(build).await.clone()
}

async fn build() -> axum::Router {
    let settings = umbra::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("bulk_off.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    // Note: NO `.bulk()`.
    let rest = RestPlugin::default()
        .default_permission(AllowAny)
        .resource(ResourceConfig::for_::<Gadget>());

    let app = umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Gadget>()
        .plugin(rest)
        .build()
        .expect("App::build");

    let pool = umbra::db::pool();
    sqlx::query("CREATE TABLE gadget (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create gadget");

    app.into_router()
}

async fn send(router: &axum::Router, method: Method, uri: &str, body: Value) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let _ = resp.into_body().collect().await;
    status
}

#[tokio::test]
async fn array_post_rejected_without_bulk() {
    let router = boot().await;
    let status = send(
        &router,
        Method::POST,
        "/api/gadget/",
        json!([{ "name": "a" }, { "name": "b" }]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an array POST is rejected when the resource didn't opt into bulk"
    );
    // The two array items ("a"/"b") never landed — the array was rejected
    // wholesale before any insert.
    let arr_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gadget WHERE name IN ('a','b')")
        .fetch_one(&umbra::db::pool())
        .await
        .unwrap();
    assert_eq!(arr_rows, 0, "no rows created from the rejected array");
}

#[tokio::test]
async fn collection_patch_delete_not_available_without_bulk() {
    let router = boot().await;

    let patch = send(
        &router,
        Method::PATCH,
        "/api/gadget/",
        json!([{ "id": 1, "name": "x" }]),
    )
    .await;
    assert_eq!(
        patch,
        StatusCode::NOT_FOUND,
        "collection PATCH has no bulk surface without .bulk()"
    );

    let delete = send(&router, Method::DELETE, "/api/gadget/", json!({ "ids": [1] })).await;
    assert_eq!(
        delete,
        StatusCode::NOT_FOUND,
        "collection DELETE has no bulk surface without .bulk()"
    );

    // The single-object POST is still fine.
    let single = send(&router, Method::POST, "/api/gadget/", json!({ "name": "ok" })).await;
    assert_eq!(single, StatusCode::CREATED);
}
