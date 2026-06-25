//! End-to-end tests for the pagination envelope on `/api/<table>/`.
//!
//! Drives a real axum router against an in-memory SQLite, seeds rows,
//! and inspects the envelope each built-in produces. Lives in its own
//! test binary so the App can boot with a custom `RestPlugin`
//! configuration (the framework's settings OnceLock only allows one
//! App boot per process; the existing `integration.rs` already uses
//! NoPagination, so PageNumber + LimitOffset get their own binary
//! each via separate test files would be ideal — for v1 this single
//! file picks PageNumber and asserts the envelope shape there).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{PageNumberPagination, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Note {
    id: i64,
    title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("pagination.sqlite");
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
            .model::<Note>()
            .plugin(
                RestPlugin::default()
                    .paginate(PageNumberPagination::new(10).with_max_page_size(50)),
            )
            .build()
            .expect("App::build with PageNumber pagination");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE note (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create note table");

        // 25 rows so paging shows three pages at page_size=10.
        for i in 1..=25 {
            sqlx::query("INSERT INTO note (title) VALUES (?)")
                .bind(format!("note {i}"))
                .execute(&pool)
                .await
                .expect("seed");
        }

        app.into_router()
    })
    .await
}

async fn get_json(router: axum::Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).expect("valid json");
    (status, parsed)
}

// =====================================================================
// PageNumberPagination — envelope shape.
// =====================================================================

#[tokio::test]
async fn list_returns_first_page_with_envelope() {
    let app = boot().await.clone();
    let (status, body) = get_json(app, "/api/note/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 25, "total row count");
    assert_eq!(body["total_pages"], 3);
    assert_eq!(body["current_page"], 1);
    assert_eq!(body["page_size"], 10);
    assert_eq!(body["previous"], Value::Null);
    assert_eq!(body["next"], 2);
    assert_eq!(body["results"].as_array().unwrap().len(), 10);
}

#[tokio::test]
async fn page_query_param_skips_rows() {
    let app = boot().await.clone();
    let (status, body) = get_json(app, "/api/note/?page=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current_page"], 2);
    assert_eq!(body["previous"], 1);
    assert_eq!(body["next"], 3);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 10);
    // First row on page 2 should be note 11 (1-indexed).
    assert_eq!(results[0]["title"], "note 11");
}

#[tokio::test]
async fn last_page_has_null_next_and_partial_results() {
    let app = boot().await.clone();
    let (status, body) = get_json(app, "/api/note/?page=3").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current_page"], 3);
    assert_eq!(body["next"], Value::Null);
    assert_eq!(body["previous"], 2);
    // Last page has 5 rows (25 - 20 = 5).
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 5);
}

#[tokio::test]
async fn page_size_query_param_overrides_default() {
    let app = boot().await.clone();
    let (status, body) = get_json(app, "/api/note/?page=1&page_size=5").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["page_size"], 5);
    assert_eq!(body["total_pages"], 5); // ceil(25/5)
    assert_eq!(body["results"].as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn page_size_clamps_to_max() {
    let app = boot().await.clone();
    let (status, body) = get_json(app, "/api/note/?page_size=9999").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["page_size"], 50, "clamped to max_page_size");
}

#[tokio::test]
async fn invalid_page_param_falls_back_to_first_page() {
    let app = boot().await.clone();
    let (status, body) = get_json(app, "/api/note/?page=garbage").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current_page"], 1);
}
