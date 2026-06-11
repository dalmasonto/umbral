//! End-to-end tests for the field-override surface — `hide`,
//! `transform`, `computed`.
//!
//! Lives in its own test binary (separate process from
//! `integration.rs`) so the App can be booted with a custom RestPlugin
//! configuration — the framework's settings OnceLock only lets one
//! App boot per binary, and integration.rs already does that with the
//! default config.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbra_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct User {
    id: i64,
    username: String,
    email: String,
    password_hash: String,
    first_name: String,
    last_name: String,
}

// A second model exercising the multi-field / model-reference hide
// surface (Feature A). `hide_model::<Account>(["secret_a", ...])`
// drops several columns at once, keyed off `Account::TABLE`.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "account")]
struct Account {
    id: i64,
    label: String,
    secret_a: String,
    secret_b: String,
    secret_c: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("rest_overrides.sqlite");
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

        // The custom-field surface under test. Every response for
        // `user`:
        // 1. drops `password_hash` entirely;
        // 2. masks `email` so only the domain leaks;
        // 3. synthesises a `display_name` from first + last.
        let rest = RestPlugin::default()
            // single-str form — proves &str: HideFields keeps the old
            // call shape compiling unchanged (non-breaking).
            .hide("user", "password_hash")
            // slice form + model-reference form (Feature A): drop three
            // columns from `account` keyed off Account::TABLE, and one
            // more via the slice overload on the table-name builder.
            .hide("account", ["secret_a"])
            .hide_model::<Account>(["secret_b", "secret_c"])
            .transform("user", "email", |v| {
                let s = v.as_str().unwrap_or("");
                match s.split_once('@') {
                    Some((_, d)) => json!(format!("***@{d}")),
                    None => v.clone(),
                }
            })
            .computed("user", "display_name", |row| {
                let f = row.get("first_name").and_then(|v| v.as_str()).unwrap_or("");
                let l = row.get("last_name").and_then(|v| v.as_str()).unwrap_or("");
                json!(format!("{f} {l}").trim().to_string())
            });

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<User>()
            .model::<Account>()
            .plugin(rest)
            .build()
            .expect("App::build with overrides");

        let pool = umbra::db::pool();
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

        // Seed two users so list + retrieve both have data.
        sqlx::query(
            "INSERT INTO user (username, email, password_hash, first_name, last_name) \
             VALUES \
             ('alice', 'alice@example.com', 'argon2:hidden-1', 'Alice', 'Doe'), \
             ('bob', 'bob@other.org', 'argon2:hidden-2', 'Bob', 'Smith')",
        )
        .execute(&pool)
        .await
        .expect("seed");

        sqlx::query(
            "CREATE TABLE account (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                label TEXT NOT NULL,\
                secret_a TEXT NOT NULL,\
                secret_b TEXT NOT NULL,\
                secret_c TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create account table");
        sqlx::query(
            "INSERT INTO account (label, secret_a, secret_b, secret_c) \
             VALUES ('acct', 'AAA', 'BBB', 'CCC')",
        )
        .execute(&pool)
        .await
        .expect("seed account");

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

async fn json_request(
    router: axum::Router,
    method: &str,
    uri: &str,
    body: &str,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).expect("valid json");
    (status, parsed)
}

// =====================================================================
// hide — `password_hash` never appears in any response.
// =====================================================================

#[tokio::test]
async fn list_response_omits_hidden_fields() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/").await;
    assert_eq!(status, StatusCode::OK);
    let results = body["results"].as_array().expect("results array");
    assert!(!results.is_empty());
    for row in results {
        assert!(
            row.get("password_hash").is_none(),
            "password_hash leaked into list response: {row}"
        );
        assert!(row.get("username").is_some(), "username should remain");
    }
}

#[tokio::test]
async fn retrieve_response_omits_hidden_fields() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("password_hash").is_none(),
        "password_hash leaked into retrieve response: {body}"
    );
}

#[tokio::test]
async fn create_response_omits_hidden_fields() {
    let router = boot().await.clone();
    let payload = json!({
        "username": "carol",
        "email": "carol@example.com",
        "password_hash": "argon2:fresh",
        "first_name": "Carol",
        "last_name": "Lee"
    })
    .to_string();
    let (status, body) = json_request(router, "POST", "/api/user/", &payload).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        body.get("password_hash").is_none(),
        "password_hash leaked into create response: {body}"
    );
    // The column itself was still written — the hide is an outbound-
    // shape transformation, not a column-level access restriction.
    // (Verifying that would mean reading the column from the DB; the
    // surrounding `is_some` on `id` is enough to confirm the row
    // landed.)
    assert!(body.get("id").is_some(), "create should return new id");
}

// =====================================================================
// transform — `email` rendered as `***@domain`.
// =====================================================================

#[tokio::test]
async fn list_response_masks_transformed_field() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/").await;
    assert_eq!(status, StatusCode::OK);
    let alice = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["username"] == "alice")
        .expect("alice in list");
    assert_eq!(alice["email"], json!("***@example.com"));
    let bob = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["username"] == "bob")
        .expect("bob in list");
    assert_eq!(bob["email"], json!("***@other.org"));
}

