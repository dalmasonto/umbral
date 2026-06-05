//! Gap #34 — `Manager::in_bulk(pks)`.
//!
//! Convenience method for the "I have a list of IDs from cache /
//! external system, give me the rows" pattern. Returns
//! `HashMap<PK, T>` so the caller can look up by id without an extra
//! grouping pass.

#![allow(dead_code)]

use std::collections::HashMap;

use sqlx::SqlitePool;
use umbra_core::db;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "ib_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE ib_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
    for title in &["a", "b", "c", "d"] {
        sqlx::query("INSERT INTO ib_post (title) VALUES (?)")
            .bind(*title)
            .execute(&pool)
            .await
            .expect("seed");
    }
    pool
}

#[tokio::test]
async fn in_bulk_returns_hashmap_keyed_by_pk() {
    let pool = fresh_pool().await;
    let by_id: HashMap<i64, Post> = Post::objects()
        .on(&pool)
        .in_bulk(vec![1, 3])
        .await
        .expect("in_bulk");
    assert_eq!(by_id.len(), 2);
    assert_eq!(by_id[&1].title, "a");
    assert_eq!(by_id[&3].title, "c");
}

#[tokio::test]
async fn in_bulk_with_empty_input_returns_empty_map() {
    let pool = fresh_pool().await;
    let by_id: HashMap<i64, Post> = Post::objects()
        .on(&pool)
        .in_bulk(Vec::<i64>::new())
        .await
        .expect("in_bulk empty");
    assert!(by_id.is_empty());
}

#[tokio::test]
async fn in_bulk_silently_skips_missing_ids() {
    let pool = fresh_pool().await;
    let by_id: HashMap<i64, Post> = Post::objects()
        .on(&pool)
        .in_bulk(vec![1, 999, 2])
        .await
        .expect("in_bulk");
    assert_eq!(by_id.len(), 2, "999 not present; just skipped");
    assert!(by_id.contains_key(&1));
    assert!(by_id.contains_key(&2));
}
