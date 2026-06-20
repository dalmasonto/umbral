//! gaps2 #35 — soft-delete on the dynamic path.
//!
//! Verifies that `DynQuerySet` (the runtime-typed queryset used by admin
//! and REST) honours soft-delete on models tagged `#[umbra(soft_delete)]`:
//!
//! (a) Default `DynQuerySet` list/count excludes soft-deleted rows.
//! (b) `DynQuerySet::delete()` soft-deletes — the row stays in the DB
//!     with `deleted_at` set, and the default scope can no longer see it.
//! (c) `DynQuerySet::with_deleted()` brings trashed rows back into scope.
//! (d) `DynQuerySet::only_deleted()` restricts to trashed rows only.
//! (e) `DynQuerySet::hard_delete()` hard-purges a (soft-deleted) row.
//! (f) A model WITHOUT `#[umbra(soft_delete)]` hard-deletes as before.

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use umbra::migrate::registered_models;
use umbra::orm::DynQuerySet;
use umbra_core::db;

// ── test models ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(soft_delete, table = "dynsdt_post")]
pub struct DynSdPost {
    pub id: i64,
    #[umbra(string)]
    pub title: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "dynsdt_hard")]
pub struct DynHardPost {
    pub id: i64,
    #[umbra(string)]
    pub title: String,
}

// ── singleton boot ─────────────────────────────────────────────────────────

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults always load");
        // File-backed DB so all tokio runtimes share one connection pool.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbra_soft_delete_dynamic_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<DynSdPost>()
            .model::<DynHardPost>()
            .build()
            .expect("App::build");
        sqlx::query(
            "CREATE TABLE dynsdt_post (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                title      TEXT NOT NULL,
                deleted_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create dynsdt_post");
        sqlx::query(
            "CREATE TABLE dynsdt_hard (
                id    INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create dynsdt_hard");
    })
    .await;
}

fn soft_meta() -> umbra::migrate::ModelMeta {
    registered_models()
        .into_iter()
        .find(|m| m.table == "dynsdt_post")
        .expect("dynsdt_post registered")
}

fn hard_meta() -> umbra::migrate::ModelMeta {
    registered_models()
        .into_iter()
        .find(|m| m.table == "dynsdt_hard")
        .expect("dynsdt_hard registered")
}

// Helper: insert a soft-delete-model row via DynQuerySet.
async fn insert_sd(title: &str) {
    let meta = soft_meta();
    let mut body = serde_json::Map::new();
    body.insert("title".into(), serde_json::Value::String(title.to_string()));
    DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect("insert_json on soft-delete model");
}

// Helper: insert a hard-delete-model row via DynQuerySet.
async fn insert_hard(title: &str) {
    let meta = hard_meta();
    let mut body = serde_json::Map::new();
    body.insert("title".into(), serde_json::Value::String(title.to_string()));
    DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect("insert_json on hard-delete model");
}

// ── (a) default scope excludes trashed rows ────────────────────────────────

#[tokio::test]
async fn dyn_default_scope_excludes_trashed_rows() {
    boot().await;
    let pid = std::process::id();
    let live = format!("dyn-live-{pid}-a");
    let dead = format!("dyn-dead-{pid}-a");
    insert_sd(&live).await;
    insert_sd(&dead).await;

    // Soft-delete the "dead" row via the dynamic path.
    let meta = soft_meta();
    let deleted = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &dead)
        .delete()
        .await
        .expect("dyn soft-delete");
    assert_eq!(deleted, 1, "one row affected");

    // Default fetch must hide the trashed row.
    let rows = DynQuerySet::for_meta(&meta)
        .fetch_as_json()
        .await
        .expect("fetch_as_json");
    let titles: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.get("title").and_then(|v| v.as_str()))
        .collect();
    assert!(titles.contains(&live.as_str()), "live row must be visible");
    assert!(
        !titles.contains(&dead.as_str()),
        "trashed row must be hidden in default scope"
    );

    // count() must also exclude the trashed row.
    let live_count = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &live)
        .count()
        .await
        .expect("count live");
    assert_eq!(live_count, 1, "count of live row must be 1");

    let dead_count = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &dead)
        .count()
        .await
        .expect("count dead");
    assert_eq!(dead_count, 0, "count of trashed row must be 0 in default scope");
}

// ── (b) DynQuerySet::delete() soft-deletes — row stays in DB ──────────────

