//! Bulk delete on a soft-delete model goes through `DynQuerySet::delete()`
//! semantics (gaps2 #82 + #35): the rows are soft-deleted (stamped
//! `deleted_at`), not hard-removed, and disappear from the live API.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(soft_delete, table = "note")]
struct Note {
    id: i64,
    body: String,
    deleted_at: Option<DateTime<Utc>>,
}

async fn boot() -> axum::Router {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("bulk_sd.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    let rest = RestPlugin::default()
        .default_permission(AllowAny)
        .resource(ResourceConfig::for_::<Note>().bulk());

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Note>()
        .plugin(rest)
        .build()
        .expect("App::build");

    let pool = umbral::db::pool();
    sqlx::query("CREATE TABLE note (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL, deleted_at TEXT)")
        .execute(&pool)
        .await
        .expect("create note");

    app.into_router()
}

async fn send(router: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
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
async fn bulk_delete_soft_deletes() {
    let router = boot().await;

    let (_s, body) = send(
        &router,
        Method::POST,
        "/api/note/",
        json!([{ "body": "a" }, { "body": "b" }, { "body": "c" }]),
    )
    .await;
    let ids: Vec<i64> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();

    let (status, _b) = send(
        &router,
        Method::DELETE,
        "/api/note/",
        json!({ "ids": [ids[0], ids[1]] }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Live API only sees the one un-deleted note.
    assert_eq!(
        Note::objects().count().await.unwrap(),
        1,
        "two rows soft-deleted out of the live set"
    );

    // The rows still physically exist (soft delete, not hard).
    let physical: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM note")
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap();
    assert_eq!(physical, 3, "rows are soft-deleted (deleted_at stamped), not removed");

    let dead: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM note WHERE deleted_at IS NOT NULL")
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap();
    assert_eq!(dead, 2, "the two deleted rows carry a deleted_at timestamp");
}
