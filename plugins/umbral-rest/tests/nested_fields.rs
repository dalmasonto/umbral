//! End-to-end tests for nested `?fields=` projection into
//! `?include=`'d relations (Feature B1).
//!
//! Drives the real list/retrieve handler path so the response JSON we
//! read back is exactly what a client would see. The handler runs
//! `?include=` hydration first (turning an FK integer into a nested
//! object), then `apply_sparse_fields` prunes that nested JSON down to
//! the requested paths.
//!
//! Separate test binary from `field_overrides.rs` / `integration.rs`
//! because the framework's settings OnceLock allows only one App boot
//! per process.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral::orm::ForeignKey;
use umbral_rest::RestPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nf_profile")]
struct Profile {
    id: i64,
    #[umbral(string)]
    github_url: String,
    #[umbral(string)]
    bio: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nf_author")]
struct Author {
    id: i64,
    #[umbral(string)]
    name: String,
    #[umbral(string)]
    nickname: String,
    profile: ForeignKey<Profile>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nf_post")]
struct Post {
    id: i64,
    #[umbral(string)]
    title: String,
    author: ForeignKey<Author>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("rest_nested.sqlite");
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
            .model::<Profile>()
            .model::<Author>()
            .model::<Post>()
            .plugin(RestPlugin::default())
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        for sql in &[
            "CREATE TABLE nf_profile (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                github_url TEXT NOT NULL,\
                bio TEXT NOT NULL\
             )",
            "CREATE TABLE nf_author (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL,\
                nickname TEXT NOT NULL,\
                profile INTEGER NOT NULL REFERENCES nf_profile(id)\
             )",
            "CREATE TABLE nf_post (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                author INTEGER NOT NULL REFERENCES nf_author(id)\
             )",
        ] {
            sqlx::query(sql).execute(&pool).await.expect("create table");
        }

        sqlx::query("INSERT INTO nf_profile (github_url, bio) VALUES ('gh/alice', 'hello world')")
            .execute(&pool)
            .await
            .expect("seed profile");
        sqlx::query("INSERT INTO nf_author (name, nickname, profile) VALUES ('Alice', 'ali', 1)")
            .execute(&pool)
            .await
            .expect("seed author");
        sqlx::query("INSERT INTO nf_post (title, author) VALUES ('First Post', 1)")
            .execute(&pool)
            .await
            .expect("seed post");

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
// Nested projection — `?include=author&fields=author__name` prunes the
// nested object down to just the requested child.
// =====================================================================

#[tokio::test]
async fn nested_fields_prune_included_relation_underscore_form() {
    let router = boot().await.clone();
    let (status, body) =
        get_json(router, "/api/nf_post/1?include=author&fields=author__name").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    // The post object: only `author` survived the top-level prune
    // (title, id dropped because not requested).
    let obj = body.as_object().expect("body is object");
    assert_eq!(
        obj.keys().cloned().collect::<Vec<_>>(),
        vec!["author".to_string()],
        "only author kept at top level, got {obj:?}"
    );
    let author = obj
        .get("author")
        .unwrap()
        .as_object()
        .expect("author is object");
    assert_eq!(
        author.keys().cloned().collect::<Vec<_>>(),
        vec!["name".to_string()],
        "author pruned to only `name`, got {author:?}"
    );
    assert_eq!(author.get("name"), Some(&Value::String("Alice".into())));
}

#[tokio::test]
async fn dot_form_behaves_identically_to_underscore() {
    let router = boot().await.clone();
    let (s1, dot) = get_json(
        boot().await.clone(),
        "/api/nf_post/1?include=author&fields=author.name",
    )
    .await;
    let (s2, underscore) =
        get_json(router, "/api/nf_post/1?include=author&fields=author__name").await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(dot, underscore, "dot and __ separators are equivalent");
}

#[tokio::test]
async fn nested_fields_keep_requested_top_levels_alongside_relation() {
    let router = boot().await.clone();
    let (status, body) = get_json(
        router,
        "/api/nf_post/1?include=author&fields=title,author__nickname",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let obj = body.as_object().expect("body is object");
    let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
    keys.sort();
    assert_eq!(keys, vec!["author", "title"]);
    assert_eq!(obj.get("title"), Some(&Value::String("First Post".into())));
    let author = obj.get("author").unwrap().as_object().unwrap();
    assert_eq!(
        author.keys().cloned().collect::<Vec<_>>(),
        vec!["nickname".to_string()]
    );
}

// =====================================================================
// Multi-hop — `?include=author.profile&fields=author__profile__bio`
// prunes two levels deep.
// =====================================================================

#[tokio::test]
async fn multi_hop_nested_projection() {
    let router = boot().await.clone();
    let (status, body) = get_json(
        router,
        "/api/nf_post/1?include=author.profile&fields=author__profile__bio",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let obj = body.as_object().expect("object");
    assert_eq!(
        obj.keys().cloned().collect::<Vec<_>>(),
        vec!["author".to_string()]
    );
    let author = obj
        .get("author")
        .unwrap()
        .as_object()
        .expect("author object");
    assert_eq!(
        author.keys().cloned().collect::<Vec<_>>(),
        vec!["profile".to_string()],
        "author pruned to only `profile`, got {author:?}"
    );
    let profile = author
        .get("profile")
        .unwrap()
        .as_object()
        .expect("profile object (include=author.profile hydrated it)");
    assert_eq!(
        profile.keys().cloned().collect::<Vec<_>>(),
        vec!["bio".to_string()],
        "profile pruned to only `bio`, got {profile:?}"
    );
    assert_eq!(
        profile.get("bio"),
        Some(&Value::String("hello world".into()))
    );
}

// =====================================================================
// `?fields=rel__field` WITHOUT `?include=rel` leaves the FK integer
// untouched — no crash, the relation is still the raw int.
// =====================================================================

#[tokio::test]
async fn nested_field_without_include_leaves_fk_integer() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/nf_post/1?fields=id,author__name").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let obj = body.as_object().expect("object");
    // `author` is the raw FK integer (no ?include=), so the nested
    // path can't prune into it — the integer survives verbatim.
    assert_eq!(obj.get("author"), Some(&Value::Number(1.into())));
    assert_eq!(obj.get("id"), Some(&Value::Number(1.into())));
}

// =====================================================================
// Backward-compat: pure `?fields=id,title` (no dots) still works.
// =====================================================================

#[tokio::test]
async fn plain_fields_still_work() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/nf_post/1?fields=id,title").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let obj = body.as_object().expect("object");
    let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
    keys.sort();
    assert_eq!(keys, vec!["id", "title"]);
}
