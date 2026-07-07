//! Bulk endpoints (gaps2 #82) — happy path + atomicity +
//! back-compat for the `.bulk()`-opted-in resource. One App::build per
//! test binary (umbral-core's settings/pool OnceLock is one-shot), so the
//! off-by-default / security / soft-delete cases live in sibling files
//! (`bulk_off.rs`, `bulk_security.rs`, `bulk_softdelete.rs`).

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
use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Widget {
    id: i64,
    name: String,
    qty: i32,
}

// One App::build per test binary (settings/pool OnceLock is one-shot). The
// router is shared across every `#[tokio::test]` in this file; each test
// uses disjoint rows so they don't interfere.
static ROUTER: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> axum::Router {
    ROUTER.get_or_init(build).await.clone()
}

async fn build() -> axum::Router {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("bulk.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
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
        .resource(ResourceConfig::for_::<Widget>().bulk());

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Widget>()
        .plugin(rest)
        .build()
        .expect("App::build");

    let pool = umbral::db::pool();
    sqlx::query(
        "CREATE TABLE widget (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, qty INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("create widget");

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

// Count rows whose name starts with `prefix` — each test namespaces its
// data so the shared DB doesn't make the assertions interfere. Raw SQL is
// fine in a test (the no-raw-SQL rule is about plugin SRC, not tests).
async fn count_prefix(prefix: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM widget WHERE name LIKE ?")
        .bind(format!("{prefix}%"))
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn bulk_create_array_then_single_object_back_compat() {
    let router = boot().await;

    // Bulk create: POST an array of 3 → 201, 3 rows, all present.
    let (status, body) = send(
        &router,
        Method::POST,
        "/api/widget/",
        json!([
            { "name": "bc-a", "qty": 1 },
            { "name": "bc-b", "qty": 2 },
            { "name": "bc-c", "qty": 3 }
        ]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "bulk create returns 201; {body}"
    );
    let arr = body.as_array().expect("array body");
    assert_eq!(arr.len(), 3, "three rows echoed back");
    assert_eq!(arr[0]["name"], "bc-a");
    assert!(
        arr[0]["id"].as_i64().is_some(),
        "each row carries its new id"
    );
    assert_eq!(count_prefix("bc-").await, 3, "three rows landed in the DB");

    // Back-compat: a single JSON OBJECT still does an ordinary single
    // create returning the one object (not wrapped in an array).
    let (status, body) = send(
        &router,
        Method::POST,
        "/api/widget/",
        json!({ "name": "bc-solo", "qty": 9 }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "single create still 201; {body}"
    );
    assert!(
        body.is_object(),
        "single create returns an object, not an array"
    );
    assert_eq!(body["name"], "bc-solo");
    assert_eq!(count_prefix("bc-").await, 4);
}

#[tokio::test]
async fn bulk_create_is_atomic() {
    let router = boot().await;

    // Item index 1 is invalid (`name` is NOT NULL, sent as null).
    let (status, _body) = send(
        &router,
        Method::POST,
        "/api/widget/",
        json!([
            { "name": "atom-ok", "qty": 1 },
            { "name": null, "qty": 2 },
            { "name": "atom-also-ok", "qty": 3 }
        ]),
    )
    .await;
    assert!(
        status.is_client_error(),
        "an invalid item fails the batch (got {status})"
    );
    assert_eq!(
        count_prefix("atom-").await,
        0,
        "ZERO rows created — the whole transaction rolled back"
    );
}

#[tokio::test]
async fn bulk_update_array_then_atomic() {
    let router = boot().await;

    // Seed three rows.
    let (_s, body) = send(
        &router,
        Method::POST,
        "/api/widget/",
        json!([
            { "name": "x", "qty": 10 },
            { "name": "y", "qty": 20 },
            { "name": "z", "qty": 30 }
        ]),
    )
    .await;
    let ids: Vec<i64> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();

    // Bulk update: PATCH the collection with an array carrying each PK.
    let (status, body) = send(
        &router,
        Method::PATCH,
        "/api/widget/",
        json!([
            { "id": ids[0], "qty": 100 },
            { "id": ids[1], "qty": 200 }
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bulk update returns 200; {body}");
    let arr = body.as_array().expect("array body");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["qty"], 100, "row read back reflects the update");
    assert_eq!(arr[1]["qty"], 200);

    // Atomicity: one item names a PK that doesn't exist → rollback, none
    // of the items in THIS batch are applied.
    let (status, _body) = send(
        &router,
        Method::PATCH,
        "/api/widget/",
        json!([
            { "id": ids[2], "qty": 999 },
            { "id": 999999, "qty": 12345 }
        ]),
    )
    .await;
    assert!(
        status.is_client_error(),
        "a bad PK fails the batch (got {status})"
    );

    // The good item in the failed batch was rolled back — z still 30.
    let (_s, row) = send(
        &router,
        Method::GET,
        &format!("/api/widget/{}", ids[2]),
        Value::Null,
    )
    .await;
    assert_eq!(row["qty"], 30, "the rolled-back update never took effect");
}

#[tokio::test]
async fn bulk_delete_ids() {
    let router = boot().await;

    let (_s, body) = send(
        &router,
        Method::POST,
        "/api/widget/",
        json!([
            { "name": "del-1", "qty": 1 },
            { "name": "del-2", "qty": 2 },
            { "name": "del-keep", "qty": 3 }
        ]),
    )
    .await;
    let ids: Vec<i64> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(count_prefix("del-").await, 3);

    let (status, _body) = send(
        &router,
        Method::DELETE,
        "/api/widget/",
        json!({ "ids": [ids[0], ids[1]] }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "bulk delete returns 204");
    assert_eq!(count_prefix("del-").await, 1, "the two named rows are gone");

    // The unnamed row survived.
    let (status, _row) = send(
        &router,
        Method::GET,
        &format!("/api/widget/{}", ids[2]),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the un-named row was kept");
}

#[tokio::test]
async fn bulk_over_cap_is_rejected() {
    let router = boot().await;
    let before = count_prefix("cap-").await;

    // 1001 items — one past the 1000 ceiling.
    let items: Vec<Value> = (0..1001)
        .map(|i| json!({ "name": format!("cap-{i}"), "qty": i }))
        .collect();
    let (status, _body) = send(&router, Method::POST, "/api/widget/", Value::Array(items)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an over-cap batch is rejected before any DB write"
    );
    assert_eq!(count_prefix("cap-").await, before, "nothing was written");
}
