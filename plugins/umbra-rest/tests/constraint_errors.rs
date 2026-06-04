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
        body["code"], "validation_error",
        "machine-readable code; got body: {body}",
    );
    // The pre-DB existence check keys the error under the FK
    // column directly — much more actionable than the
    // engine-level "FOREIGN KEY constraint failed" non-field
    // message used to be.
    let fk_errors = body["author_id"]
        .as_array()
        .expect("`author_id` should carry the per-field FK message; got {body}");
    let msg = fk_errors[0].as_str().unwrap_or("");
    assert!(
        msg.contains("not found"),
        "message should say the referenced row doesn't exist; got {msg:?}",
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
    // Three valid outcomes, in order of preference:
    //  - `validation_error`: pre-validation caught the missing
    //    `username` before it reached the DB (preferred — no SQL
    //    round-trip).
    //  - `null_constraint`: the row reached SQLite and it rejected
    //    the NOT NULL violation.
    //  - `bad_input`: the ORM's protocol-error path turned the
    //    missing column into a structured 400.
    assert!(
        code == "validation_error" || code == "null_constraint" || code == "bad_input",
        "expected validation_error / null_constraint / bad_input, got code={code:?} body={body}",
    );

    // All three shapes name the offending column. `validation_error`
    // and `null_constraint` carry `username` as a field error;
    // `bad_input` puts the explanation in the top-level `error`.
    if code == "validation_error" || code == "null_constraint" {
        let username_errors = body["username"]
            .as_array()
            .expect("`username` should carry the field error; got {body}");
        let msg = username_errors[0].as_str().unwrap_or("");
        assert!(
            msg.contains("required") || msg.contains("blank"),
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

// =========================================================================
// Required-field pre-validation: empty strings and missing values
// on NOT NULL columns with no default are rejected BEFORE the
// DB sees them. (User report: shop demo accepted a row of
// `"name": ""`, `"slug": ""` etc. because the constraint layer
// happily writes empty strings into NOT NULL VARCHAR.)
// =========================================================================

#[tokio::test]
async fn empty_string_on_required_field_is_rejected() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/author/",
        // `username` is NOT NULL — sending `""` shouldn't slip
        // through as an empty row.
        json!({ "username": "", "email": "blank@example.com" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    assert_eq!(body["code"], "validation_error");
    let username_errors = body["username"]
        .as_array()
        .expect("`username` should carry the required-field message; got {body}");
    assert!(
        username_errors[0]
            .as_str()
            .map(|s| s.contains("required") || s.contains("blank"))
            .unwrap_or(false),
        "got {username_errors:?}",
    );
}

#[tokio::test]
async fn multiple_blank_fields_surface_in_one_response() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/author/",
        json!({ "username": "", "email": "" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    // Both fields surface in the same response — the client can
    // highlight every form input in one round-trip.
    assert!(body["username"].is_array(), "got {body}");
    assert!(body["email"].is_array(), "got {body}");
}

#[tokio::test]
async fn missing_required_field_is_rejected_too() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/author/",
        json!({ "email": "noname@example.com" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    assert_eq!(body["code"], "validation_error");
    assert!(body["username"].is_array(), "got {body}");
}

// =========================================================================
// Body-aware error enrichment: UNIQUE / FK errors now include
// the offending VALUE so the client knows exactly which input is
// duplicated / referencing a missing row.
// =========================================================================

#[tokio::test]
async fn unique_violation_message_names_the_offending_value() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/author/",
        json!({ "username": "dave", "email": "alice@example.com" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    assert_eq!(body["code"], "unique_constraint");
    let email_errors = body["email"]
        .as_array()
        .expect("`email` field error; got {body}");
    let msg = email_errors[0].as_str().unwrap_or("");
    assert!(
        msg.contains("'alice@example.com'"),
        "message should name the offending value; got {msg:?}",
    );
}

// The pre-DB existence check shipped here catches the same case
// the SQLite-engine-level FOREIGN KEY message used to surface in
// `non_field_errors`. The replacement test
// `fk_pointing_at_nonexistent_row_is_caught_pre_db` (below)
// asserts the new field-keyed shape; the legacy non-field-errors
// path is now unreachable from a normal API request.

// =========================================================================
// FK = 0 (or negative) is the form-default "nothing selected"
// placeholder — pre-validation should catch it before the row
// touches the DB, so the response keys the error under the FK
// column instead of buried in a non-field message.
// =========================================================================

#[tokio::test]
async fn fk_zero_is_reported_as_not_found_under_the_fk_column() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/comment/",
        // `author_id: 0` is the typical "form didn't pick a
        // value" sentinel. Auto-increment rows start at 1, so
        // 0 can't possibly reference a real author. The error
        // should carry that truth ("row with id=0 not found"),
        // not a synthetic "this field is required."
        json!({ "author_id": 0, "body": "ghost" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    assert_eq!(body["code"], "validation_error");
    let fk_errors = body["author_id"]
        .as_array()
        .expect("`author_id` should carry the FK-not-found message; got {body}");
    let msg = fk_errors[0].as_str().unwrap_or("");
    assert!(
        msg.contains("not found"),
        "message should say the referenced row doesn't exist; got {msg:?}",
    );
    assert!(
        msg.contains("id=0"),
        "message should name the offending value; got {msg:?}",
    );
}

#[tokio::test]
async fn fk_pointing_at_nonexistent_row_is_caught_pre_db() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/comment/",
        json!({ "author_id": 99999, "body": "wrong" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    assert_eq!(body["code"], "validation_error");
    // The pre-DB existence check keys the error under the FK
    // column directly, so the client doesn't have to parse a
    // non_field_errors string to figure out which FK was bad.
    let fk_errors = body["author_id"]
        .as_array()
        .expect("`author_id` should carry the FK-not-found message; got {body}");
    let msg = fk_errors[0].as_str().unwrap_or("");
    assert!(
        msg.contains("id=99999"),
        "message should name the offending value; got {msg:?}",
    );
}

// =========================================================================
// Required-field AND FK-not-found surface in the SAME response.
// The user report: blank `name` + bogus `category` → previously
// only the blank `name` came back because the required check
// short-circuited. Now both checks run and merge before the 400.
// =========================================================================

#[tokio::test]
async fn blank_string_and_bad_fk_surface_together() {
    let router = boot().await.clone();
    let (status, body) = post_json(
        router,
        "/api/comment/",
        // `body` is blank (required-field error) AND `author_id`
        // is 0 (FK-not-found). Both should appear in one
        // response.
        json!({ "author_id": 0, "body": "" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got body: {body}");
    assert_eq!(body["code"], "validation_error");

    let body_errors = body["body"]
        .as_array()
        .expect("`body` should carry the required-field message; got {body}");
    assert!(
        body_errors[0]
            .as_str()
            .map(|s| s.contains("required") || s.contains("blank"))
            .unwrap_or(false),
        "got {body_errors:?}",
    );

    let fk_errors = body["author_id"]
        .as_array()
        .expect("`author_id` should ALSO be present; got {body}");
    let msg = fk_errors[0].as_str().unwrap_or("");
    assert!(
        msg.contains("not found") && msg.contains("id=0"),
        "got {msg:?}",
    );
}
