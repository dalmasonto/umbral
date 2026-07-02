//! A nested child write must enforce the CHILD resource's own permission
//! class, not just the parent handler's gate (audit_2 plugin-rest H2). Here
//! the parent allows writes but the child is `ReadOnly`, so a nested create of
//! a child must be forbidden and roll the whole tree back. One App::build.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

use umbral::orm::ForeignKey;
use umbral_rest::{AllowAny, ReadOnly, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Order {
    id: i64,
    customer: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct OrderItem {
    id: i64,
    #[umbral(on_delete = "cascade")]
    order: ForeignKey<Order>,
    product: String,
    qty: i32,
}

async fn boot() -> axum::Router {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("nested_childperm.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    // Parent writable (AllowAny); the child is ReadOnly → writes denied.
    let rest = RestPlugin::default()
        .default_permission(AllowAny)
        .resource(ResourceConfig::for_::<Order>().nested("items", "order_item"))
        .resource(ResourceConfig::for_::<OrderItem>().permission(ReadOnly));

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Order>()
        .model::<OrderItem>()
        .plugin(rest)
        .build()
        .expect("App::build");

    let pool = umbral::db::pool();
    sqlx::query(
        "CREATE TABLE \"order\" (id INTEGER PRIMARY KEY AUTOINCREMENT, customer TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("create order");
    sqlx::query(
        "CREATE TABLE order_item (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            \"order\" INTEGER NOT NULL REFERENCES \"order\"(id),
            product TEXT NOT NULL,
            qty INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("create order_item");

    app.into_router()
}

async fn post(router: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn nested_child_write_enforces_child_permission() {
    let router = boot().await;

    // Parent write is allowed, but creating the child is not (ReadOnly).
    let (status, body) = post(
        &router,
        "/api/order/",
        json!({
            "customer": "Ada",
            "items": [ { "product": "Widget", "qty": 2 } ]
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a nested child write must honor the child's own permission class; {body}"
    );

    // Whole tree rolled back — no parent, no child.
    assert_eq!(
        Order::objects().count().await.unwrap(),
        0,
        "the forbidden nested write created no parent"
    );
    assert_eq!(
        OrderItem::objects().count().await.unwrap(),
        0,
        "the forbidden nested write created no child"
    );
}
