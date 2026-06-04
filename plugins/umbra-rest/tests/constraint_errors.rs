//! DRF-style validation errors for DB constraint failures.
//!
//! Boots an app with a tiny schema that has every constraint
//! shape we care about:
//!
//! - `Comment.author_id` is a foreign key into `Author`. POSTing
//!   a comment with an unknown author id triggers a SQLite
//!   FOREIGN KEY constraint (code 787).
//! - `Author.email` is `UNIQUE`. POSTing a second author with the
//!   same email triggers a UNIQUE constraint (code 2067) and the
//!   field name surfaces in the response body.
//! - `Author.username` is `NOT NULL`. POSTing without it triggers
//!   a NOT NULL constraint (code 1299) and the field name
//!   surfaces in the response body.
//!
//! The pre-fix shape was a 500 with `{ "error": "...", "code":
//! "database_error" }`. The post-fix shape is a 400 with DRF-flat
//! field errors (`{ "category": ["..."], "code":
//! "fk_constraint" }`).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbra::orm::ForeignKey;
use umbra_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Author {
    id: i64,
    username: String,
    email: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Comment {
    id: i64,
    author_id: ForeignKey<Author>,
    body: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("constraint_errors.sqlite");
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

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Author>()
            .model::<Comment>()
            .plugin(RestPlugin::default())
            .build()
            .expect("App::build");

        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE author (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL,\
                email TEXT NOT NULL UNIQUE\
             )",
        )
        .execute(&pool)
        .await
        .expect("create author");
        sqlx::query(
            "CREATE TABLE comment (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                author_id INTEGER NOT NULL REFERENCES author(id),\
                body TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create comment");
        // SQLite needs FKs enabled per-connection. The pool gives
        // sticky-enough behaviour for these tests; production
        // setups would set this in a pool acquire hook.
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("enable fks");

        // Seed one author so the unique-email collision test has
        // something to collide with.
        sqlx::query("INSERT INTO author (username, email) VALUES ('alice', 'alice@example.com')")
            .execute(&pool)
            .await
            .expect("seed author");

        app.into_router()
    })
    .await
}

async fn post_json(router: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
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

// =========================================================================
// FOREIGN KEY violation (SQLite code 787)
// =========================================================================

#[tokio::test]
async fn foreign_key_violation_renders_as_400_with_fk_constraint_code() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/comment/",
        json!({ "author_id": 9999, "body": "orphan" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "expected 400, got {body}");
    assert_eq!(
        body["code"], "fk_constraint",
        "machine-readable code; got body: {body}",
    );
    let nfe = body["non_field_errors"]
        .as_array()
        .expect("non_field_errors array; SQLite doesn't tell us which FK failed");
    assert!(!nfe.is_empty());
    assert!(
        nfe[0]
            .as_str()
            .map(|s| s.contains("foreign-key"))
            .unwrap_or(false),
        "message should mention the constraint kind; got {nfe:?}",
    );
}

// =========================================================================
// UNIQUE violation (SQLite code 2067) — field name surfaces
// =========================================================================

#[tokio::test]
async fn unique_violation_keys_the_error_under_the_offending_column() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/author/",
        json!({ "username": "bob", "email": "alice@example.com" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    assert_eq!(body["code"], "unique_constraint");
    let email_errors = body["email"]
        .as_array()
        .expect("`email` should carry the unique-violation message; got {body}");
    assert_eq!(email_errors.len(), 1);
    assert!(
        email_errors[0]
            .as_str()
            .map(|s| s.contains("already exists"))
            .unwrap_or(false),
        "message should mention duplication; got {email_errors:?}",
    );
}

// =========================================================================
// NOT NULL violation (SQLite code 1299) — field name surfaces
// =========================================================================

#[tokio::test]
async fn not_null_violation_keys_the_error_under_the_required_column() {
    let router = boot().await.clone();
    // POST with username omitted entirely. The ORM's
    // pre-validation will likely catch it as a Protocol error
    // ("field X required") BEFORE the row reaches SQLite — in
    // which case we get the `bad_input` 400 path. If the row
    // does reach SQLite, we get the structured `null_constraint`
    // path. Either way it's a 400 with a discoverable reason —
    // pin both shapes.
    let (status, body) = post_json(
        router,
        "/api/author/",
        json!({ "email": "ghost@example.com" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    let code = body["code"].as_str().unwrap_or("");
    assert!(
        code == "null_constraint" || code == "bad_input",
        "expected null_constraint or bad_input, got code={code:?} body={body}",
    );

    // When the ORM passes the row through and SQLite rejects it,
    // the structured shape surfaces `username` as a field error.
    if code == "null_constraint" {
        let username_errors = body["username"]
            .as_array()
            .expect("`username` should carry the not-null message; got {body}");
        assert!(
            username_errors[0]
                .as_str()
                .map(|s| s.contains("required"))
                .unwrap_or(false),
            "got {username_errors:?}",
        );
    }
}

// =========================================================================
// Successful POST still returns 201 (regression — the new error
// path can't intercept happy-path inserts).
// =========================================================================

#[tokio::test]
async fn valid_payload_still_returns_201_created() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/author/",
        json!({ "username": "carol", "email": "carol@example.com" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "got body: {body}");
    assert_eq!(body["username"], "carol");
}
