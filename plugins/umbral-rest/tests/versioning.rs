//! URL-path API versioning (gaps2 #82): with
//! `versioning(VersioningScheme::url_path()).allowed_versions(["v1","v2"])`,
//! the resource route-set mounts under `/api/v1/...` AND `/api/v2/...`.
//!
//! - `GET /api/v1/<table>/` and `GET /api/v2/<table>/` both resolve.
//! - `GET /api/v3/<table>/` matches no route → 404 (unknown version isn't
//!   routable, with version in the URL path).
//! - The resolved version reaches handler-visible context: a custom
//!   action echoes `ctx.version`, proving "v1"/"v2" thread through.
//! - Safe-by-default still holds under a versioned path: `password_hash`
//!   is stripped and a default-blocked table 404s.

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

use umbral_rest::{
    ActionScope, AllowAny, ResourceConfig, RestPlugin, VersioningConfig, VersioningScheme,
};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    title: String,
}

/// Stands in for AuthUser: a `password_hash` column the hard denylist
/// must strip even under a versioned path.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct AuthUserStub {
    id: i64,
    username: String,
    password_hash: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("versioning_urlpath.sqlite");
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

        // A `whoami` action echoes the resolved version straight from the
        // request context, so a test can assert it round-tripped.
        let post_cfg = ResourceConfig::new("post").action(
            "whoami",
            Method::GET,
            ActionScope::Collection,
            |ctx| async move { Ok(json!({ "version": ctx.version })) },
        );

        let rest = RestPlugin::default()
            .default_permission(AllowAny)
            // Opt the auth_user_stub table out of the block-list, WITHOUT a
            // .hide("password_hash") — the hard denylist must still strip it.
            .expose(["auth_user_stub"])
            .resource(post_cfg)
            .versioning(
                VersioningConfig::new(VersioningScheme::url_path())
                    .allowed_versions(["v1", "v2"])
                    .default_version("v1"),
            );

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .model::<AuthUserStub>()
            .plugin(rest)
            .build()
            .expect("App::build with URL-path versioning");

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
        sqlx::query(
            "CREATE TABLE auth_user_stub (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL,\
                password_hash TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("create auth_user_stub");
        sqlx::query(
            "INSERT INTO auth_user_stub (username, password_hash) \
             VALUES ('alice', '$argon2id$SECRET')",
        )
        .execute(&pool)
        .await
        .expect("seed auth_user_stub");

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
async fn v1_and_v2_both_resolve() {
    let (s1, b1) = get_json("/api/v1/post/").await;
    assert_eq!(s1, StatusCode::OK, "v1 list must resolve: {b1}");
    assert_eq!(b1["results"][0]["title"], json!("hello"), "{b1}");

    let (s2, b2) = get_json("/api/v2/post/").await;
    assert_eq!(s2, StatusCode::OK, "v2 list must resolve: {b2}");
    assert_eq!(b2["results"][0]["title"], json!("hello"), "{b2}");
}

#[tokio::test]
async fn unknown_version_is_404() {
    let (status, _body) = get_json("/api/v3/post/").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unallowed version matches no route → 404"
    );
}

#[tokio::test]
async fn unversioned_path_is_404_when_url_path_versioning_is_on() {
    // With URL-path versioning the version is required in the
    // path; there is no unversioned `/api/<table>/` fallback.
    let (status, _body) = get_json("/api/post/").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "URL-path versioning requires the version segment — bare /api/post/ 404s"
    );
}

#[tokio::test]
async fn resolved_version_reaches_the_request_context() {
    let (s1, b1) = get_json("/api/v1/post/whoami/").await;
    assert_eq!(s1, StatusCode::OK, "v1 action must resolve: {b1}");
    assert_eq!(
        b1["version"],
        json!("v1"),
        "ctx.version must be the URL-path version: {b1}"
    );

    let (s2, b2) = get_json("/api/v2/post/whoami/").await;
    assert_eq!(s2, StatusCode::OK, "v2 action must resolve: {b2}");
    assert_eq!(b2["version"], json!("v2"), "{b2}");
}

#[tokio::test]
async fn safe_by_default_holds_under_a_versioned_path() {
    // password_hash is stripped even though auth_user_stub was .expose()d
    // without a .hide(), under the versioned path.
    let (status, body) = get_json("/api/v1/auth_user_stub/1").await;
    assert_eq!(status, StatusCode::OK, "exposed table retrieve: {body}");
    assert!(
        body.get("password_hash").is_none(),
        "password_hash must stay stripped under a versioned path: {body}"
    );
    assert_eq!(body["username"], json!("alice"), "{body}");

    // A table NOT exposed and on the default block-list (session) 404s.
    let (blocked, _b) = get_json("/api/v1/session/").await;
    assert_eq!(
        blocked,
        StatusCode::NOT_FOUND,
        "a default-blocked table is still hidden under a versioned path"
    );
}
