//! REST POST / PATCH writes the M2M arrays through to the
//! auto-generated junction table. The user reported that
//! `POST /api/post/ { "tags": [1] }` returned a 201 but the
//! junction stayed empty — this test pins the fixed behaviour:
//!
//! 1. A valid array on POST lands in `<table>_<field>`.
//! 2. The response JSON echoes the persisted ids back so the
//!    client can verify without a follow-up GET.
//! 3. PATCH replaces the entire set (wipe + re-insert inside a
//!    transaction); empty array clears all relations.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbra::orm::{M2M, SqlType, load_junction_selection};
use umbra_rest::RestPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Tag {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Post {
    id: i64,
    title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    tags: M2M<Tag>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("m2m_writethrough.sqlite");
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

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Tag>()
            .model::<Post>()
            .plugin(RestPlugin::default())
            .build()
            .expect("App::build");

        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE tag (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create tag");
        sqlx::query(
            "CREATE TABLE post (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create post");
        // Junction. The macro convention is `<parent>_<field>`,
        // so for `Post.tags` that's `post_tags`.
        sqlx::query(
            "CREATE TABLE post_tags (\
                parent_id INTEGER NOT NULL REFERENCES post(id),\
                child_id INTEGER NOT NULL REFERENCES tag(id),\
                PRIMARY KEY (parent_id, child_id)\
             )",
        )
        .execute(&pool)
        .await
        .expect("create post_tags");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("enable fks");

        // Seed three tags through the typed Manager — the
        // ORM owns inserts; tests should not.
        Tag::objects()
            .bulk_create(vec![
                Tag { id: 0, name: "rust".into() },
                Tag { id: 0, name: "web".into() },
                Tag { id: 0, name: "database".into() },
            ])
            .await
            .expect("seed tags");

        app.into_router()
    })
    .await
}

async fn post_json(router: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).expect("valid json");
    (status, parsed)
}

async fn patch_json(router: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("PATCH")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).expect("valid json");
    (status, parsed)
}

/// Read the junction rows through the ORM's public dynamic
/// helper (no raw SQL in the test). Returns child ids parsed
/// back to `i64` and sorted so assertions don't depend on
/// insert order.
async fn junction_rows_for(post_id: i64) -> Vec<i64> {
    let parent = sea_query::Value::BigInt(Some(post_id));
    let mut ids: Vec<i64> = load_junction_selection("post_tags", parent, SqlType::BigInt)
        .await
        .expect("read junction")
        .into_iter()
        .map(|s| s.parse::<i64>().expect("child_id is i64"))
        .collect();
    ids.sort();
    ids
}

// =========================================================================

#[tokio::test]
async fn post_writes_m2m_ids_into_the_junction_table() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/post/",
        json!({ "title": "hello", "tags": [1, 2] }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "got body: {body}");
    let post_id = body["id"].as_i64().expect("id in response");
    assert_eq!(junction_rows_for(post_id).await, vec![1, 2]);
}

#[tokio::test]
async fn post_response_echoes_the_persisted_tag_ids() {
    let router = boot().await.clone();
    let (_, body) = post_json(
        router,
        "/api/post/",
        json!({ "title": "echo", "tags": [2, 3] }),
    )
    .await;
    let echoed = body["tags"]
        .as_array()
        .expect("tags array should be in the response; got {body}");
    let ids: Vec<i64> = echoed.iter().filter_map(|v| v.as_i64()).collect();
    assert_eq!(ids, vec![2, 3]);
}

#[tokio::test]
async fn patch_replaces_the_full_set_of_tags() {
    let router = boot().await.clone();
    let (_, created) = post_json(
        router.clone(),
        "/api/post/",
        json!({ "title": "patch-me", "tags": [1, 2] }),
    )
    .await;
    let post_id = created["id"].as_i64().unwrap();
    assert_eq!(junction_rows_for(post_id).await, vec![1, 2]);

    let (status, _patched) = patch_json(
        router,
        &format!("/api/post/{post_id}"),
        json!({ "tags": [3] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Wipe + re-insert: only the new id remains.
    assert_eq!(junction_rows_for(post_id).await, vec![3]);
}

#[tokio::test]
async fn patch_with_empty_array_clears_all_tags() {
    let router = boot().await.clone();
    let (_, created) = post_json(
        router.clone(),
        "/api/post/",
        json!({ "title": "clear-me", "tags": [1, 2, 3] }),
    )
    .await;
    let post_id = created["id"].as_i64().unwrap();

    let (status, _) = patch_json(
        router,
        &format!("/api/post/{post_id}"),
        json!({ "tags": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(junction_rows_for(post_id).await.is_empty());
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

#[tokio::test]
async fn get_response_includes_the_m2m_arrays() {
    let router = boot().await.clone();
    let (_, created) = post_json(
        router.clone(),
        "/api/post/",
        json!({ "title": "for-read", "tags": [1, 3] }),
    )
    .await;
    let post_id = created["id"].as_i64().unwrap();

    // Single-row GET.
    let (status, body) = get_json(router.clone(), &format!("/api/post/{post_id}")).await;
    assert_eq!(status, StatusCode::OK);
    let echoed = body["tags"]
        .as_array()
        .expect("tags array on GET response; got {body}");
    let ids: Vec<i64> = echoed.iter().filter_map(|v| v.as_i64()).collect();
    assert_eq!(ids, vec![1, 3]);
}

#[tokio::test]
async fn patch_readback_includes_the_updated_m2m_arrays() {
    let router = boot().await.clone();
    let (_, created) = post_json(
        router.clone(),
        "/api/post/",
        json!({ "title": "readback", "tags": [1] }),
    )
    .await;
    let post_id = created["id"].as_i64().unwrap();

    // PATCH replaces the tag set; the response (which goes
    // through fetch_rows after the UPDATE) must include the
    // NEW arrays, not the old ones and not nothing at all.
    let (status, patched) = patch_json(
        router,
        &format!("/api/post/{post_id}"),
        json!({ "tags": [2, 3] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let echoed = patched["tags"]
        .as_array()
        .expect("tags array on PATCH response; got {patched}");
    let ids: Vec<i64> = echoed.iter().filter_map(|v| v.as_i64()).collect();
    assert_eq!(ids, vec![2, 3]);
}

#[tokio::test]
async fn post_with_a_missing_tag_id_is_rejected_before_the_junction_write() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/post/",
        json!({ "title": "ghosts", "tags": [999] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    assert_eq!(body["code"], "validation_error");
    let tag_errs = body["tags"].as_array().expect("tags errors; got {body}");
    let msg = tag_errs[0].as_str().unwrap_or("");
    assert!(msg.contains("999") && msg.contains("not exist"), "got {msg:?}");
}
