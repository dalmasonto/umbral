//! `ResourceConfig::action_by(...)` (gaps4 #80) must appear in the
//! generated OpenAPI spec exactly like a plain `.action()` does — same
//! registry (`umbral_rest::registered_action_schemas()`), same
//! `OpenApiPlugin` spec builder, just a different path shape (no
//! `/<name>/` suffix, keyed by the lookup column instead of `{id}`).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_openapi::OpenApiPlugin;
use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Community {
    id: i64,
    #[umbral(unique)]
    slug: String,
    name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("openapi_action_by.sqlite");
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

        let resource =
            ResourceConfig::new("community").action_by("slug", Method::GET, |ctx| async move {
                Ok(json!({ "community": ctx.resolved_row }))
            });

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Community>()
            .plugin(
                RestPlugin::default()
                    .default_permission(AllowAny)
                    .resource(resource),
            )
            .plugin(OpenApiPlugin::default())
            .build()
            .expect("App::build with RestPlugin + OpenApiPlugin");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        app.into_router()
    })
    .await
}

async fn get_request(router: axum::Router, uri: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The by-field action gets its own path item, keyed by the lookup column
/// (`{slug}`), NOT `/api/community/{id}/slug/` — the shape a plain
/// `.action()` would have produced.
#[tokio::test]
async fn action_by_appears_in_the_openapi_spec_keyed_by_the_lookup_column() {
    let router = boot().await.clone();
    let (status, body) = get_request(router, "/openapi/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let paths = v["paths"].as_object().expect("paths is an object");

    assert!(
        paths.contains_key("/api/community/{slug}/"),
        "expected /api/community/{{slug}}/ in the spec; got {:?}",
        paths.keys().collect::<Vec<_>>()
    );
    assert!(
        !paths.contains_key("/api/community/{id}/slug/"),
        "action_by must not use the plain .action() path shape"
    );

    let item = &paths["/api/community/{slug}/"];
    let get_op = &item["get"];
    assert!(get_op.is_object(), "GET operation present: {get_op}");
    let params = get_op["parameters"]
        .as_array()
        .expect("parameters is an array");
    assert_eq!(params.len(), 1);
    assert_eq!(params[0]["name"], json!("slug"));
    assert_eq!(params[0]["in"], json!("path"));
}

/// The registered model's ordinary CRUD paths still appear too —
/// `.action_by` is additive to the resource's REST surface, not a
/// replacement of its documentation.
#[tokio::test]
async fn ordinary_crud_paths_still_documented_alongside_action_by() {
    let router = boot().await.clone();
    let (_, body) = get_request(router, "/openapi/openapi.json").await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let paths = v["paths"].as_object().expect("paths");
    assert!(paths.contains_key("/api/community/"));
    assert!(paths.contains_key("/api/community/{id}"));
}
