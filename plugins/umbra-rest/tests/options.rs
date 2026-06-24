//! `OPTIONS` on REST resources (gaps2 #98): a resource answers `OPTIONS` with
//! `204 No Content` + an `Allow` header listing its supported verbs — not the
//! bare `405` axum returns for an unregistered method. The collection `Allow`
//! reflects the `.bulk()` opt-in (collection PATCH/DELETE only when enabled).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbra_rest::{AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Gadget {
    id: i64,
    name: String,
} // NO `.bulk()`

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Widget {
    id: i64,
    name: String,
} // `.bulk()`

static ROUTER: OnceCell<axum::Router> = OnceCell::const_new();
async fn boot() -> axum::Router {
    ROUTER.get_or_init(build).await.clone()
}

async fn build() -> axum::Router {
    let settings = umbra::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("rest_options.sqlite");
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
        .resource(ResourceConfig::for_::<Gadget>())
        .resource(ResourceConfig::for_::<Widget>().bulk());

    let app = umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Gadget>()
        .model::<Widget>()
        .plugin(rest)
        .build()
        .expect("App::build");

    let pool = umbra::db::pool();
    for t in ["gadget", "widget"] {
        sqlx::query(&format!(
            "CREATE TABLE {t} (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)"
        ))
        .execute(&pool)
        .await
        .expect("create table");
    }
    app.into_router()
}

async fn options(router: &axum::Router, uri: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let allow = resp
        .headers()
        .get(http::header::ALLOW)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (status, allow)
}

#[tokio::test]
async fn options_collection_non_bulk_is_204_with_get_post_only() {
    let router = boot().await;
    let (status, allow) = options(&router, "/api/gadget").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "OPTIONS answers 204, not 405");
    assert!(
        allow.contains("OPTIONS") && allow.contains("GET") && allow.contains("POST"),
        "collection Allow lists OPTIONS/GET/POST: {allow}"
    );
    assert!(
        !allow.contains("PATCH") && !allow.contains("DELETE"),
        "a non-bulk collection must NOT advertise PATCH/DELETE: {allow}"
    );
}

#[tokio::test]
async fn options_collection_bulk_advertises_patch_delete() {
    let router = boot().await;
    let (status, allow) = options(&router, "/api/widget").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        allow.contains("PATCH") && allow.contains("DELETE"),
        "a bulk-enabled collection advertises PATCH/DELETE: {allow}"
    );
}

#[tokio::test]
async fn options_detail_is_204_with_full_crud() {
    let router = boot().await;
    let (status, allow) = options(&router, "/api/gadget/1").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for m in ["OPTIONS", "GET", "PUT", "PATCH", "DELETE"] {
        assert!(allow.contains(m), "detail Allow should include {m}: {allow}");
    }
}
