//! Gap 16 — `QuerySet::values(&[&str])`: SELECT-only-named-columns
//! projection that yields `Vec<serde_json::Value::Object>` rows instead
//! of typed `T` instances. The cost-of-large-list-views fix — skip the
//! 50KB body BLOB when only `id` and `title` are needed.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use umbra_core::db;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "vals_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub published: bool,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE vals_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            body TEXT NOT NULL,
            published BOOLEAN NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
    for (title, body, published) in &[
        ("a", "body-a", true),
        ("b", "body-b", false),
        ("c", "body-c", true),
    ] {
        sqlx::query("INSERT INTO vals_post (title, body, published) VALUES (?, ?, ?)")
            .bind(*title)
            .bind(*body)
            .bind(*published)
            .execute(&pool)
            .await
            .expect("seed");
    }
    pool
}

#[tokio::test]
async fn values_returns_only_named_columns() {
    let pool = fresh_pool().await;
    let rows: Vec<Value> = Post::objects()
        .on(&pool)
        .values(&["id", "title"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 3);
    for row in &rows {
        let obj = row.as_object().expect("object");
        assert_eq!(
            obj.len(),
            2,
            "only id and title; got keys: {:?}",
            obj.keys().collect::<Vec<_>>()
        );
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("title"));
        assert!(!obj.contains_key("body"));
        assert!(!obj.contains_key("published"));
    }
}

#[tokio::test]
async fn values_preserves_column_types() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .on(&pool)
        .values(&["id", "title", "published"])
        .await
        .expect("values");
    let first = &rows[0];
    assert!(first["id"].is_number(), "id is number");
    assert!(first["title"].is_string(), "title is string");
    assert!(first["published"].is_boolean(), "published is bool");
}

#[tokio::test]
async fn values_composes_with_filter_and_order_by() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .filter(post::PUBLISHED.eq(true))
        .order_by(post::ID.desc())
        .on(&pool)
        .values(&["title"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 2);
    // Descending id: 'c' (id=3) then 'a' (id=1)
    assert_eq!(rows[0]["title"], json!("c"));
    assert_eq!(rows[1]["title"], json!("a"));
}

#[tokio::test]
async fn values_unknown_column_errors() {
    let pool = fresh_pool().await;
    let err = Post::objects()
        .on(&pool)
        .values(&["title", "nope_not_a_col"])
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("nope_not_a_col"),
        "error should name unknown column; got: {msg}"
    );
}

#[tokio::test]
async fn values_on_manager_works_without_filter() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .on(&pool)
        .values(&["id"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 3);
}
