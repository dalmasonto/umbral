//! Security regression tests: `hide` / `transform` / `computed` must
//! apply to `?include=`'d NESTED relation objects, not just the
//! top-level row.
//!
//! The bug this guards against: a resource that includes a relation
//! (`?include=author`) hydrates the related row into a nested object
//! under that key. A `hide("child_table", "secret")` registered for
//! the related table was NEVER applied to that nested object — so a
//! sensitive column (mirroring `auth_user.password_hash`) leaked
//! straight through the nested relation. The real repro the user hit:
//! `GET /api/plugin/?include=created_by` returned
//! `created_by: { ..., "password_hash": "$argon2id$..." }` despite a
//! `hide("auth_user", "password_hash")`.
//!
//! These drive the real list/retrieve handler path, so the JSON we
//! read back is exactly what a client sees (`fetch_rows` →
//! `apply_overrides` → serialize). Separate test binary because the
//! framework's settings OnceLock allows only one App boot per process.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral::orm::ForeignKey;
use umbral_rest::RestPlugin;

// Grandchild — holds the field we hide at the deepest level
// (`no_zone`) plus a survivor field (`city`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "no_zone")]
struct Zone {
    id: i64,
    #[umbral(string)]
    city: String,
    secret_zone: String,
}

// Child — mirrors `auth_user`: carries the security-critical `secret`
// column (the stand-in for `password_hash`), a `email` we transform,
// and an FK to the grandchild for the multi-hop case.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "no_child")]
struct Child {
    id: i64,
    #[umbral(string)]
    username: String,
    email: String,
    secret: String,
    zone: ForeignKey<Zone>,
}

// Parent — mirrors `plugin`: has an FK (`created_by`) to the child,
// the relation a client expands via `?include=created_by`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "no_parent")]
struct Parent {
    id: i64,
    #[umbral(string)]
    label: String,
    created_by: ForeignKey<Child>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("rest_nested_overrides.sqlite");
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

        // Hide the security-critical `secret` on the child table, the
        // grandchild's `secret_zone`, transform the child's `email`,
        // and add a computed field on the child. NONE of these are
        // registered against the parent — they only fire on the child
        // / grandchild, including when those tables appear NESTED.
        let rest = RestPlugin::default()
            .hide("no_child", ["secret"])
            .hide("no_zone", ["secret_zone"])
            .transform("no_child", "email", |v| {
                let s = v.as_str().unwrap_or("");
                match s.split_once('@') {
                    Some((_, d)) => json!(format!("***@{d}")),
                    None => v.clone(),
                }
            })
            .computed("no_child", "display_name", |row| {
                let u = row.get("username").and_then(|v| v.as_str()).unwrap_or("");
                json!(format!("user:{u}"))
            });

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Zone>()
            .model::<Child>()
            .model::<Parent>()
            .plugin(rest)
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        for sql in &[
            "CREATE TABLE no_zone (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                city TEXT NOT NULL,\
                secret_zone TEXT NOT NULL\
             )",
            "CREATE TABLE no_child (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL,\
                email TEXT NOT NULL,\
                secret TEXT NOT NULL,\
                zone INTEGER NOT NULL REFERENCES no_zone(id)\
             )",
            "CREATE TABLE no_parent (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                label TEXT NOT NULL,\
                created_by INTEGER NOT NULL REFERENCES no_child(id)\
             )",
        ] {
            sqlx::query(sql).execute(&pool).await.expect("create table");
        }

