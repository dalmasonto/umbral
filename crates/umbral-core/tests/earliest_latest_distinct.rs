//! Gaps #37 and #17 — `earliest()` / `latest()` sugar + `distinct()`.
//!
//! - `earliest(col)` = `order_by(col.asc()).first()`.
//! - `latest(col)`   = `order_by(col.desc()).first()`.
//! - `distinct()`    = `SELECT DISTINCT ...` on the QuerySet.

#![allow(dead_code)]

use sqlx::SqlitePool;
use umbral_core::db;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
)]
#[umbral(table = "el_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub view_count: i64,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE el_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            view_count INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
    // ids 1..=4 with view_counts 100, 30, 80, 30 (dup view_count).
    for (title, views) in &[("a", 100i64), ("b", 30), ("c", 80), ("d", 30)] {
        sqlx::query("INSERT INTO el_post (title, view_count) VALUES (?, ?)")
            .bind(*title)
            .bind(*views)
            .execute(&pool)
            .await
            .expect("seed");
    }
    pool
}

#[tokio::test]
async fn earliest_returns_min_by_column() {
    let pool = fresh_pool().await;
    let row = Post::objects()
        .on(&pool)
        .earliest("view_count")
        .await
        .expect("earliest")
        .expect("some row");
    assert_eq!(row.view_count, 30, "smallest view_count wins");
}

#[tokio::test]
async fn latest_returns_max_by_column() {
    let pool = fresh_pool().await;
    let row = Post::objects()
        .on(&pool)
        .latest("view_count")
        .await
        .expect("latest")
        .expect("some row");
    assert_eq!(row.view_count, 100);
}

#[tokio::test]
async fn earliest_on_empty_set_returns_none() {
    let pool = fresh_pool().await;
    sqlx::query("DELETE FROM el_post")
        .execute(&pool)
        .await
        .expect("clear");
    let row = Post::objects()
        .on(&pool)
        .earliest("view_count")
        .await
        .expect("earliest");
    assert!(row.is_none());
}

#[tokio::test]
async fn distinct_renders_select_distinct() {
    let pool = fresh_pool().await;
    let sql = Post::objects()
        .on(&pool)
        .distinct()
        .to_sql()
        .to_ascii_lowercase();
    assert!(
        sql.contains("select distinct"),
        "expected SELECT DISTINCT; got: {sql}"
    );
}

#[tokio::test]
async fn explain_returns_a_non_empty_plan() {
    let pool = fresh_pool().await;
    let plan = Post::objects()
        .filter(post::ID.gt(0))
        .on(&pool)
        .explain()
        .await
        .expect("explain");
    assert!(!plan.is_empty(), "plan should have at least one line");
    // SQLite's EXPLAIN QUERY PLAN always mentions either SCAN or
    // SEARCH for the queried table.
    let lower = plan.to_ascii_lowercase();
    assert!(
        lower.contains("el_post"),
        "plan should mention the table name; got: {plan}"
    );
}

#[tokio::test]
async fn distinct_dedups_via_values_projection() {
    // Distinct's most visible effect is on a column-projected query.
    // Pair it with `.values(&["view_count"])` and assert the dupe
    // (30 appears twice in seed) folds.
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .on(&pool)
        .distinct()
        .values(&["view_count"])
        .await
        .expect("distinct + values");
    let mut got: Vec<i64> = rows
        .iter()
        .map(|r| r["view_count"].as_i64().unwrap())
        .collect();
    got.sort();
    // Distinct (30, 80, 100) — three values, not four.
    assert_eq!(got, vec![30, 80, 100]);
}
