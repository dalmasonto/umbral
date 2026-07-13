//! Writable nested UPDATE (gaps3 #9). `PATCH /api/order/{id}` with a nested
//! `items: [...]` array upserts the children in ONE transaction: an item
//! carrying the child pk updates that row (scoped to this parent via the FK),
//! an item without a pk creates one, and children absent from the payload are
//! left untouched (no implicit deletes). A child pk that belongs to a
//! DIFFERENT parent is a 404 and the whole PATCH rolls back. Sibling of
//! `nested_writes.rs`; one App::build (settings init is one-shot).

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

async fn boot() -> axum::Router {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("nested_update.sqlite");
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

    let rest = RestPlugin::default()
        .default_permission(AllowAny)
        .resource(ResourceConfig::for_::<Order>().nested("items", "order_item"));

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Order>()
        .model::<OrderItem>()
        .plugin(rest)
        .build()
        .expect("App::build");

    umbral::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema");

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
async fn nested_update_upserts_children_and_scopes_by_parent() {
    let router = boot().await;

    // Seed order A with two items via the nested-create path.
    let (status, order_a) = send(
        &router,
        Method::POST,
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
    assert_eq!(status, StatusCode::CREATED, "seed create: {order_a}");
    let a_id = order_a["id"].as_i64().expect("order A id");
    let widget_id = order_a["items"][0]["id"].as_i64().expect("widget id");
    let gadget_id = order_a["items"][1]["id"].as_i64().expect("gadget id");

    // Seed a SECOND order B with its own item — used for the cross-parent guard.
    let (_s, order_b) = send(
        &router,
        Method::POST,
        "/api/order/",
        json!({ "customer": "Grace", "items": [ { "product": "Bolt", "qty": 7 } ] }),
    )
    .await;
    let b_item_id = order_b["items"][0]["id"].as_i64().expect("order B item id");

    // --- The nested PATCH: update the parent column, UPDATE the widget by id,
    //     CREATE a new item (no id), and DON'T mention the gadget at all.
    let (status, body) = send(
        &router,
        Method::PATCH,
        &format!("/api/order/{a_id}"),
        json!({
            "customer": "Ada Lovelace",
            "items": [
                { "id": widget_id, "qty": 99 },
                { "product": "Sprocket", "qty": 4 }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "nested update returns 200; {body}");

    // Parent scalar column was updated.
    assert_eq!(body["customer"], "Ada Lovelace");

    // Response carries the two upserted children (updated widget + new sprocket).
    let returned = body["items"].as_array().expect("items in response");
    assert_eq!(
        returned.len(),
        2,
        "only the payload's children are returned"
    );
    assert_eq!(returned[0]["id"].as_i64(), Some(widget_id));
    assert_eq!(returned[0]["qty"], 99, "existing child updated in place");
    assert_eq!(returned[1]["product"], "Sprocket");
    assert_eq!(
        returned[1]["order"].as_i64(),
        Some(a_id),
        "new child's FK auto-set to the parent being updated"
    );

    // Read the object graph back from the DB, not just the response.
    // Widget updated, gadget UNTOUCHED (not in payload), Sprocket created.
    let widget = OrderItem::objects()
        .filter(order_item::ID.eq(widget_id))
        .first()
        .await
        .unwrap()
        .expect("widget row");
    assert_eq!(widget.qty, 99, "widget qty persisted");
    assert_eq!(widget.product, "Widget", "unmentioned child cols preserved");

    let gadget = OrderItem::objects()
        .filter(order_item::ID.eq(gadget_id))
        .first()
        .await
        .unwrap()
        .expect("gadget row");
    assert_eq!(gadget.qty, 1, "child absent from payload left untouched");

    // Order A now owns 3 items (widget, gadget, sprocket) — no implicit delete.
    assert_eq!(
        OrderItem::objects()
            .filter(order_item::ORDER.eq(a_id))
            .count()
            .await
            .unwrap(),
        3,
        "no children were deleted; the new one was added"
    );

    // --- Cross-parent guard: PATCH order A with order B's item id → 404,
    //     and B's item must be unchanged (whole PATCH rolled back).
    let (status, _b) = send(
        &router,
        Method::PATCH,
        &format!("/api/order/{a_id}"),
        json!({
            "customer": "should-not-persist",
            "items": [ { "id": b_item_id, "qty": 555 } ]
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a child pk from another parent is a 404"
    );

    // B's item is unchanged...
    let b_item = OrderItem::objects()
        .filter(order_item::ID.eq(b_item_id))
        .first()
        .await
        .unwrap()
        .expect("order B item");
    assert_eq!(b_item.qty, 7, "another parent's child was not mutated");

    // ...and A's parent column change from the failed PATCH rolled back.
    let a = Order::objects()
        .filter(order::ID.eq(a_id))
        .first()
        .await
        .unwrap()
        .expect("order A");
    assert_eq!(
        a.customer, "Ada Lovelace",
        "failed nested PATCH rolled the parent update back too"
    );

    // --- Guard (gaps3 #10): an array under a key that is NOT a column, an M2M
    //     relation, or a declared nested relation must be REJECTED, not
    //     silently dropped. `bogus` is none of those on `order`.
    let before = Order::objects().count().await.unwrap();
    let (status, _b) = send(
        &router,
        Method::POST,
        "/api/order/",
        json!({ "customer": "Hopper", "bogus": [ { "x": 1 } ] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an undeclared nested array is a 400, not a silent drop"
    );
    assert_eq!(
        Order::objects().count().await.unwrap(),
        before,
        "the rejected request created no row"
    );
}