#[tokio::test]
async fn dyn_delete_soft_deletes_row_stays_in_db() {
    boot().await;
    let pid = std::process::id();
    let title = format!("dyn-sd-{pid}-b");
    insert_sd(&title).await;

    let meta = soft_meta();
    let affected = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &title)
        .delete()
        .await
        .expect("dyn delete");
    assert_eq!(affected, 1, "exactly one row soft-deleted");

    // Row is absent from the default (live) scope.
    let live = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &title)
        .count()
        .await
        .expect("count live");
    assert_eq!(live, 0, "soft-deleted row must not appear in default scope");

    // Row IS present (with deleted_at set) when using with_deleted().
    let all = DynQuerySet::for_meta(&meta)
        .with_deleted()
        .filter_eq_string("title", &title)
        .fetch_as_json()
        .await
        .expect("fetch with_deleted");
    assert_eq!(all.len(), 1, "row must still exist in DB after soft-delete");
    let deleted_at = all[0].get("deleted_at").expect("deleted_at column present");
    assert!(
        !deleted_at.is_null(),
        "deleted_at must be set after soft-delete, got: {deleted_at:?}"
    );
}

// ── (c) with_deleted() brings trashed rows into scope ─────────────────────

#[tokio::test]
async fn dyn_with_deleted_includes_trashed_rows() {
    boot().await;
    let pid = std::process::id();
    let live = format!("dyn-live-{pid}-c");
    let dead = format!("dyn-dead-{pid}-c");
    insert_sd(&live).await;
    insert_sd(&dead).await;

    let meta = soft_meta();
    DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &dead)
        .delete()
        .await
        .expect("soft-delete");

    // with_deleted() must see both.
    let rows = DynQuerySet::for_meta(&meta)
        .with_deleted()
        .fetch_as_json()
        .await
        .expect("fetch with_deleted");
    let titles: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.get("title").and_then(|v| v.as_str()))
        .collect();
    assert!(titles.contains(&live.as_str()), "live row present");
    assert!(titles.contains(&dead.as_str()), "trashed row present with with_deleted()");
}

// ── (d) only_deleted() restricts to trashed rows only ─────────────────────

#[tokio::test]
async fn dyn_only_deleted_restricts_to_trash() {
    boot().await;
    let pid = std::process::id();
    let live = format!("dyn-live-{pid}-d");
    let dead = format!("dyn-dead-{pid}-d");
    insert_sd(&live).await;
    insert_sd(&dead).await;

    let meta = soft_meta();
    DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &dead)
        .delete()
        .await
        .expect("soft-delete");

    let rows = DynQuerySet::for_meta(&meta)
        .only_deleted()
        .fetch_as_json()
        .await
        .expect("fetch only_deleted");
    let titles: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.get("title").and_then(|v| v.as_str()))
        .collect();
    assert!(
        titles.contains(&dead.as_str()),
        "trashed row present in only_deleted()"
    );
    assert!(
        !titles.contains(&live.as_str()),
        "live row must NOT appear in only_deleted()"
    );
}

// ── (e) hard_delete() purges row from DB entirely ─────────────────────────

#[tokio::test]
async fn dyn_hard_delete_purges_row_from_db() {
    boot().await;
    let pid = std::process::id();
    let title = format!("dyn-purge-{pid}-e");
    insert_sd(&title).await;

    let meta = soft_meta();
    // Soft-delete first, then hard-purge via with_deleted() + hard_delete().
    DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &title)
        .delete()
        .await
        .expect("soft-delete");

    let affected = DynQuerySet::for_meta(&meta)
        .with_deleted()
        .hard_delete()
        .filter_eq_string("title", &title)
        .delete()
        .await
        .expect("hard_delete");
    assert_eq!(affected, 1, "hard_delete must affect one row");

    // Even with_deleted() can't find it — row is gone.
    let count = DynQuerySet::for_meta(&meta)
        .with_deleted()
        .filter_eq_string("title", &title)
        .count()
        .await
        .expect("count after hard_delete");
    assert_eq!(count, 0, "row must be gone after hard_delete");
}

// ── (f) non-soft-delete model: DynQuerySet::delete() hard-deletes ─────────

#[tokio::test]
async fn dyn_non_soft_model_hard_deletes() {
    boot().await;
    let pid = std::process::id();
    let title = format!("dyn-hard-{pid}-f");
    insert_hard(&title).await;

    let meta = hard_meta();
    assert!(!meta.soft_delete, "hard model must not have soft_delete flag");

    let affected = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &title)
        .delete()
        .await
        .expect("hard delete on non-soft model");
    assert_eq!(affected, 1, "one row hard-deleted");

    let count = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", &title)
        .count()
        .await
        .expect("count after hard delete");
    assert_eq!(count, 0, "row must be gone after hard delete on non-soft model");
}
