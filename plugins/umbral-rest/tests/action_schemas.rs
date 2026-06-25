//! Custom `@action` input/output schemas (feature #60). A declared input
//! schema validates the request body before the handler runs; the schemas
//! are exposed for OpenAPI emission.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

use umbral_rest::{ActionContext, ActionError, ActionScope, AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Doc {
    id: i64,
    title: String,
}

async fn boot() -> axum::Router {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("actions.sqlite");
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

    let resource =
        ResourceConfig::for_::<Doc>()
            .action(
                "publish",
                Method::POST,
                ActionScope::Detail,
                |_ctx: ActionContext| async move {
                    Ok::<Value, ActionError>(json!({ "published": true }))
                },
            )
            .action_input_schema(
                "publish",
                json!({
                    "type": "object",
                    "required": ["note"],
                    "properties": {
                        "note": { "type": "string" },
                        "channel": { "type": "string", "enum": ["stable", "beta"] }
                    }
                }),
            )
            .action_output_schema(
                "publish",
                json!({ "type": "object", "properties": { "published": { "type": "boolean" } } }),
            );

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Doc>()
        .plugin(
            RestPlugin::default()
                .default_permission(AllowAny)
                .resource(resource),
        )
        .build()
        .expect("App::build");

    let pool = umbral::db::pool();
    sqlx::query("CREATE TABLE doc (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create doc");

    app.into_router()
}

async fn post(router: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn action_input_schema_validates_and_is_exposed() {
    let router = boot().await;

    // Valid body → the handler runs.
    let (status, body) = post(
        &router,
        "/api/doc/1/publish/",
        json!({ "note": "shipping it", "channel": "stable" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "valid body passes: {body}");
    assert_eq!(body["published"], true);

    // Missing required `note` → 400 before the handler.
    let (status, _) = post(&router, "/api/doc/1/publish/", json!({})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "missing required field is rejected"
    );

    // Wrong type for `note` → 400.
    let (status, _) = post(&router, "/api/doc/1/publish/", json!({ "note": 123 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "wrong type is rejected");

    // Bad enum value for `channel` → 400.
    let (status, _) = post(
        &router,
        "/api/doc/1/publish/",
        json!({ "note": "ok", "channel": "nightly" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "bad enum value is rejected"
    );

    // The schemas are exposed for OpenAPI.
    let schemas = umbral_rest::registered_action_schemas();
    let publish = schemas
        .iter()
        .find(|a| a.name == "publish")
        .expect("publish action exposed");
    assert_eq!(publish.table, "doc");
    assert_eq!(publish.method, "POST");
    assert!(publish.detail, "publish is detail-scope");
    assert!(publish.input_schema.is_some(), "input schema exposed");
    assert!(publish.output_schema.is_some(), "output schema exposed");
}
