//! Permission-gated `@action` coverage. Lives in its own test binary
//! because the resource's permission is plugin-wide and we want a
//! different permission than the default-permissioned binary in
//! `actions.rs`.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{ActionScope, IsAuthenticated, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("actions_gated.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let resource = ResourceConfig::new("post")
            .permission(IsAuthenticated)
            .action(
                "publish",
                Method::POST,
                ActionScope::Detail,
                |_ctx| async move { Ok(json!({ "should": "not reach" })) },
            );
        let rest = RestPlugin::default().resource(resource);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .plugin(rest)
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

async fn run(router: axum::Router, method: Method, uri: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    router.oneshot(req).await.expect("oneshot").status()
}

/// An anonymous request to a custom action gated by `IsAuthenticated`
/// returns 401, proving the permission gate runs for `Action::Custom`
/// the same way it does for the built-in CRUD actions.
#[tokio::test]
async fn anonymous_custom_action_is_rejected_when_gated() {
    let router = boot().await.clone();
    let status = run(router, Method::POST, "/api/post/1/publish/").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
