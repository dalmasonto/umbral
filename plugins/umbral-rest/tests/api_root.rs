//! The REST API root (`GET /api/`) — a browsable index of exposed
//! resources plus every plugin's advertised `api_endpoints()`. This
//! tests the generic discovery seam: a local plugin contributes an
//! endpoint via `Plugin::api_endpoints()`, and REST surfaces it at the
//! root without naming (or depending on) that plugin. The same path
//! umbral-oauth's provider links travel.

#![allow(dead_code, private_interfaces)]

use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral::plugin::{ApiEndpoint, Plugin};
use umbral_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    title: String,
}

/// A minimal plugin that advertises one client endpoint. Stands in for
/// umbral-oauth so this test exercises the generic seam without a cross-
/// plugin dependency.
struct DiscoveryPlugin;

impl Plugin for DiscoveryPlugin {
    fn name(&self) -> &'static str {
        "discovery_demo"
    }
    fn api_endpoints(&self) -> Vec<ApiEndpoint> {
        vec![ApiEndpoint {
            group: "demo".to_string(),
            name: "google.login".to_string(),
            method: "GET".to_string(),
            path: "/oauth/google/login".to_string(),
            label: "Sign in with Google".to_string(),
        }]
    }
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("api_root.sqlite");
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

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .plugin(DiscoveryPlugin)
            .plugin(RestPlugin::default())
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("create post table");

        app.into_router()
    })
    .await
}

async fn get_json(uri: &str, host: &str) -> (StatusCode, Value) {
    let router = boot().await.clone();
    let req = Request::builder()
        .uri(uri)
        .header("host", host)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

/// The root lists the `post` resource (collection + detail paths) — the
/// allow-filter passes it through because it's a registered model.
#[tokio::test]
async fn root_lists_exposed_resources() {
    let (status, body) = get_json("/api/", "api.example.com").await;
    assert_eq!(status, StatusCode::OK);

    let post = &body["resources"]["post"];
    assert_eq!(post["path"], "/api/post/");
    assert_eq!(post["detail"], "/api/post/{id}");
}

/// The root aggregates the local plugin's advertised endpoint, and joins
/// the request origin into an absolute `url`. This proves the generic
/// `api_endpoints()` seam reaches REST's index without coupling.
#[tokio::test]
async fn root_aggregates_plugin_endpoints() {
    let (status, body) = get_json("/api/", "api.example.com").await;
    assert_eq!(status, StatusCode::OK);

    let endpoints = body["endpoints"].as_array().expect("endpoints array");
    let login = endpoints
        .iter()
        .find(|e| e["name"] == "google.login")
        .expect("aggregated plugin endpoint present");
    assert_eq!(login["group"], "demo");
    assert_eq!(login["path"], "/oauth/google/login");
    assert_eq!(login["label"], "Sign in with Google");
    // Absolute URL joined from the request Host header.
    assert_eq!(login["url"], "http://api.example.com/oauth/google/login");
}
