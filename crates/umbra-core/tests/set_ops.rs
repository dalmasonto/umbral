//! Gap #28 — `QuerySet::union` / `intersect` / `except`.
//!
//! Combine two QuerySets of the same `T` into a single result set
//! via SQL's `UNION` / `INTERSECT` / `EXCEPT`. Both backends ship
//! all three; the v1 forms emit the de-duplicating variant
//! (UNION DISTINCT, not UNION ALL).

#![allow(dead_code)]

use sqlx::SqlitePool;
use umbra_core::db;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "so_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub published: bool,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE so_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            published BOOLEAN NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
    // 1=alpha (pub), 2=beta (pub), 3=gamma (draft), 4=delta (pub)
    for (title, pub_) in &[
        ("alpha", true),
        ("beta", true),
        ("gamma", false),
        ("delta", true),
    ] {
        sqlx::query("INSERT INTO so_post (title, published) VALUES (?, ?)")
            .bind(*title)
            .bind(*pub_)
            .execute(&pool)
            .await
            .expect("seed");
    }
    pool
}

#[tokio::test]
async fn union_combines_and_dedupes() {
    let pool = fresh_pool().await;
    // id <= 2  union  id >= 2  → ids {1, 2, 3, 4}  (2 appears once).
    let a = Post::objects().filter(post::ID.le(2));
    let b = Post::objects().filter(post::ID.ge(2));
    let mut rows = a.union(b).on(&pool).fetch().await.expect("union");
    rows.sort_by_key(|p| p.id);
    let ids: Vec<i64> = rows.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn intersect_keeps_only_shared_rows() {
    let pool = fresh_pool().await;
    // published  intersect  id >= 3  → just id=4 (delta)
    let pub_ = Post::objects().filter(post::PUBLISHED.eq(true));
    let high = Post::objects().filter(post::ID.ge(3));
    let rows = pub_
        .intersect(high)
        .on(&pool)
        .fetch()
        .await
        .expect("intersect");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, 4);
}

#[tokio::test]
async fn except_removes_rows_in_second() {
    let pool = fresh_pool().await;
    // published  except  id == 2  → ids 1, 4 (drop beta)
    let pub_ = Post::objects().filter(post::PUBLISHED.eq(true));
    let two = Post::objects().filter(post::ID.eq(2));
    let mut rows = pub_.except(two).on(&pool).fetch().await.expect("except");
    rows.sort_by_key(|p| p.id);
    let ids: Vec<i64> = rows.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec![1, 4]);
}
