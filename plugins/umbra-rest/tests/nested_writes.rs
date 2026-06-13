//! Writable nested serializers (feature #58). `POST /api/order/` with a
//! nested `items: [...]` array creates the order + its line items (FK
//! auto-set) in one request; a bad child compensates (the parent is
//! rolled back). One App::build (settings init is one-shot).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

use umbra::orm::ForeignKey;
use umbra_rest::{ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Order {
    id: i64,
    customer: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct OrderItem {
    id: i64,
    #[umbra(on_delete = "cascade")]
    order: ForeignKey<Order>,
    product: String,
    qty: i32,
}

async fn boot() -> axum::Router {
    let settings = umbra::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("nested.sqlite");
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

    let rest = RestPlugin::default()
        .resource(ResourceConfig::for_::<Order>().nested("items", "order_item"));

    let app = umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Order>()
        .model::<OrderItem>()
        .plugin(rest)
        .build()
        .expect("App::build");

    let pool = umbra::db::pool();
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
async fn nested_create_and_compensation() {
    let router = boot().await;

    // 1. POST an order with two line items — parent + children in one call.
    let (status, body) = post(
        &router,
        "/api/order/",
        json!({
            "customer": "Ada",
            "items": [
                { "product": "Widget", "qty": 2 },
                { "product": "Gadget", "qty": 1 }
            ]
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "nested create returns 201; {body}"
    );

    let order_id = body["id"].as_i64().expect("parent id");
    let items = body["items"]
        .as_array()
        .expect("items embedded in response");
    assert_eq!(items.len(), 2, "both children returned");
    // Each child got its FK to the parent set automatically.
    assert_eq!(items[0]["order"].as_i64(), Some(order_id));
    assert_eq!(items[1]["order"].as_i64(), Some(order_id));
    assert_eq!(items[0]["product"], "Widget");
    assert_eq!(items[1]["qty"], 1);

    // The rows really landed: 1 order, 2 items.
    assert_eq!(Order::objects().count().await.unwrap(), 1);
    assert_eq!(OrderItem::objects().count().await.unwrap(), 2);

    // 2. POST an order whose SECOND item is invalid (missing `product`).
    //    The whole write must roll back — no orphaned order.
    let (status, _body) = post(
        &router,
        "/api/order/",
        json!({
            "customer": "Grace",
            "items": [
                { "product": "Good", "qty": 1 },
                { "qty": 5 }
            ]
        }),
    )
    .await;
    assert!(
        status.is_client_error(),
        "an invalid child fails the request (got {status})"
    );

    // Compensation: still just the first order + its 2 items — Grace's
    // order and its one good child were deleted.
    assert_eq!(
        Order::objects().count().await.unwrap(),
        1,
        "the half-created parent was compensated (deleted)"
    );
    assert_eq!(
        OrderItem::objects().count().await.unwrap(),
        2,
        "the already-created sibling was compensated too"
    );
}
