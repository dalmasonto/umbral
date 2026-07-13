//! PK refactor — `in_bulk` on a String-PK model. The result map is keyed
//! by `T::PrimaryKey`, so a slug-keyed model returns a
//! `HashMap<String, Tag>`. Before the lift `in_bulk` took `Vec<i64>` and
//! returned `HashMap<i64, T>`, silently dropping every non-i64 row.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ib_tag")]
pub struct Tag {
    #[umbral(primary_key)]
    pub slug: String,
    pub label: String,
}

async fn boot() -> SqlitePool {
    let settings = umbral::Settings::from_env().expect("settings");
    let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Tag>()
        .build()
        .expect("App::build");
    umbral_core::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema");
    for (slug, label) in &[("rust", "Rust"), ("go", "Go"), ("zig", "Zig")] {
        sqlx::query("INSERT INTO ib_tag (slug, label) VALUES (?, ?)")
            .bind(*slug)
            .bind(*label)
            .execute(&pool)
            .await
            .expect("seed");
    }
    pool
}

#[tokio::test]
async fn in_bulk_keys_the_map_by_string_pk() {
    let pool = boot().await;
    let map = Tag::objects()
        .on(&pool)
        .in_bulk(vec![
            "rust".to_string(),
            "zig".to_string(),
            "missing".to_string(),
        ])
        .await
        .expect("in_bulk");

    // Keyed by the String PK; missing slugs are simply absent.
    assert_eq!(map.len(), 2);
    assert_eq!(map.get("rust").map(|t| t.label.as_str()), Some("Rust"));
    assert_eq!(map.get("zig").map(|t| t.label.as_str()), Some("Zig"));
    assert!(!map.contains_key("missing"));
    assert!(!map.contains_key("go"), "go wasn't requested");
}
