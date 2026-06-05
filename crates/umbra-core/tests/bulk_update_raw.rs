//! Gaps #20 + #22 — `Manager::bulk_update(instances)` and
//! `Manager::raw(sql)`.
//!
//! - bulk_update: applies per-instance differing values in a single
//!   `UPDATE table SET col = CASE id WHEN .. THEN .. END WHERE id IN (..)`.
//! - raw: thin escape hatch around `sqlx::query_as` that still
//!   returns typed `Vec<T>`.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

static SERIALISE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "bu_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub view_count: i64,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("bulk_update_raw.sqlite");
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
        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .build()
            .expect("App::build");
        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE bu_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                view_count INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE");
    })
    .await;
}

async fn truncate() {
    let pool = umbra::db::pool();
    sqlx::query("DELETE FROM bu_post")
        .execute(&pool)
        .await
        .expect("truncate");
}

// =====================================================================
// bulk_update
// =====================================================================

#[tokio::test]
async fn bulk_update_applies_per_row_differing_values() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    // Seed 3 rows.
    let mut seeds: Vec<Post> = Vec::new();
    for i in 1..=3 {
        let row = Post::objects()
            .create(Post {
                id: 0,
                title: format!("t{i}"),
                view_count: 0,
            })
            .await
            .expect("seed");
        seeds.push(row);
    }
    // Mutate locally: distinct title + view_count per row.
    let mut updates = seeds.clone();
    updates[0].title = "alpha".into();
    updates[0].view_count = 10;
    updates[1].title = "beta".into();
    updates[1].view_count = 20;
    updates[2].title = "gamma".into();
    updates[2].view_count = 30;

    let n = Post::objects()
        .bulk_update(updates)
        .await
        .expect("bulk_update");
    assert_eq!(n, 3, "all three updated");

    let mut all = Post::objects()
        .order_by(post::ID.asc())
        .fetch()
        .await
        .expect("readback");
    all.sort_by_key(|r| r.id);
    assert_eq!(all[0].title, "alpha");
    assert_eq!(all[0].view_count, 10);
    assert_eq!(all[1].title, "beta");
    assert_eq!(all[1].view_count, 20);
    assert_eq!(all[2].title, "gamma");
    assert_eq!(all[2].view_count, 30);
}

#[tokio::test]
async fn bulk_update_empty_input_is_a_noop() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    let n = Post::objects()
        .bulk_update(Vec::<Post>::new())
        .await
        .expect("bulk_update empty");
    assert_eq!(n, 0);
}

// =====================================================================
// raw()
// =====================================================================

#[tokio::test]
async fn raw_returns_typed_rows() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    let _ = Post::objects()
        .create(Post {
            id: 0,
            title: "raw-test".into(),
            view_count: 99,
        })
        .await
        .expect("seed");

    let rows: Vec<Post> = Post::objects()
        .raw("SELECT * FROM bu_post WHERE view_count = 99")
        .await
        .expect("raw fetch");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "raw-test");
    assert_eq!(rows[0].view_count, 99);
}

#[tokio::test]
async fn raw_supports_zero_rows() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    let rows: Vec<Post> = Post::objects()
        .raw("SELECT * FROM bu_post WHERE id = -1")
        .await
        .expect("raw empty fetch");
    assert!(rows.is_empty());
}
