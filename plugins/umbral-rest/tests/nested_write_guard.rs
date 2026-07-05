//! Security guards on writable nested writes (audit_2 plugin-rest H2/H3).
//! A nested child body must be run through `strip_hidden_for_write` (so a
//! caller can't set a hidden field on a nested child) and the whole tree must
//! be bounded by a total-node cap (so one request can't expand to unbounded
//! statements). One App::build per test binary.

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
use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

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
    secret: Option<String>,
}

async fn boot() -> axum::Router {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("nested_guard.sqlite");
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

    // Parent+child both writable; `secret` on the child is hidden.
    let rest = RestPlugin::default()
        .default_permission(AllowAny)
        .resource(ResourceConfig::for_::<Order>().nested("items", "order_item"))
        .hide("order_item", "secret");

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
            qty INTEGER NOT NULL,
            secret TEXT
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
async fn nested_child_hidden_field_stripped_and_tree_bounded() {
    let router = boot().await;

    // --- H2: a hidden field supplied on a NESTED CHILD must be stripped,
    //     exactly as it is on a top-level create.
    let (status, body) = post(
        &router,
        "/api/order/",
        json!({
            "customer": "Ada",
            "items": [
                { "product": "Widget", "qty": 2, "secret": "should-be-stripped" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "nested create: {body}");

    let item_id = body["items"][0]["id"].as_i64().expect("item id");
    let item = OrderItem::objects()
        .filter(order_item::ID.eq(item_id))
        .first()
        .await
        .unwrap()
        .expect("item row");
    assert_eq!(
        item.secret, None,
        "hidden `secret` set on a nested child must be stripped, not persisted"
    );

    // --- H3: the whole nested tree is bounded. A payload with more child
    //     nodes than the cap is a 400 before any rows are committed.
    let before = Order::objects().count().await.unwrap();
    let many: Vec<Value> = (0..1001)
        .map(|i| json!({ "product": format!("p{i}"), "qty": 1 }))
        .collect();
    let (status, _b) = post(
        &router,
        "/api/order/",
        json!({ "customer": "Grace", "items": many }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an oversized nested payload is rejected"
    );
    assert_eq!(
        Order::objects().count().await.unwrap(),
        before,
        "the rejected oversized request created no rows"
    );
}
