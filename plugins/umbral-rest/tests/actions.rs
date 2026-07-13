//! End-to-end tests for custom-action endpoints registered
//! through `ResourceConfig::action(...)`.
//!
//! All tests share one booted app (process-wide `OnceLock` state in
//! `umbral-core` doesn't tolerate `App::build()` being called twice
//! from the same test binary). The boot registers ONE
//! `ResourceConfig` with every action variant the suite exercises;
//! each test drives the merged router against the URL it cares
//! about. Permission-gated coverage lives in its own binary
//! (`actions_gated.rs`) because the resource's permission setting is
//! plugin-wide.

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

use umbral_rest::{ActionScope, AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    title: String,
    published: bool,
}

fn build_resource() -> ResourceConfig {
    ResourceConfig::new("post")
        .action(
            "recent",
            Method::GET,
            ActionScope::Collection,
            |ctx| async move {
                Ok(json!({
                    "kind": "recent",
                    "table": ctx.table,
                    "limit": ctx.query.get("limit").cloned().unwrap_or_default(),
                }))
            },
        )
        .action(
            "publish",
            Method::POST,
            ActionScope::Detail,
            |ctx| async move {
                let id: i64 = ctx
                    .pk
                    .as_deref()
                    .unwrap_or("0")
                    .parse()
                    .map_err(|_| umbral_rest::ActionError::BadInput("bad id".into()))?;
                Ok(json!({ "published": id, "name": ctx.name }))
            },
        )
        .action("tag", Method::POST, ActionScope::Detail, |ctx| async move {
            let tag = ctx
                .body
                .get("tag")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(json!({ "id": ctx.pk, "tag": tag }))
        })
        .action(
            "reject_me",
            Method::POST,
            ActionScope::Collection,
            |_ctx| async move { Err(umbral_rest::ActionError::BadInput("nope".into())) },
        )
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("actions.sqlite");
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

        let rest = RestPlugin::default()
            .default_permission(AllowAny)
            .resource(build_resource());

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        app.into_router()
    })
    .await
}

async fn run(
    router: axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req_builder = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(b) => req_builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => req_builder.body(Body::empty()).unwrap(),
    };
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Collection-scope action mounts at `/api/<table>/<name>/`.
#[tokio::test]
async fn collection_action_mounts_and_dispatches() {
    let router = boot().await.clone();

    let (status, body) = run(router, Method::GET, "/api/post/recent/?limit=5", None).await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    assert_eq!(body["kind"], json!("recent"));
    assert_eq!(body["table"], json!("post"));
    assert_eq!(body["limit"], json!("5"));
}

/// Trailing slash is optional.
#[tokio::test]
async fn collection_action_accepts_no_trailing_slash() {
    let router = boot().await.clone();

    let (status, body) = run(router, Method::GET, "/api/post/recent", None).await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    assert_eq!(body["kind"], json!("recent"));
}

/// Detail-scope action gets the `id` segment as `ctx.pk`.
#[tokio::test]
async fn detail_action_mounts_and_receives_pk() {
    let router = boot().await.clone();

    let (status, body) = run(router, Method::POST, "/api/post/42/publish/", None).await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    assert_eq!(body["published"], json!(42));
    assert_eq!(body["name"], json!("publish"));
}

/// The JSON body is parsed and exposed on `ctx.body`.
#[tokio::test]
async fn action_receives_json_body() {
    let router = boot().await.clone();

    let (status, body) = run(
        router,
        Method::POST,
        "/api/post/7/tag/",
        Some(json!({ "tag": "featured" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    assert_eq!(body["tag"], json!("featured"));
    assert_eq!(body["id"], json!("7"));
}

/// `ActionError::BadInput` surfaces as HTTP 400 with the message in
/// the JSON envelope.
#[tokio::test]
async fn action_handler_can_signal_bad_input() {
    let router = boot().await.clone();

    let (status, body) = run(router, Method::POST, "/api/post/reject_me/", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body was {body}");
    assert_eq!(body["error"], json!("nope"));
}
