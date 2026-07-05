//! Accept-header API versioning (gaps2 #82): with
//! `versioning(VersioningScheme::accept_header()).allowed_versions(["v1","v2"])`,
//! paths stay `/api/<table>/` and the version comes from the `Accept`
//! header's `version` media-type param.
//!
//! - `Accept: application/json; version=v2` resolves to v2 on the context.
//! - An absent version falls back to `default_version` ("v1").
//! - A version outside `allowed_versions` → 406 Not Acceptable.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{
    ActionScope, AllowAny, ResourceConfig, RestPlugin, VersioningConfig, VersioningScheme,
};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("versioning_accept.sqlite");
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

        let post_cfg = ResourceConfig::new("post").action(
            "whoami",
            Method::GET,
            ActionScope::Collection,
            |ctx| async move { Ok(json!({ "version": ctx.version })) },
        );

        let rest = RestPlugin::default()
            .default_permission(AllowAny)
            .resource(post_cfg)
            .versioning(
                VersioningConfig::new(VersioningScheme::accept_header())
                    .allowed_versions(["v1", "v2"])
                    .default_version("v1"),
            );

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .plugin(rest)
            .build()
            .expect("App::build with accept-header versioning");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
        )
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

async fn get_with_accept(uri: &str, accept: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(a) = accept {
        builder = builder.header(header::ACCEPT, a);
    }
    let req = builder.body(Body::empty()).unwrap();
    let resp = boot().await.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, parsed)
}

#[tokio::test]
async fn paths_stay_unversioned() {
    // No version segment in the URL — accept-header versioning leaves
    // paths exactly as the unversioned API.
    let (status, body) = get_with_accept("/api/post/", None).await;
    assert_eq!(status, StatusCode::OK, "/api/post/ must resolve: {body}");
}

#[tokio::test]
async fn accept_header_version_resolves_to_v2() {
    let (status, body) =
        get_with_accept("/api/post/whoami/", Some("application/json; version=v2")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["version"],
        json!("v2"),
        "version=v2 in the Accept header must reach ctx.version: {body}"
    );
}

#[tokio::test]
async fn absent_version_falls_back_to_default() {
    let (status, body) = get_with_accept("/api/post/whoami/", Some("application/json")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["version"],
        json!("v1"),
        "absent version must fall back to default_version: {body}"
    );

    // No Accept header at all → same default.
    let (status2, body2) = get_with_accept("/api/post/whoami/", None).await;
    assert_eq!(status2, StatusCode::OK, "{body2}");
    assert_eq!(body2["version"], json!("v1"), "{body2}");
}

#[tokio::test]
async fn unknown_version_is_406() {
    let (status, _body) = get_with_accept("/api/post/", Some("application/json; version=v9")).await;
    assert_eq!(
        status,
        StatusCode::NOT_ACCEPTABLE,
        "a version outside allowed_versions must be 406 (version read from the Accept header)"
    );
}
