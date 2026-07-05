//! Recursive N-level writable nesting (gaps3 #10). A `.nested()` declaration
//! on each level lets a single `POST`/`PATCH` create or upsert a whole tree:
//! order → items → components. Depth is driven by `cfg.nested` per table, so
//! a grandchild is written iff its parent's table also declared `.nested()`.
//! Sibling of `nested_writes.rs` / `nested_updates.rs`.

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
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Component {
    id: i64,
    #[umbral(on_delete = "cascade")]
    item: ForeignKey<OrderItem>,
    name: String,
    grams: i32,
}

async fn boot() -> axum::Router {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("nested_deep.sqlite");
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

    // Nesting is declared per level: order → items, item → components.
    let rest = RestPlugin::default()
        .default_permission(AllowAny)
        .resource(ResourceConfig::for_::<Order>().nested("items", "order_item"))
        .resource(ResourceConfig::for_::<OrderItem>().nested("components", "component"));

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Order>()
        .model::<OrderItem>()
        .model::<Component>()
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
    sqlx::query(
        "CREATE TABLE component (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            item INTEGER NOT NULL REFERENCES order_item(id),
            name TEXT NOT NULL,
            grams INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("create component");

    app.into_router()
}

async fn send(
    router: &axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
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
async fn three_level_create_and_deep_upsert() {
    let router = boot().await;

    // --- 3-level CREATE: order → item → components, all in one POST.
    let (status, body) = send(
        &router,
        Method::POST,
        "/api/order/",
        json!({
            "customer": "Ada",
            "items": [
                {
                    "product": "Widget", "qty": 2,
                    "components": [
                        { "name": "Screw", "grams": 5 },
                        { "name": "Spring", "grams": 3 }
                    ]
                }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "3-level create: {body}");

    let order_id = body["id"].as_i64().expect("order id");
    let item = &body["items"][0];
    let item_id = item["id"].as_i64().expect("item id");
    let comps = item["components"].as_array().expect("components echoed");
    assert_eq!(comps.len(), 2, "both grandchildren returned");
    let screw_id = comps[0]["id"].as_i64().expect("screw id");

    // The whole tree really landed — grandchildren are NOT silently dropped.
    assert_eq!(Order::objects().count().await.unwrap(), 1);
    assert_eq!(OrderItem::objects().count().await.unwrap(), 1);
    assert_eq!(Component::objects().count().await.unwrap(), 2);
    // Grandchild FK auto-wired to the middle level.
    assert_eq!(comps[0]["item"].as_i64(), Some(item_id));

    // --- 3-level UPSERT via PATCH: update the existing screw, add a new
    //     grandchild, and DON'T mention the spring (must survive).
    let (status, body) = send(
        &router,
        Method::PATCH,
        &format!("/api/order/{order_id}"),
        json!({
            "items": [
                {
                    "id": item_id,
                    "components": [
                        { "id": screw_id, "grams": 50 },
                        { "name": "Washer", "grams": 1 }
                    ]
                }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "deep upsert: {body}");

    // Screw updated in place.
    let screw = Component::objects()
        .filter(component::ID.eq(screw_id))
        .first()
        .await
        .unwrap()
        .expect("screw row");
    assert_eq!(screw.grams, 50, "grandchild updated at depth 3");
    assert_eq!(screw.name, "Screw", "unmentioned grandchild cols preserved");

    // Spring untouched (absent from payload → no implicit delete), Washer added.
    assert_eq!(
        Component::objects()
            .filter(component::ITEM.eq(item_id))
            .count()
            .await
            .unwrap(),
        3,
        "spring survived, washer created — no implicit deletes at depth 3"
    );

    // --- Cross-parent guard holds at depth 3: patch with a component id that
    //     belongs to a different item is a 404, and the tree rolls back.
    let (_s, order_b) = send(
        &router,
        Method::POST,
        "/api/order/",
        json!({
            "customer": "Grace",
            "items": [ { "product": "Bolt", "qty": 1,
                         "components": [ { "name": "Nut", "grams": 2 } ] } ]
        }),
    )
    .await;
    let b_item_id = order_b["items"][0]["id"].as_i64().unwrap();
    let b_comp_id = order_b["items"][0]["components"][0]["id"].as_i64().unwrap();

    // Try to reach order A's item with order B's component id.
    let (status, _b) = send(
        &router,
        Method::PATCH,
        &format!("/api/order/{order_id}"),
        json!({
            "items": [ { "id": item_id,
                         "components": [ { "id": b_comp_id, "grams": 999 } ] } ]
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a grandchild pk from another parent is a 404"
    );

    // B's component unchanged (whole nested PATCH rolled back).
    let b_comp = Component::objects()
        .filter(component::ID.eq(b_comp_id))
        .first()
        .await
        .unwrap()
        .expect("order B component");
    assert_eq!(b_comp.grams, 2, "another item's grandchild was not mutated");
    let _ = b_item_id;
}
