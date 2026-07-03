//! audit_2 core-orm #2 / #4 — the dynamic pool write path is atomic and
//! reports the UPDATE's real affected-row count.
//!
//! #2: `insert_json` / `update_json` run the parent write AND the M2M
//! junction writes on ONE transaction. A junction-write failure rolls the
//! parent back instead of leaving an orphaned (tag-less) row durably
//! committed. We force a junction failure by deliberately NOT creating the
//! junction table — validation checks the *child* table (which exists), so
//! the write starts, the parent row inserts on the tx, and the junction
//! INSERT then fails on the missing table, rolling the whole thing back.
//!
//! #4: `update_json` returns the UPDATE's real `rows_affected()` (from the
//! effective WHERE, which excludes soft-deleted rows) rather than a
//! matched-count derived from a separate SELECT over the raw predicate
//! (which double-counted soft-deleted rows).

#![allow(dead_code)]

use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::OnceCell;

use umbral::migrate::registered_models;
use umbral::orm::{DynQuerySet, M2M};
use umbral_core::db;

/// Serialise the tests in this file. They share one file-backed DB and the
/// `atomtx_post` table; running them concurrently would race on the SQLite
/// write lock and on each other's rows.
fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

// ── models ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "atomtx_tag")]
pub struct Tag {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "atomtx_post")]
pub struct Post {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    /// M2M to Tag — junction table `atomtx_post_tags` (intentionally NOT
    /// created, so the junction write fails after the parent write).
    #[umbral(m2m = "atomtx_tag")]
    pub tags: M2M<Tag>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(soft_delete, table = "atomtx_sd")]
pub struct SoftPost {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        // File-backed so the pool's connections all see the same DB (an
        // in-memory sqlite DB is per-connection, which breaks read-after-
        // write across pool checkouts and across the tx/pool split).
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "umbral_dyn_write_atomicity_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Tag>()
            .model::<Post>()
            .model::<SoftPost>()
            .build()
            .expect("App::build");

        for sql in &[
            "CREATE TABLE atomtx_tag (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL
            )",
            "CREATE TABLE atomtx_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL
            )",
            // NOTE: atomtx_post_tags is deliberately absent.
            "CREATE TABLE atomtx_sd (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                deleted_at TEXT
            )",
        ] {
            sqlx::query(sql).execute(&pool).await.expect("ddl");
        }
        // One tag so the M2M *validation* (which checks the child table)
        // passes and the write actually starts.
        sqlx::query("INSERT INTO atomtx_tag (id, name) VALUES (1, 'rust')")
            .execute(&pool)
            .await
            .expect("seed tag");
    })
    .await;
}

fn meta(table: &str) -> umbral::migrate::ModelMeta {
    registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .expect("registered")
}

// ── #2 insert atomicity ─────────────────────────────────────────────────

#[tokio::test]
async fn insert_json_rolls_back_parent_when_junction_write_fails() {
    let _guard = test_lock().lock().await;
    boot().await;

    let mut body = serde_json::Map::new();
    body.insert("title".to_string(), Value::String("orphan?".to_string()));
    // tag 1 exists → M2M validation passes; the junction INSERT then fails
    // because atomtx_post_tags does not exist.
    body.insert("tags".to_string(), Value::Array(vec![Value::from(1_i64)]));

    let res = DynQuerySet::for_meta(&meta("atomtx_post"))
        .insert_json(&body)
        .await;
    assert!(
        res.is_err(),
        "insert must surface the junction-write failure as Err"
    );

    // The parent must NOT be committed — a failed junction write leaves
    // ZERO new rows, not an orphaned tag-less row. Scope the check to this
    // test's title so a row another test durably seeded can't mask a
    // regression here.
    let remaining = DynQuerySet::for_meta(&meta("atomtx_post"))
        .filter_eq_string("title", "orphan?")
        .count()
        .await
        .expect("count");
    assert_eq!(
        remaining, 0,
        "parent INSERT must roll back with the junction write; found {remaining} orphan(s)"
    );
}

// ── #2 update atomicity ─────────────────────────────────────────────────

#[tokio::test]
async fn update_json_rolls_back_parent_when_junction_write_fails() {
    let _guard = test_lock().lock().await;
    boot().await;

    // Seed a post directly (bypassing the M2M path).
    let pool = db::pool();
    sqlx::query("INSERT INTO atomtx_post (id, title) VALUES (100, 'orig')")
        .execute(&pool)
        .await
        .expect("seed post");

    let mut patch = serde_json::Map::new();
    patch.insert("title".to_string(), Value::String("changed".to_string()));
    patch.insert("tags".to_string(), Value::Array(vec![Value::from(1_i64)]));

    let res = DynQuerySet::for_meta(&meta("atomtx_post"))
        .filter_eq_string("id", "100")
        .update_json(&patch)
        .await;
    assert!(res.is_err(), "update must surface the junction failure");

    // The UPDATE must have rolled back with the failed junction write: the
    // title is unchanged. Read it back with raw SQL — `fetch_as_json`
    // would try to hydrate the (deliberately missing) junction table.
    let title: String = sqlx::query_scalar("SELECT title FROM atomtx_post WHERE id = 100")
        .fetch_one(&pool)
        .await
        .expect("read back");
    assert_eq!(
        title, "orig",
        "the UPDATE must roll back with the junction write, not half-apply"
    );
}

// ── #4 real rows_affected on a soft-delete model ─────────────────────────

#[tokio::test]
async fn update_json_returns_real_rows_affected_excluding_soft_deleted() {
    let _guard = test_lock().lock().await;
    boot().await;

    let pool = db::pool();
    // Two rows sharing title "dup": one live, one soft-deleted.
    sqlx::query("INSERT INTO atomtx_sd (id, title, deleted_at) VALUES (1, 'dup', NULL)")
        .execute(&pool)
        .await
        .expect("seed live");
    sqlx::query(
        "INSERT INTO atomtx_sd (id, title, deleted_at) VALUES (2, 'dup', '2020-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("seed trashed");

    let mut patch = serde_json::Map::new();
    patch.insert("title".to_string(), Value::String("touched".to_string()));

    // Filter matches BOTH rows on the raw predicate, but the effective
    // WHERE (deleted_at IS NULL) skips the soft-deleted one. The return
    // must be the UPDATE's real rows_affected == 1, not the matched-count
    // (2) the old code derived from a raw-predicate SELECT.
    let n = DynQuerySet::for_meta(&meta("atomtx_sd"))
        .filter_eq_string("title", "dup")
        .update_json(&patch)
        .await
        .expect("update");
    assert_eq!(
        n, 1,
        "rows_affected must count only the live row the UPDATE touched, not the soft-deleted one"
    );

    // Read back: the live row changed, the soft-deleted row did not.
    let live = DynQuerySet::for_meta(&meta("atomtx_sd"))
        .filter_eq_string("id", "1")
        .fetch_as_json()
        .await
        .expect("fetch live");
    assert_eq!(live[0]["title"].as_str(), Some("touched"));

    let trashed = DynQuerySet::for_meta(&meta("atomtx_sd"))
        .with_deleted()
        .filter_eq_string("id", "2")
        .fetch_as_json()
        .await
        .expect("fetch trashed");
    assert_eq!(
        trashed[0]["title"].as_str(),
        Some("dup"),
        "the soft-deleted row must be untouched by the update"
    );
}
