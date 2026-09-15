//! TaskFlow #436 — validation errors on a WRITABLE NESTED write must be
//! COLLECTED tree-wide, DRILLED DOWN / path-keyed to the offending item
//! (`posts[0].slug`, `posts[0].comments[1].body`), surfaced in the SAME flat
//! body shape as a top-level write, and TRANSACTIONAL (a failure creates zero
//! rows anywhere in the tree).
//!
//! Before the fix the nested walk inserted the parent first and `?`-bailed on
//! the first bad node, so a bad top-level field masked every nested error and
//! sibling children were never reported; a nested error that did surface was
//! keyed by the child's BARE column name (`slug`), so a client couldn't tell
//! WHICH item failed.
//!
//! One shared `App` (settings init is one-shot per process); each test uses
//! globally-unique `contact`/`slug` values and scopes its DB assertions to
//! those, so the tests are independent of one another under parallel runs and
//! the global UNIQUE on `nested_post.slug`.

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

use umbral::orm::ForeignKey;
use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Blog {
    id: i64,
    #[umbral(email)]
    contact: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nested_post")]
struct NestedPost {
    id: i64,
    #[umbral(on_delete = "cascade")]
    blog: ForeignKey<Blog>,
    #[umbral(unique)]
    slug: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nested_comment")]
struct NestedComment {
    id: i64,
    #[umbral(on_delete = "cascade")]
    post: ForeignKey<NestedPost>,
    body: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nested_validation.sqlite");
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
            .resource(ResourceConfig::for_::<Blog>().nested("posts", "nested_post"))
            .resource(ResourceConfig::for_::<NestedPost>().nested("comments", "nested_comment"));

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Blog>()
            .model::<NestedPost>()
            .model::<NestedComment>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // SQLite enforces FKs per-connection.
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&umbral::db::pool())
            .await
            .expect("enable fks");

        app.into_router()
    })
    .await
    .clone()
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
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn blogs_with_contact(contact: &str) -> i64 {
    Blog::objects()
        .filter(blog::CONTACT.eq(contact))
        .count()
        .await
        .unwrap()
}

async fn posts_with_slug(slug: &str) -> i64 {
    NestedPost::objects()
        .filter(nested_post::SLUG.eq(slug))
        .count()
        .await
        .unwrap()
}

/// (c) TRANSACTIONAL + (b) DRILL-DOWN on a DB-level UNIQUE clash: a nested child
/// whose unique `slug` collides with an existing row → 400, error path-keyed to
/// `posts[0].slug`, and ZERO rows created for BOTH parent and child.
#[tokio::test]
async fn nested_unique_slug_collision_is_path_keyed_and_creates_nothing() {
    let router = boot().await;

    // Seed a first blog+post so a later post can collide on this unique slug.
    let (status, _b) = post(
        &router,
        "/api/blog/",
        json!({ "contact": "u-seed@example.com", "posts": [ { "slug": "u-collide" } ] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "seed nested create must succeed"
    );
    assert_eq!(posts_with_slug("u-collide").await, 1, "seed post exists");

    // A NEW blog whose nested post reuses the unique slug.
    let (status, body) = post(
        &router,
        "/api/blog/",
        json!({ "contact": "u-new@example.com", "posts": [ { "slug": "u-collide" } ] }),
    )
    .await;
    eprintln!("UNIQUE-COLLISION body = {body}");
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    // Drilled down: keyed to the nested item, not a bare `slug`.
    assert!(
        body["posts[0].slug"].is_array(),
        "unique-slug error must be path-keyed to `posts[0].slug`; got {body}",
    );
    // Transactional: the failed second write created NOTHING new.
    assert_eq!(
        blogs_with_contact("u-new@example.com").await,
        0,
        "no new Blog row may be created when a nested child failed; got {body}",
    );
    assert_eq!(
        posts_with_slug("u-collide").await,
        1,
        "still just the seed post — the colliding insert created nothing; got {body}",
    );
}

/// (a) CROSS-NODE COLLECTION: a bad TOP-LEVEL field AND a bad NESTED field in one
/// request surface TOGETHER — `contact` (parent) and `posts[0].slug` (child) —
/// proving Phase 0 no longer masks the children.
#[tokio::test]
async fn top_level_and_nested_errors_surface_together() {
    let router = boot().await;
    let (status, body) = post(
        &router,
        "/api/blog/",
        json!({
            "contact": "tn-not-an-email",   // top-level format error
            "posts": [ { } ]                 // nested child missing required slug
        }),
    )
    .await;
    eprintln!("TOP+NESTED body = {body}");
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(body["code"], "validation_error", "got {body}");
    assert!(
        body["contact"].is_array(),
        "top-level `contact` error must be present; got {body}",
    );
    assert!(
        body["posts[0].slug"].is_array(),
        "nested `posts[0].slug` error must ALSO be present and drilled down; got {body}",
    );
    assert_eq!(
        blogs_with_contact("tn-not-an-email").await,
        0,
        "nothing created; got {body}",
    );
}

/// (a) EVERY invalid sibling surfaces, each keyed by index — not just the first.
#[tokio::test]
async fn every_invalid_nested_sibling_surfaces() {
    let router = boot().await;
    let (status, body) = post(
        &router,
        "/api/blog/",
        json!({
            "contact": "sib@example.com",   // valid top-level
            "posts": [ { }, { } ]           // BOTH children missing slug
        }),
    )
    .await;
    eprintln!("TWO-BAD-SIBLINGS body = {body}");
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        body["posts[0].slug"].is_array(),
        "first child error keyed by index; got {body}",
    );
    assert!(
        body["posts[1].slug"].is_array(),
        "SECOND child error must ALSO be present, keyed by index; got {body}",
    );
    assert_eq!(blogs_with_contact("sib@example.com").await, 0, "got {body}");
}

/// (b) DRILL-DOWN to the last item across THREE levels: a bad grandchild is keyed
/// `posts[0].comments[1].body`.
#[tokio::test]
async fn deep_nested_error_drills_down_to_the_grandchild() {
    let router = boot().await;
    let (status, body) = post(
        &router,
        "/api/blog/",
        json!({
            "contact": "deep@example.com",
            "posts": [
                {
                    "slug": "d-post-a",
                    "comments": [ { "body": "ok" }, { } ]   // comments[1] missing body
                }
            ]
        }),
    )
    .await;
    eprintln!("DEEP-NEST body = {body}");
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert!(
        body["posts[0].comments[1].body"].is_array(),
        "grandchild error must drill down to `posts[0].comments[1].body`; got {body}",
    );
    assert_eq!(
        blogs_with_contact("deep@example.com").await,
        0,
        "got {body}"
    );
    assert_eq!(posts_with_slug("d-post-a").await, 0, "got {body}");
}

/// The happy path still works end-to-end after the two-pass restructure.
#[tokio::test]
async fn valid_nested_create_still_succeeds() {
    let router = boot().await;
    let (status, body) = post(
        &router,
        "/api/blog/",
        json!({
            "contact": "v@example.com",
            "posts": [
                { "slug": "v-p1", "comments": [ { "body": "hi" } ] },
                { "slug": "v-p2" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "got {body}");
    assert_eq!(blogs_with_contact("v@example.com").await, 1);
    assert_eq!(posts_with_slug("v-p1").await, 1);
    assert_eq!(posts_with_slug("v-p2").await, 1);
    assert_eq!(
        NestedComment::objects()
            .filter(nested_comment::BODY.eq("hi"))
            .count()
            .await
            .unwrap(),
        1,
    );
}
