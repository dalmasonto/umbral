//! Phase 1, Task 1 — `Relation<T>` handle + single forward-FK resolution.
//!
//! A hand-built `Relation` for a forward FK (`Post.author -> Author`) must
//! resolve to the referenced row when awaited. Codegen that emits
//! `post.author()` lands in Task 5; here the `HopSpec` is constructed by
//! hand to prove the underlying handle resolves correctly.
//!
//! Behavioral: real rows through the actual public accessor, read the row
//! back — no SQL-string-only assertions.

#![allow(dead_code)]

use sqlx::SqlitePool;
use umbral::orm::relation::{HopKind, HopSpec, to_one_hop};
use umbral::prelude::*;
use umbral_core::db;

// =========================================================================
// Model declarations
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "author")]
pub struct Author {
    #[umbral(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "post")]
pub struct Post {
    #[umbral(primary_key)]
    id: i64,
    title: String,
    author: ForeignKey<Author>,
}

// =========================================================================
// Harness
// =========================================================================

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");

    sqlx::query(
        "CREATE TABLE author (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE author");

    sqlx::query(
        "CREATE TABLE post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author INTEGER NOT NULL REFERENCES author(id)
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE post");

    pool
}

async fn insert_author(pool: &SqlitePool, name: &str) -> Author {
    sqlx::query_as::<sqlx::Sqlite, Author>(
        "INSERT INTO author (name) VALUES (?) RETURNING id, name",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("insert author")
}

async fn insert_post(pool: &SqlitePool, title: &str, author_id: i64) -> Post {
    sqlx::query_as::<sqlx::Sqlite, Post>(
        "INSERT INTO post (title, author) VALUES (?, ?) RETURNING id, title, author",
    )
    .bind(title)
    .bind(author_id)
    .fetch_one(pool)
    .await
    .expect("insert post")
}

fn author_hop() -> HopSpec {
    HopSpec {
        kind: HopKind::Fk,
        from_table: "post",
        to_table: "author",
        fk_column: "author",
        fk_on_from: true,
        required: true,
        junction: None,
    }
}

// =========================================================================
// Tests
// =========================================================================

/// The core round-trip: build the forward-FK relation by hand and await it
/// to the referenced `Author` row.
#[tokio::test]
async fn forward_fk_relation_awaits_to_the_row() {
    let pool = fresh_pool().await;
    let ada = insert_author(&pool, "ada").await;
    let _ = insert_post(&pool, "Hello", ada.id).await;

    let post = Post::objects().on(&pool).get().await.expect("get post");

    // `.get()` terminal.
    let author: Author = to_one_hop(&post, author_hop())
        .on(&pool)
        .get()
        .await
        .expect("resolve author");
    assert_eq!(author.name, "ada", "relation must read the real row back");
    assert_eq!(author.id, ada.id);
}

/// Awaiting the handle directly (via `IntoFuture`) is an alias of `.get()`.
#[tokio::test]
async fn forward_fk_relation_into_future_awaits() {
    let pool = fresh_pool().await;
    let ada = insert_author(&pool, "grace").await;
    let _ = insert_post(&pool, "Hi", ada.id).await;

    let post = Post::objects().on(&pool).get().await.expect("get post");

    let author: Author = to_one_hop(&post, author_hop())
        .on(&pool)
        .await
        .expect("await author");
    assert_eq!(author.name, "grace");
}

/// `get_opt()` returns `Some` when the target exists.
#[tokio::test]
async fn forward_fk_get_opt_some() {
    let pool = fresh_pool().await;
    let ada = insert_author(&pool, "ada").await;
    let _ = insert_post(&pool, "Hello", ada.id).await;

    let post = Post::objects().on(&pool).get().await.expect("get post");

    let author: Option<Author> = to_one_hop(&post, author_hop())
        .on(&pool)
        .get_opt()
        .await
        .expect("get_opt");
    assert_eq!(author.map(|a| a.name), Some("ada".to_string()));
}

/// `exists()` is true when the target row exists.
#[tokio::test]
async fn forward_fk_exists_true() {
    let pool = fresh_pool().await;
    let ada = insert_author(&pool, "ada").await;
    let _ = insert_post(&pool, "Hello", ada.id).await;

    let post = Post::objects().on(&pool).get().await.expect("get post");

    let exists = to_one_hop::<Post, Author>(&post, author_hop())
        .on(&pool)
        .exists()
        .await
        .expect("exists");
    assert!(exists, "the referenced author row exists");
}

/// A dangling FK (target row missing) makes `.get()` an error, never a
/// silent `None` — the referential contract for a required FK.
#[tokio::test]
async fn forward_fk_missing_target_is_error() {
    // Simulate a dangling FK the way a corrupted / partially-migrated DB
    // would hold one: a `post` table with no DB-level REFERENCES so a row
    // pointing at a non-existent author id is insertable (FK enforcement is
    // on by default and pooled per-connection, so a runtime PRAGMA won't
    // reliably drop it — omit the constraint from this table instead).
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query("CREATE TABLE author (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("CREATE TABLE author");
    sqlx::query(
        "CREATE TABLE post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE post (no FK)");
    let post = insert_post(&pool, "Hello", 999).await;

    let result: Result<Author, _> = to_one_hop(&post, author_hop()).on(&pool).get().await;
    assert!(
        result.is_err(),
        "a missing required-FK target must surface as an Err, not a silent None"
    );

    // …but `get_opt()` reports the absence as `None`.
    let opt: Option<Author> = to_one_hop(&post, author_hop())
        .on(&pool)
        .get_opt()
        .await
        .expect("get_opt should not error on absence");
    assert!(opt.is_none(), "get_opt yields None for a missing target");
}
