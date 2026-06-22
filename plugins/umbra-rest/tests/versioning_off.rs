//! Versioning is opt-in (gaps2 #82): a `RestPlugin` with no
//! `.versioning(...)` behaves exactly as before — `/api/<table>/` works,
//! no version segment is required, and `ctx.version` is `None`.

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

use umbra_rest::{ActionScope, AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Post {
    id: i64,
    title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("versioning_off.sqlite");
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

        let post_cfg = ResourceConfig::new("post").action(
            "whoami",
            Method::GET,
            ActionScope::Collection,
            |ctx| async move { Ok(json!({ "version": ctx.version })) },
        );

        // No `.versioning(...)` — the default, unversioned API.
        let rest = RestPlugin::default()
            .default_permission(AllowAny)
            .resource(post_cfg);

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .plugin(rest)
            .build()
            .expect("App::build without versioning");

        let pool = umbra::db::pool();
        sqlx::query("CREATE TABLE post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("create post");
        sqlx::query("INSERT INTO post (title) VALUES ('hello')")
            .execute(&pool)
            .await
            .expect("seed post");

        app.into_router()
    })
    .await
}

async fn get_json(uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = boot().await.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, parsed)
}

#[tokio::test]
async fn unversioned_path_still_works() {
    let (status, body) = get_json("/api/post/").await;
    assert_eq!(status, StatusCode::OK, "/api/post/ must still resolve: {body}");
    assert_eq!(body["results"][0]["title"], json!("hello"), "{body}");
}

#[tokio::test]
async fn version_is_none_when_versioning_is_off() {
    let (status, body) = get_json("/api/post/whoami/").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["version"],
        Value::Null,
        "ctx.version must be null when versioning is off: {body}"
    );
}

#[tokio::test]
async fn a_versioned_path_404s_when_versioning_is_off() {
    // /api/v1/post/ is just an unknown table `v1` with no such resource.
    let (status, _body) = get_json("/api/v1/post/").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no version routing exists when versioning is off"
    );
}
