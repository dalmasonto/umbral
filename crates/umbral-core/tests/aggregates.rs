//! Gap 13 — `aggregate()` + `annotate()` with Count / Sum / Avg / Max / Min.
//!
//! - `aggregate(&[(name, Aggregate)])` runs a single-row aggregate and
//!   returns `Value::Object`.
//! - `annotate(group_cols, &[(name, Aggregate)])` runs a grouped
//!   aggregate and returns `Vec<Value::Object>` — one row per group.
//!
//! Both compose with `filter` / `exclude` so the WHERE clause is
//! applied before aggregation.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use umbral::orm::Aggregate;
use umbral_core::db;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "agg_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub author_id: i64,
    pub view_count: i64,
    pub published: bool,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE agg_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author_id INTEGER NOT NULL,
            view_count INTEGER NOT NULL DEFAULT 0,
            published BOOLEAN NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
    // author 1: 100 + 50 published, 10 draft     => published views=150, total=160
    // author 2: 200 published, 300 draft         => published views=200, total=500
    for (title, author, views, published) in &[
        ("p1", 1i64, 100i64, true),
        ("p2", 1, 50, true),
        ("p3", 1, 10, false),
        ("p4", 2, 200, true),
        ("p5", 2, 300, false),
    ] {
        sqlx::query(
            "INSERT INTO agg_post (title, author_id, view_count, published) VALUES (?,?,?,?)",
        )
        .bind(*title)
        .bind(*author)
        .bind(*views)
        .bind(*published)
        .execute(&pool)
        .await
        .expect("seed");
    }
    pool
}

// =====================================================================
// .aggregate(...) — single-row aggregate
// =====================================================================

#[tokio::test]
async fn aggregate_count_star_counts_all_rows() {
    let pool = fresh_pool().await;
    let v = Post::objects()
        .on(&pool)
        .aggregate(&[("n", Aggregate::count())])
        .await
        .expect("aggregate");
    assert_eq!(v["n"], json!(5));
}

#[tokio::test]
async fn aggregate_count_respects_filter() {
    let pool = fresh_pool().await;
    let v = Post::objects()
        .filter(post::PUBLISHED.eq(true))
        .on(&pool)
        .aggregate(&[("n", Aggregate::count())])
        .await
        .expect("aggregate");
    assert_eq!(v["n"], json!(3));
}

#[tokio::test]
async fn aggregate_sum_avg_max_min_in_one_call() {
    let pool = fresh_pool().await;
    let v = Post::objects()
        .filter(post::PUBLISHED.eq(true))
        .on(&pool)
        .aggregate(&[
            ("total", Aggregate::sum("view_count")),
            ("avg", Aggregate::avg("view_count")),
            ("max", Aggregate::max("view_count")),
            ("min", Aggregate::min("view_count")),
        ])
        .await
        .expect("aggregate");
    // Published rows: 100, 50, 200 — sum=350, avg≈116.67, max=200, min=50.
    assert_eq!(v["total"], json!(350));
    let avg = v["avg"].as_f64().expect("avg is number");
    assert!((avg - 350.0 / 3.0).abs() < 0.01, "avg ≈ 116.67, got {avg}");
    assert_eq!(v["max"], json!(200));
    assert_eq!(v["min"], json!(50));
}

#[tokio::test]
async fn aggregate_unknown_column_errors() {
    let pool = fresh_pool().await;
    let err = Post::objects()
        .on(&pool)
        .aggregate(&[("x", Aggregate::sum("not_a_col"))])
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not_a_col"),
        "error should name unknown column; got: {msg}"
    );
}

#[tokio::test]
async fn aggregate_empty_table_returns_null_for_sum() {
    let pool = fresh_pool().await;
    sqlx::query("DELETE FROM agg_post")
        .execute(&pool)
        .await
        .expect("clear");
    let v = Post::objects()
        .on(&pool)
        .aggregate(&[
            ("count", Aggregate::count()),
            ("total", Aggregate::sum("view_count")),
        ])
        .await
        .expect("aggregate");
    // COUNT(*) is 0 even on an empty table; SUM is NULL.
    assert_eq!(v["count"], json!(0));
    assert!(v["total"].is_null(), "SUM over empty set is NULL");
}

// =====================================================================
// .annotate(group_cols, ...) — grouped aggregate
// =====================================================================

#[tokio::test]
async fn annotate_groups_by_author_and_counts() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .on(&pool)
        .annotate(&["author_id"], &[("post_count", Aggregate::count())])
        .await
        .expect("annotate");
    assert_eq!(rows.len(), 2);
    let mut by_author: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for r in &rows {
        let a = r["author_id"].as_i64().unwrap();
        let c = r["post_count"].as_i64().unwrap();
        by_author.insert(a, c);
    }
    assert_eq!(by_author[&1], 3);
    assert_eq!(by_author[&2], 2);
}

#[tokio::test]
async fn annotate_sum_per_author_after_filter() {
    let pool = fresh_pool().await;
    // Sum of view_count per author, considering only published posts.
    let rows = Post::objects()
        .filter(post::PUBLISHED.eq(true))
        .on(&pool)
        .annotate(&["author_id"], &[("total", Aggregate::sum("view_count"))])
        .await
        .expect("annotate");
    assert_eq!(rows.len(), 2);
    let mut totals: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for r in &rows {
        let a = r["author_id"].as_i64().unwrap();
        let t = r["total"].as_i64().unwrap();
        totals.insert(a, t);
    }
    assert_eq!(totals[&1], 150); // 100 + 50
    assert_eq!(totals[&2], 200);
}
