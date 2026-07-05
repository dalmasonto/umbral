//! End-to-end test for `RestPlugin::resource(ResourceConfig)`.
//!
//! Verifies that customization registered through a `ResourceConfig`
//! built in a **different module** (here, a free function defined
//! outside `boot()`) produces the same outbound JSON shape as the
//! per-call builders did before. The point of `ResourceConfig` is to
//! enable that split-file pattern; this test pins it.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct User {
    id: i64,
    username: String,
    email: String,
    password_hash: String,
    first_name: String,
    last_name: String,
}

/// The kind of function a plugin / module / `serializers.rs`-style
/// file would expose. Lives outside `boot()` so the test exercises
/// the cross-module split that's the whole point of `ResourceConfig`.
fn user_rest_resource() -> ResourceConfig {
    ResourceConfig::new("user")
        .hide("password_hash")
        .transform("email", |v| {
            let s = v.as_str().unwrap_or("");
            match s.split_once('@') {
                Some((_, d)) => json!(format!("***@{d}")),
                None => v.clone(),
            }
        })
        .computed("display_name", |row| {
            let f = row.get("first_name").and_then(|v| v.as_str()).unwrap_or("");
            let l = row.get("last_name").and_then(|v| v.as_str()).unwrap_or("");
            json!(format!("{f} {l}").trim().to_string())
        })
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("resource_config.sqlite");
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

        // The key call: customization bundled in a ResourceConfig from
        // another module, registered via `.resource(...)`. No
        // per-call `.hide` / `.transform` / `.computed` at the
        // construction site.
        let rest = RestPlugin::default().resource(user_rest_resource());

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<User>()
            .plugin(rest)
            .build()
            .expect("App::build with ResourceConfig");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE user (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL,\
                email TEXT NOT NULL,\
                password_hash TEXT NOT NULL,\
                first_name TEXT NOT NULL,\
                last_name TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create user table");

        sqlx::query(
            "INSERT INTO user (username, email, password_hash, first_name, last_name) \
             VALUES ('alice', 'alice@example.com', 'argon2:hidden', 'Alice', 'Doe')",
        )
        .execute(&pool)
        .await
        .expect("seed");

        app.into_router()
    })
    .await
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
async fn resource_config_applies_hide_transform_and_computed() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/1").await;
    assert_eq!(status, StatusCode::OK);
    // hide:
    assert!(body.get("password_hash").is_none(), "got: {body}");
    // transform:
    assert_eq!(body["email"], json!("***@example.com"));
    // computed:
    assert_eq!(body["display_name"], json!("Alice Doe"));
    // unchanged real fields:
    assert_eq!(body["username"], json!("alice"));
}

#[tokio::test]
async fn resource_config_list_envelope_carries_overrides() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/").await;
    assert_eq!(status, StatusCode::OK);
    let alice = &body["results"][0];
    assert!(alice.get("password_hash").is_none());
    assert_eq!(alice["email"], json!("***@example.com"));
    assert_eq!(alice["display_name"], json!("Alice Doe"));
}

// =====================================================================
// Unit-level — multiple resources stack additively.
// =====================================================================

#[test]
fn resource_config_round_trips_through_resource_method() {
    let cfg = ResourceConfig::new("post")
        .hide("draft_notes")
        .transform("title", |v| {
            json!(format!("PINNED: {}", v.as_str().unwrap_or("")))
        });
    let plugin = RestPlugin::default().resource(cfg);
    let dbg = format!("{plugin:?}");
    // The Debug impl summarises counts; if these grew the config
    // landed in the plugin's vecs as expected.
    assert!(dbg.contains("transforms_count: 1"));
    // hidden is a Vec<(String, String)> on the plugin, so it serialises
    // its full contents in Debug.
    assert!(dbg.contains("draft_notes"));
}

#[test]
fn multiple_resource_configs_stack_additively() {
    let users = ResourceConfig::new("user").hide("password_hash");
    let posts = ResourceConfig::new("post").hide("draft_notes");
    let plugin = RestPlugin::default().resource(users).resource(posts);
    let dbg = format!("{plugin:?}");
    assert!(dbg.contains("password_hash"));
    assert!(dbg.contains("draft_notes"));
}

#[test]
fn resources_batch_registers_every_config() {
    // `.resources([...])` must register every config — identical to calling
    // `.resource(...)` once per item, the per-plugin "export a Vec" pattern.
    let configs = vec![
        ResourceConfig::new("user").hide("password_hash"),
        ResourceConfig::new("post").hide("draft_notes"),
    ];
    let plugin = RestPlugin::default().resources(configs);
    let dbg = format!("{plugin:?}");
    assert!(
        dbg.contains("password_hash"),
        "first config registered: {dbg}"
    );
    assert!(
        dbg.contains("draft_notes"),
        "second config registered: {dbg}"
    );
}
