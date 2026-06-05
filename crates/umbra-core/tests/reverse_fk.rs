//! Gap 30 — reverse FK accessors emitted by `#[derive(Model)]`.
//!
//! For every `ForeignKey<T>` field on a child model, the macro emits an
//! `impl T { fn <child_snake>_set(&self) -> QuerySet<Child> }` method
//! that returns a QuerySet pre-filtered to the FK column. This is the
//! Rust equivalent of Django's `user.comment_set.all()` — the parent
//! gets a typed accessor for each child relation pointing at it.

#![allow(dead_code)]

use sqlx::SqlitePool;
use umbra::orm::ForeignKey;
use umbra_core::db;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "rfk_user")]
pub struct User {
    pub id: i64,
    pub name: String,
}

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "rfk_comment")]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub author: ForeignKey<User>,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE rfk_user (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE rfk_user");
    sqlx::query(
        "CREATE TABLE rfk_comment (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            body TEXT NOT NULL,
            author INTEGER NOT NULL REFERENCES rfk_user(id)
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE rfk_comment");
    sqlx::query("INSERT INTO rfk_user (id, name) VALUES (1, 'Alice'), (2, 'Bob')")
        .execute(&pool)
        .await
        .expect("seed users");
    sqlx::query(
        "INSERT INTO rfk_comment (id, body, author) VALUES \
            (1, 'a1', 1), (2, 'a2', 1), (3, 'b1', 2)",
    )
    .execute(&pool)
    .await
    .expect("seed comments");
    pool
}

#[tokio::test]
async fn parent_gets_comment_set_accessor() {
    let pool = fresh_pool().await;
    let alice = User {
        id: 1,
        name: "Alice".into(),
    };
    let comments = alice
        .comment_set()
        .on(&pool)
        .fetch()
        .await
        .expect("comment_set fetch");
    assert_eq!(comments.len(), 2);
    assert!(comments.iter().all(|c| c.author.id() == 1));
}

#[tokio::test]
async fn comment_set_composes_with_filter_and_order_by() {
    let pool = fresh_pool().await;
    let alice = User {
        id: 1,
        name: "Alice".into(),
    };
    let comments = alice
        .comment_set()
        .filter(comment::ID.lt(2))
        .order_by(comment::ID.desc())
        .on(&pool)
        .fetch()
        .await
        .expect("filter+order");
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].body, "a1");
}

#[tokio::test]
async fn comment_set_returns_empty_for_parent_with_no_children() {
    let pool = fresh_pool().await;
    let ghost = User {
        id: 999,
        name: "Ghost".into(),
    };
    let comments = ghost
        .comment_set()
        .on(&pool)
        .fetch()
        .await
        .expect("ghost fetch");
    assert!(comments.is_empty());
}
