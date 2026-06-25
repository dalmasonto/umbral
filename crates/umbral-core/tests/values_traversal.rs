//! `.values("author__name")`-style traversal — one-hop FK
//! traversal in `.values()` that returns nested JSON objects per
//! relation. Companion to `.only()` for the case where the caller
//! wants column-trimmed related rows AND doesn't need typed
//! results.
//!
//! Tests cover: nested JSON shape, LEFT JOIN miss → `null` at the
//! relation key, parent + multiple relations in one query, loud
//! errors for unknown parent col / non-FK relation / unknown child
//! col / deeper-than-one-hop path.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::ForeignKey;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "vt_author")]
pub struct Author {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "vt_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<Author>,
    pub editor: Option<ForeignKey<Author>>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Post>()
            .build()
            .expect("App::build");
        sqlx::query(
            "CREATE TABLE vt_author (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                email TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE author");
        sqlx::query(
            "CREATE TABLE vt_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                author INTEGER NOT NULL REFERENCES vt_author(id),
                editor INTEGER REFERENCES vt_author(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE post");
        for (name, email) in &[("Alice", "a@x"), ("Bob", "b@x")] {
            sqlx::query("INSERT INTO vt_author (name, email) VALUES (?, ?)")
                .bind(*name)
                .bind(*email)
                .execute(&pool)
                .await
                .expect("seed author");
        }
        // alpha: author=Alice(1), editor=Bob(2)
        // beta:  author=Bob(2),   editor=NULL
        for (title, author, editor) in &[("alpha", 1_i64, Some(2_i64)), ("beta", 2, None)] {
            sqlx::query("INSERT INTO vt_post (title, author, editor) VALUES (?, ?, ?)")
                .bind(*title)
                .bind(*author)
                .bind(*editor)
                .execute(&pool)
                .await
                .expect("seed post");
        }
    })
    .await;
}

#[tokio::test]
async fn values_traversal_returns_nested_per_relation_object() {
    boot().await;
    let rows = Post::objects()
        .filter(post::TITLE.eq("alpha"))
        .values(&["id", "title", "author__id", "author__name"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 1);
    let row = rows[0].as_object().expect("object row");
    assert_eq!(row.get("title").and_then(|v| v.as_str()), Some("alpha"));
    let author = row
        .get("author")
        .and_then(|v| v.as_object())
        .expect("nested author object");
    assert_eq!(author.get("name").and_then(|v| v.as_str()), Some("Alice"));
    assert_eq!(author.get("id").and_then(|v| v.as_i64()), Some(1));
    // Only the two requested child cols — no email leak.
    assert!(!author.contains_key("email"));
}

#[tokio::test]
async fn values_traversal_multiple_relations_in_one_query() {
    boot().await;
    let rows = Post::objects()
        .filter(post::TITLE.eq("alpha"))
        .values(&["id", "author__name", "editor__name"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 1);
    let row = rows[0].as_object().expect("object");
    let author = row
        .get("author")
        .and_then(|v| v.as_object())
        .expect("author");
    let editor = row
        .get("editor")
        .and_then(|v| v.as_object())
        .expect("editor");
    assert_eq!(author.get("name").and_then(|v| v.as_str()), Some("Alice"));
    assert_eq!(editor.get("name").and_then(|v| v.as_str()), Some("Bob"));
}

#[tokio::test]
async fn left_join_miss_maps_relation_to_null() {
    boot().await;
    // beta has editor = NULL. The relation key should be
    // `Value::Null` — distinct from a nested object full of nulls,
    // so caller code can `obj["editor"].is_null()` branch cleanly.
    let rows = Post::objects()
        .filter(post::TITLE.eq("beta"))
        .values(&["title", "editor__name"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 1);
    let row = rows[0].as_object().expect("object");
    assert_eq!(row.get("title").and_then(|v| v.as_str()), Some("beta"));
    assert!(row.get("editor").map(|v| v.is_null()).unwrap_or(false));
}

#[tokio::test]
async fn values_traversal_works_without_parent_cols() {
    boot().await;
    // Pure relation-only projection — every result row is just the
    // nested author object. Works because the JOIN ON clause uses
    // the parent's `author` FK column even though it doesn't show
    // up in the outer SELECT.
    let rows = Post::objects()
        .filter(post::TITLE.eq("alpha"))
        .values(&["author__name"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 1);
    let row = rows[0].as_object().expect("object");
    assert_eq!(row.len(), 1, "only `author` key present");
    let author = row
        .get("author")
        .and_then(|v| v.as_object())
        .expect("author");
    assert_eq!(author.get("name").and_then(|v| v.as_str()), Some("Alice"));
}

#[tokio::test]
async fn unknown_parent_column_errors_loudly() {
    boot().await;
    let err = Post::objects()
        .values(&["nope", "author__name"])
        .await
        .expect_err("unknown parent col must error");
    assert!(err.to_string().contains("nope"));
}

#[tokio::test]
async fn unknown_relation_errors_loudly() {
    boot().await;
    let err = Post::objects()
        .values(&["not_a_rel__name"])
        .await
        .expect_err("unknown relation must error");
    let msg = err.to_string();
    assert!(msg.contains("not_a_rel"), "names the relation: {msg}");
}

#[tokio::test]
async fn unknown_child_column_errors_loudly() {
    boot().await;
    let err = Post::objects()
        .values(&["author__not_a_col"])
        .await
        .expect_err("unknown child col must error");
    assert!(err.to_string().contains("not_a_col"));
}

#[tokio::test]
async fn deeper_than_one_hop_traversal_errors_loudly() {
    boot().await;
    let err = Post::objects()
        .values(&["author__manager__name"])
        .await
        .expect_err("nested path must error in v1");
    let msg = err.to_string();
    assert!(msg.contains("one-hop"), "names the constraint: {msg}");
}

#[tokio::test]
async fn non_fk_traversal_target_errors_loudly() {
    boot().await;
    // `title` is a plain string col on Post — not a relation.
    let err = Post::objects()
        .values(&["title__upper"])
        .await
        .expect_err("non-FK relation target must error");
    assert!(err.to_string().contains("not a foreign key"));
}

#[tokio::test]
async fn no_double_underscore_path_keeps_existing_values_shape() {
    boot().await;
    // The byte-for-byte unchanged path: no `__` in any col name →
    // the original parent-only values() impl runs.
    let rows = Post::objects()
        .filter(post::TITLE.eq("alpha"))
        .values(&["id", "title"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 1);
    let row = rows[0].as_object().expect("object");
    assert_eq!(row.len(), 2);
    assert!(row.contains_key("id"));
    assert!(row.contains_key("title"));
    assert!(!row.contains_key("author"));
}
