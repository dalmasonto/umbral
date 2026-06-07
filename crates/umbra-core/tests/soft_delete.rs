//! Feature #72 — soft-delete regression tests.
//!
//! A model tagged `#[umbra(soft_delete)]` must:
//!   - declare its own `deleted_at: Option<DateTime<Utc>>` column,
//!   - get `WHERE deleted_at IS NULL` auto-injected on every
//!     QuerySet terminal,
//!   - rewrite `delete()` and `delete_instance()` as
//!     `UPDATE ... SET deleted_at = NOW()` instead of `DELETE FROM`,
//!   - opt back into soft-deleted rows via `.with_deleted()` /
//!     `.only_deleted()`, and
//!   - allow a hard purge via `.hard_delete()`.
//!
//! A peer model WITHOUT the marker must keep the pre-feature
//! behaviour (no auto-filter, hard DELETE).

#![allow(dead_code, private_interfaces)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(soft_delete, table = "sd_post")]
pub struct SoftPost {
    pub id: i64,
    pub title: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "hard_post")]
pub struct HardPost {
    pub id: i64,
    pub title: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults always load");
        let pool = umbra::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<SoftPost>()
            .model::<HardPost>()
            .build()
            .expect("App::build should succeed");
        sqlx::query("CREATE TABLE sd_post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, deleted_at TEXT)")
            .execute(&pool)
            .await
            .expect("create sd_post");
        sqlx::query("CREATE TABLE hard_post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("create hard_post");
        for (table, label) in &[("sd_post", "a"), ("sd_post", "b"), ("sd_post", "c"), ("hard_post", "x")] {
            sqlx::query(&format!("INSERT INTO {table} (title) VALUES (?)"))
                .bind(*label)
                .execute(&pool)
                .await
                .expect("seed");
        }
    })
    .await;
}

#[tokio::test]
async fn soft_delete_const_is_set_from_macro_attr() {
    boot().await;
    assert!(<SoftPost as umbra::orm::Model>::SOFT_DELETE);
    assert!(!<HardPost as umbra::orm::Model>::SOFT_DELETE);
}

#[tokio::test]
async fn delete_rewrites_to_update_for_soft_models() {
    boot().await;
    // Soft-delete the row labelled "a".
    let affected = SoftPost::objects()
        .filter(soft_post::TITLE.eq("a"))
        .delete()
        .await
        .expect("soft delete");
    assert_eq!(affected, 1);

    // The default queryset auto-hides soft-deleted rows.
    let visible = SoftPost::objects().fetch().await.expect("fetch visible");
    let titles: Vec<&str> = visible.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, vec!["b", "c"]);

    // .with_deleted() shows them again.
    let all = SoftPost::objects()
        .with_deleted()
        .fetch()
        .await
        .expect("fetch all incl deleted");
    assert_eq!(all.len(), 3);

    // .only_deleted() returns just the dropped row.
    let trash = SoftPost::objects()
        .only_deleted()
        .fetch()
        .await
        .expect("fetch trash");
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].title, "a");
    assert!(trash[0].deleted_at.is_some());
}

#[tokio::test]
async fn hard_delete_bypasses_soft_path_on_opt_in() {
    boot().await;
    // Soft-delete first so we have a known soft-deleted row to
    // hard-purge — exercises the .hard_delete() path against a row
    // the default queryset can't see, which is the realistic admin
    // "empty the trash" flow.
    let title = format!("purge-me-{}", std::process::id());
    SoftPost::objects()
        .create(SoftPost {
            id: 0,
            title: title.clone(),
            deleted_at: None,
        })
        .await
        .expect("seed purge row");
    SoftPost::objects()
        .filter(soft_post::TITLE.eq(title.as_str()))
        .delete()
        .await
        .expect("soft-delete purge row");
    let affected = SoftPost::objects()
        .filter(soft_post::TITLE.eq(title.as_str()))
        .with_deleted()
        .hard_delete()
        .delete()
        .await
        .expect("hard delete via with_deleted + hard_delete");
    assert_eq!(affected, 1);

    // Even .with_deleted() can't find it — the row is truly gone.
    let any = SoftPost::objects()
        .filter(soft_post::TITLE.eq(title.as_str()))
        .with_deleted()
        .fetch()
        .await
        .expect("post-purge fetch");
    assert!(any.is_empty());
}

#[tokio::test]
async fn hard_model_delete_is_unchanged() {
    boot().await;
    let affected = HardPost::objects()
        .filter(hard_post::TITLE.eq("x"))
        .delete()
        .await
        .expect("hard delete on non-soft model");
    assert_eq!(affected, 1);
    let remaining = HardPost::objects()
        .fetch()
        .await
        .expect("fetch after hard delete");
    assert!(remaining.is_empty());
}
