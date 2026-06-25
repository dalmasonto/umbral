//! Gap #31 follow-up — live-SQLite coverage for `JsonCol::has_key`
//! and `JsonCol::path_text`. The existing `json_ops.rs` tests pin
//! the Postgres SQL render shape; this file proves the SQLite
//! fallback (`json_extract IS NOT NULL` for has_key, `json_extract(col, '$.a.b') = ?`
//! for path_text) actually returns the right rows end-to-end.

#![allow(dead_code)]

use serde_json::json;
use sqlx::SqlitePool;
use umbral_core::db;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
)]
#[umbral(table = "jsl_doc")]
pub struct Doc {
    pub id: i64,
    pub meta: serde_json::Value,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE jsl_doc (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            meta TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
    for (id, j) in &[
        (1i64, json!({"name": "alpha", "nested": {"role": "admin"}})),
        (2, json!({"name": "beta", "nested": {"role": "user"}})),
        (3, json!({"other": "gamma"})),
    ] {
        sqlx::query("INSERT INTO jsl_doc (id, meta) VALUES (?, ?)")
            .bind(*id)
            .bind(j.to_string())
            .execute(&pool)
            .await
            .expect("seed");
    }
    pool
}

#[tokio::test]
async fn sqlite_has_key_filters_present() {
    let pool = fresh_pool().await;
    let rows = Doc::objects()
        .filter(doc::META.has_key("name"))
        .on(&pool)
        .fetch()
        .await
        .expect("has_key on SQLite");
    let mut ids: Vec<i64> = rows.iter().map(|d| d.id).collect();
    ids.sort();
    assert_eq!(ids, vec![1, 2]);
}

#[tokio::test]
async fn sqlite_has_key_returns_empty_for_missing_key() {
    let pool = fresh_pool().await;
    let rows = Doc::objects()
        .filter(doc::META.has_key("zzz-missing"))
        .on(&pool)
        .fetch()
        .await
        .expect("has_key miss");
    assert!(rows.is_empty());
}

#[tokio::test]
async fn sqlite_path_text_eq_drills_into_nested_value() {
    let pool = fresh_pool().await;
    let rows = Doc::objects()
        .filter(doc::META.path_text(&["nested", "role"]).eq("admin"))
        .on(&pool)
        .fetch()
        .await
        .expect("path_text on SQLite");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, 1);
}

#[tokio::test]
async fn sqlite_path_text_ne_drills_into_nested_value() {
    let pool = fresh_pool().await;
    let rows = Doc::objects()
        .filter(doc::META.path_text(&["nested", "role"]).ne("admin"))
        .on(&pool)
        .fetch()
        .await
        .expect("path_text ne on SQLite");
    // id=2 has role=user; id=3 has no nested.role (NULL <> 'admin'
    // is NULL, which is falsy in WHERE — only id=2 returns).
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, 2);
}
