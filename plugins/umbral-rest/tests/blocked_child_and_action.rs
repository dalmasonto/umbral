//! Block-list enforcement on the two paths that previously skipped it
//! (audit_2 plugin-rest L-6 / L-7). One `App::build` per test binary, so
//! both findings share a single booted app:
//!
//! * L-6 — a `.nested(...)` child that is NOT allowed (here `.exclude`d)
//!   must be rejected before any write, so a nested payload can't reach a
//!   table whose own `/api/<table>/` endpoint 404s.
//! * L-7 — a custom `@action` registered on a blocked/excluded table must
//!   404 at dispatch, exactly like the CRUD handlers, rather than staying
//!   reachable through its still-mounted route.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral::orm::ForeignKey;
use umbral_rest::{ActionScope, AllowAny, ResourceConfig, RestPlugin};

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
    let path = tmp.path().join("blocked_child.sqlite");
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

    // Order is writable and declares a nested child `order_item`. But
    // `order_item` is BOTH `.exclude`d (so its own endpoint 404s) AND
    // carries a custom `@action`. The nested write and the action must
    // both honour the block-list.
    let rest = RestPlugin::default()
        .default_permission(AllowAny)
        .exclude(["order_item"])
        .resource(ResourceConfig::for_::<Order>().nested("items", "order_item"))
        .resource(ResourceConfig::for_::<OrderItem>().action(
            "ping",
            Method::POST,
            ActionScope::Detail,
            |ctx| async move { Ok(json!({ "pong": ctx.pk })) },
        ));

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

/// Process-wide single boot: `App::build` / settings init can run only
/// once per test binary, so both tests share one router.
static ROUTER: OnceCell<axum::Router> = OnceCell::const_new();

async fn router() -> axum::Router {
    ROUTER.get_or_init(boot).await.clone()
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
async fn nested_child_on_blocked_table_is_rejected() {
    let router = router().await;

    // Sanity: the child's own endpoint is blocked.
    let (status, _b) = send(&router, Method::GET, "/api/order_item/", Value::Null).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "excluded order_item endpoint must 404"
    );

    // L-6: a nested write targeting the excluded child must be rejected,
    // and no rows may be written.
    let before = Order::objects().count().await.unwrap();
    let (status, body) = send(
        &router,
        Method::POST,
        "/api/order/",
        json!({
            "customer": "Ada",
            "items": [ { "product": "Widget", "qty": 2 } ]
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "nested write to a blocked child must be rejected: {body}"
    );
    assert_eq!(
        Order::objects().count().await.unwrap(),
        before,
        "the rejected nested write created no parent row either"
    );
    assert_eq!(OrderItem::objects().count().await.unwrap(), 0);
}

#[tokio::test]
async fn custom_action_on_blocked_table_404s() {
    let router = router().await;

    // L-7: the action route is still mounted, but dispatch must re-check
    // the block-list and 404 — the action is on an excluded table.
    let (status, _b) = send(
        &router,
        Method::POST,
        "/api/order_item/1/ping/",
        Value::Null,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an @action on a blocked table must not stay reachable"
    );
}