        sqlx::query("INSERT INTO no_zone (city, secret_zone) VALUES ('Nairobi', 'ZZZ')")
            .execute(&pool)
            .await
            .expect("seed zone");
        sqlx::query(
            "INSERT INTO no_child (username, email, secret, zone) \
             VALUES ('alice', 'alice@example.com', '$argon2id$leaked', 1)",
        )
        .execute(&pool)
        .await
        .expect("seed child");
        sqlx::query("INSERT INTO no_parent (label, created_by) VALUES ('the-plugin', 1)")
            .execute(&pool)
            .await
            .expect("seed parent");

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

// =====================================================================
// 1. THE LEAK REPRO — the hidden `secret` must NOT appear in the nested
//    `created_by` object, but a non-hidden field must survive (proving
//    we strip the field, not nuke the whole nested object).
//    FAILS before the fix (secret present); PASSES after.
// =====================================================================

#[tokio::test]
async fn nested_included_relation_strips_hidden_field() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/no_parent/?include=created_by").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let first = body["results"]
        .as_array()
        .expect("results array")
        .first()
        .expect("at least one parent");
    let nested = first
        .get("created_by")
        .and_then(|v| v.as_object())
        .expect("created_by hydrated into a nested object");

    // THE SECURITY ASSERTION: the hidden column must be gone.
    assert!(
        nested.get("secret").is_none(),
        "DATA LEAK: hidden `secret` leaked through the nested created_by relation: {nested:?}"
    );
    // ...but we didn't nuke the whole object: a non-hidden field stays.
    assert_eq!(
        nested.get("username"),
        Some(&json!("alice")),
        "non-hidden field should survive in the nested object"
    );
}

// Retrieve path (single object) leaks the same way without the fix.
#[tokio::test]
async fn nested_included_relation_strips_hidden_field_on_retrieve() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/no_parent/1?include=created_by").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let nested = body
        .get("created_by")
        .and_then(|v| v.as_object())
        .expect("created_by nested object");
    assert!(
        nested.get("secret").is_none(),
        "DATA LEAK on retrieve path: {nested:?}"
    );
    assert_eq!(nested.get("username"), Some(&json!("alice")));
}

// =====================================================================
// 2. TOP-LEVEL UNAFFECTED — `GET /api/no_child/` still hides `secret`
//    at the root (existing behavior intact).
// =====================================================================

#[tokio::test]
async fn top_level_hide_still_applies() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/no_child/1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.get("secret").is_none(),
        "top-level hide regressed: {body}"
    );
    assert_eq!(body.get("username"), Some(&json!("alice")));
}

// =====================================================================
// 3. MULTI-HOP — parent → child → zone (grandchild). The hidden
//    `secret_zone` must be stripped from the doubly-nested object.
// =====================================================================

#[tokio::test]
async fn multi_hop_nested_relation_strips_hidden_field() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/no_parent/1?include=created_by.zone").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let child = body
        .get("created_by")
        .and_then(|v| v.as_object())
        .expect("created_by nested object");
    // child's own hidden field still stripped at this depth.
    assert!(
        child.get("secret").is_none(),
        "child secret leaked: {child:?}"
    );
    let zone = child
        .get("zone")
        .and_then(|v| v.as_object())
        .expect("zone hydrated (include=created_by.zone)");
    assert!(
        zone.get("secret_zone").is_none(),
        "DATA LEAK: grandchild `secret_zone` leaked through doubly-nested object: {zone:?}"
    );
    assert_eq!(
        zone.get("city"),
        Some(&json!("Nairobi")),
        "non-hidden grandchild field should survive"
    );
}

// =====================================================================
// 4. TRANSFORM + COMPUTED RECURSE — a transform on the child's `email`
//    and a computed `display_name` must also fire on the NESTED child
//    object, mirroring `hide`.
// =====================================================================

#[tokio::test]
async fn transform_and_computed_recurse_into_nested_relation() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/no_parent/1?include=created_by").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let nested = body
        .get("created_by")
        .and_then(|v| v.as_object())
        .expect("created_by nested object");
    // transform applied to the nested object's email:
    assert_eq!(
        nested.get("email"),
        Some(&json!("***@example.com")),
        "transform did not recurse into nested relation: {nested:?}"
    );
    // computed field synthesised on the nested object:
    assert_eq!(
        nested.get("display_name"),
        Some(&json!("user:alice")),
        "computed did not recurse into nested relation: {nested:?}"
    );
}
