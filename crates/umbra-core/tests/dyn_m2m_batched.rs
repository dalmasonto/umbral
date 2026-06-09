//! gap2 #16 — `DynQuerySet::fetch_as_json` batches M2M echo across
//! every parent row in one query per relation, not one per row.
//!
//! Pins the read shape: each row carries its own `<relation>:
//! [child_id, ...]` array, and rows with no junction links still
//! surface the field as an empty array. Query budget is `1 + count(
//! m2m_relations)` regardless of parent count.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::{DynQuerySet, M2M};
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "m2mb_tag")]
pub struct Tag {
    pub id: i64,
    #[umbra(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "m2mb_post")]
pub struct Post {
    pub id: i64,
    #[umbra(string)]
    pub title: String,
    /// M2M to Tag — junction table `m2mb_post_tags`.
    #[umbra(m2m = "m2mb_tag")]
    pub tags: M2M<Tag>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Tag>()
            .model::<Post>()
            .build()
            .expect("App::build");

        for sql in &[
            "CREATE TABLE m2mb_tag (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL
            )",
            "CREATE TABLE m2mb_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL
            )",
            "CREATE TABLE m2mb_post_tags (
                parent_id INTEGER NOT NULL REFERENCES m2mb_post(id) ON DELETE CASCADE,
                child_id  INTEGER NOT NULL REFERENCES m2mb_tag(id)  ON DELETE CASCADE,
                PRIMARY KEY (parent_id, child_id)
            )",
        ] {
            sqlx::query(sql).execute(&pool).await.expect("ddl");
        }
        for (id, name) in &[(1_i64, "rust"), (2, "web"), (3, "framework")] {
            sqlx::query("INSERT INTO m2mb_tag (id, name) VALUES (?, ?)")
                .bind(*id)
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed tag");
        }
        for (id, title) in &[(1_i64, "p1"), (2, "p2"), (3, "p3"), (4, "p4")] {
            sqlx::query("INSERT INTO m2mb_post (id, title) VALUES (?, ?)")
                .bind(*id)
                .bind(*title)
                .execute(&pool)
                .await
                .expect("seed post");
        }
        // p1 → {rust, web}; p2 → {web, framework}; p3 → {framework}; p4 → {}
        for (parent, child) in &[(1, 1), (1, 2), (2, 2), (2, 3), (3, 3)] {
            sqlx::query("INSERT INTO m2mb_post_tags (parent_id, child_id) VALUES (?, ?)")
                .bind(*parent as i64)
                .bind(*child as i64)
                .execute(&pool)
                .await
                .expect("seed junction");
        }
    })
    .await;
}

fn meta_for(table: &str) -> umbra::migrate::ModelMeta {
    umbra::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .expect("registered")
}

#[tokio::test]
async fn m2m_echo_renders_correct_child_ids_per_parent() {
    boot().await;
    let meta = meta_for("m2mb_post");
    let mut rows = DynQuerySet::for_meta(&meta)
        .order_by_col("id", false)
        .fetch_as_json()
        .await
        .expect("fetch");
    assert!(rows.len() >= 4, "expected at least 4 seeded posts");

    let by_title: std::collections::HashMap<String, serde_json::Map<String, serde_json::Value>> =
        rows.drain(..)
            .map(|r| {
                let title = r
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                (title, r)
            })
            .collect();

    let read_tags = |t: &str| -> Vec<i64> {
        by_title[t]
            .get("tags")
            .and_then(|v| v.as_array())
            .expect("tags array")
            .iter()
            .filter_map(|v| v.as_i64())
            .collect()
    };
    assert_eq!(read_tags("p1"), vec![1, 2]);
    assert_eq!(read_tags("p2"), vec![2, 3]);
    assert_eq!(read_tags("p3"), vec![3]);
    assert_eq!(
        read_tags("p4"),
        Vec::<i64>::new(),
        "rows with no junction links must still echo an empty array"
    );
}
