//! End-to-end tests for `?ordering=` on the REST list endpoint.
//!
//! Verifies that the comma-separated ordering param is actually
//! applied: ascending, descending, multi-field, and that unknown fields
//! are silently ignored (degrading to default DB order rather than 400).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ord_item")]
struct Item {
    id: i64,
    name: String,
    score: i64,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ordering.sqlite");
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

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Item>()
            .plugin(RestPlugin::default())
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE ord_item (\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\
                 name TEXT NOT NULL,\
                 score INTEGER NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create ord_item table");

        // Seed rows with deliberately non-sequential (name, score) so order is observable.
        sqlx::query(
            "INSERT INTO ord_item (name, score) VALUES \
             ('banana', 30),\
             ('apple',  10),\
             ('cherry', 20)",
        )
        .execute(&pool)
        .await
        .expect("seed ord_item");

        app.into_router()
    })
    .await
}

async fn get_results(router: axum::Router, uri: &str) -> (StatusCode, Vec<Value>) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let results = body["results"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    (status, results)
}

// =========================================================================
// Ascending order by text field
// =========================================================================

#[tokio::test]
async fn ordering_asc_by_name_returns_alphabetical_order() {
    let router = boot().await.clone();
    let (status, rows) = get_results(router, "/api/ord_item/?ordering=name").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows.len(), 3, "expected all 3 rows");
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap_or("")).collect();
    assert_eq!(names, vec!["apple", "banana", "cherry"], "ascending name order: {names:?}");
}

// =========================================================================
// Descending order by text field
// =========================================================================

#[tokio::test]
async fn ordering_desc_by_name_returns_reverse_alphabetical() {
    let router = boot().await.clone();
    let (status, rows) = get_results(router, "/api/ord_item/?ordering=-name").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows.len(), 3, "expected all 3 rows");
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap_or("")).collect();
    assert_eq!(names, vec!["cherry", "banana", "apple"], "descending name order: {names:?}");
}

// =========================================================================
// Ascending order by integer field
// =========================================================================

#[tokio::test]
async fn ordering_asc_by_score_returns_lowest_first() {
    let router = boot().await.clone();
    let (status, rows) = get_results(router, "/api/ord_item/?ordering=score").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows.len(), 3);
    let scores: Vec<i64> = rows.iter().map(|r| r["score"].as_i64().unwrap_or(0)).collect();
    assert_eq!(scores, vec![10, 20, 30], "ascending score order: {scores:?}");
}

// =========================================================================
// Descending order by integer field
// =========================================================================

#[tokio::test]
async fn ordering_desc_by_score_returns_highest_first() {
    let router = boot().await.clone();
    let (status, rows) = get_results(router, "/api/ord_item/?ordering=-score").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows.len(), 3);
    let scores: Vec<i64> = rows.iter().map(|r| r["score"].as_i64().unwrap_or(0)).collect();
    assert_eq!(scores, vec![30, 20, 10], "descending score order: {scores:?}");
}

// =========================================================================
// Unknown field is silently ignored (degrades to default order, not 400)
// =========================================================================

#[tokio::test]
async fn ordering_unknown_field_is_silently_ignored_and_returns_200() {
    let router = boot().await.clone();
    let (status, rows) = get_results(router, "/api/ord_item/?ordering=does_not_exist").await;
    // Must be 200 with all 3 rows — unknown fields silently drop, no 400.
    assert_eq!(status, StatusCode::OK, "unknown ordering field must not produce a 400");
    assert_eq!(rows.len(), 3, "all rows still returned when ordering field unknown");
}