#[tokio::test]
async fn retrieve_response_masks_transformed_field() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["email"], json!("***@example.com"));
}

// =====================================================================
// computed — `display_name` synthesised from first+last.
// =====================================================================

#[tokio::test]
async fn list_response_includes_computed_field() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/").await;
    assert_eq!(status, StatusCode::OK);
    let alice = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["username"] == "alice")
        .expect("alice");
    assert_eq!(alice["display_name"], json!("Alice Doe"));
    let bob = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["username"] == "bob")
        .expect("bob");
    assert_eq!(bob["display_name"], json!("Bob Smith"));
}

#[tokio::test]
async fn retrieve_response_includes_computed_field() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["display_name"], json!("Alice Doe"));
}

// =====================================================================
// All three combine on the same row.
// =====================================================================

#[tokio::test]
async fn one_row_carries_all_three_override_types() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/1").await;
    assert_eq!(status, StatusCode::OK);
    // hidden:
    assert!(body.get("password_hash").is_none());
    // transformed:
    assert_eq!(body["email"], json!("***@example.com"));
    // computed:
    assert_eq!(body["display_name"], json!("Alice Doe"));
    // untouched real fields:
    assert_eq!(body["username"], json!("alice"));
    assert_eq!(body["first_name"], json!("Alice"));
}

// =====================================================================
// Sparse fieldset — `?fields=...` retains only listed keys (gap #81).
// =====================================================================

#[tokio::test]
async fn list_response_respects_fields_query_param() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/?fields=id,username").await;
    assert_eq!(status, StatusCode::OK);
    for row in body["results"].as_array().expect("results array") {
        let obj = row.as_object().expect("row is object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        // Only id + username should appear; computed/transform fields drop out too.
        assert_eq!(
            keys.len(),
            2,
            "row should have exactly 2 keys, got {keys:?}"
        );
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("username"));
        assert!(!obj.contains_key("email"));
        assert!(!obj.contains_key("display_name"));
    }
}

#[tokio::test]
async fn retrieve_response_respects_fields_query_param() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/1?fields=username,first_name").await;
    assert_eq!(status, StatusCode::OK);
    let obj = body.as_object().expect("body is object");
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys.len(),
        2,
        "row should have exactly 2 keys, got {keys:?}"
    );
    assert!(obj.contains_key("username"));
    assert!(obj.contains_key("first_name"));
    assert!(!obj.contains_key("id"));
    assert!(!obj.contains_key("display_name"));
}

#[tokio::test]
async fn fields_query_param_silently_drops_unknown_names() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/1?fields=username,no_such_column").await;
    assert_eq!(status, StatusCode::OK);
    let obj = body.as_object().expect("body is object");
    // Only the real key lands; unknown name is ignored, no 400.
    assert_eq!(obj.len(), 1);
    assert_eq!(obj.get("username"), Some(&json!("alice")));
}

#[tokio::test]
async fn empty_fields_param_falls_back_to_full_row() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/1?fields=").await;
    assert_eq!(status, StatusCode::OK);
    let obj = body.as_object().expect("body is object");
    // Empty `?fields=` is treated as "no filter" — full row comes back
    // (minus hide overrides that always apply).
    assert!(obj.contains_key("username"));
    assert!(obj.contains_key("display_name"));
}

// =====================================================================
// Feature A — multi-field hide + model-reference hide + for_::<M>().
// =====================================================================

#[tokio::test]
async fn slice_hide_drops_every_listed_field() {
    // `.hide("account", ["secret_a"])` (slice form) drops secret_a;
    // `.hide_model::<Account>(["secret_b","secret_c"])` drops the
    // other two. All three must be absent, `label` must remain.
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/account/1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let obj = body.as_object().expect("object");
    assert!(obj.get("secret_a").is_none(), "secret_a leaked: {obj:?}");
    assert!(obj.get("secret_b").is_none(), "secret_b leaked: {obj:?}");
    assert!(obj.get("secret_c").is_none(), "secret_c leaked: {obj:?}");
    assert_eq!(obj.get("label"), Some(&json!("acct")), "label kept");
}

#[tokio::test]
async fn single_str_hide_still_works_non_breaking() {
    // The original `.hide("user", "password_hash")` single-&str call
    // shape compiles unchanged and still strips the field — proves
    // `&str: HideFields` is non-breaking.
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/user/1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("password_hash").is_none(),
        "single-str hide regressed: {body}"
    );
}

#[test]
fn resource_config_for_uses_model_table() {
    // `ResourceConfig::for_::<Account>()` builds a config whose table
    // equals `Account::TABLE` — the model-keyed constructor.
    use umbra::orm::Model;
    let cfg = umbra_rest::ResourceConfig::for_::<Account>();
    assert_eq!(cfg.table(), Account::TABLE);
    assert_eq!(cfg.table(), "account");
    // And its multi-field `.hide` accepts a slice without churning the
    // call site.
    let cfg = cfg.hide(["secret_a", "secret_b"]);
    let _ = cfg; // builder return — compile-proof of the slice overload.
}
