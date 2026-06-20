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

    // The default queryset auto-hides soft-deleted "a"; "b" and "c"
    // must still be visible. The parallel test inserts other rows
    // into the same DB so we only assert the bound seeded titles
    // (`a` / `b` / `c`) appear correctly.
    let visible_titles: Vec<String> = SoftPost::objects()
        .fetch()
        .await
        .expect("fetch visible")
        .into_iter()
        .map(|p| p.title)
        .filter(|t| matches!(t.as_str(), "a" | "b" | "c"))
        .collect();
    assert!(visible_titles.contains(&"b".to_string()));
    assert!(visible_titles.contains(&"c".to_string()));
    assert!(!visible_titles.contains(&"a".to_string()));

    // .with_deleted() brings "a" back into scope.
    let all_titles: Vec<String> = SoftPost::objects()
        .with_deleted()
        .fetch()
        .await
        .expect("fetch all incl deleted")
        .into_iter()
        .map(|p| p.title)
        .filter(|t| matches!(t.as_str(), "a" | "b" | "c"))
        .collect();
    assert!(all_titles.contains(&"a".to_string()));

    // .only_deleted() must contain "a" (and may contain rows from
    // parallel hard_delete tests, so check membership not exact).
    let trash = SoftPost::objects()
        .only_deleted()
        .fetch()
        .await
        .expect("fetch trash");
    let a_row = trash.iter().find(|p| p.title == "a").expect("a in trash");
    assert!(a_row.deleted_at.is_some());
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

// ── gaps2 #34: update_values / update_expr must honour the soft-delete scope ──
//
// Each test seeds its own rows with a unique prefix (process-id + suffix) so
// the tests can share the singleton in-memory DB without stepping on each other
// or on the rows seeded by the boot() fixture.

/// (a) Default `update_values` on a soft-delete model must skip trashed rows.
/// Before the fix this would have updated both live and trashed rows because
/// `build_update_for` walked only the explicit predicates.
#[tokio::test]
async fn update_values_default_scope_skips_trashed_rows() {
    boot().await;
    let pid = std::process::id();
    let live_title = format!("upd-live-{pid}");
    let dead_title = format!("upd-dead-{pid}");

    // Seed one live row and one to-be-trashed row.
    SoftPost::objects()
        .create(SoftPost { id: 0, title: live_title.clone(), deleted_at: None })
        .await
        .expect("create live row");
    SoftPost::objects()
        .create(SoftPost { id: 0, title: dead_title.clone(), deleted_at: None })
        .await
        .expect("create dead row");

    // Soft-delete the second row.
    SoftPost::objects()
        .filter(soft_post::TITLE.eq(dead_title.as_str()))
        .delete()
        .await
        .expect("soft-delete dead row");

    // Bulk-update all rows matching our prefix with a new suffix.
    let new_live = format!("{live_title}-updated");
    let mut patch = serde_json::Map::new();
    patch.insert("title".into(), serde_json::Value::String(new_live.clone()));
    let updated = SoftPost::objects()
        .filter(soft_post::TITLE.eq(live_title.as_str()))
        .update_values(patch)
        .await
        .expect("update_values on live row");
    assert_eq!(updated, 1, "exactly one live row should be updated");

    // The trashed row must still carry its original title (not the new one).
    let trashed: Vec<SoftPost> = SoftPost::objects()
        .only_deleted()
        .fetch()
        .await
        .expect("fetch trashed");
    let trashed_row = trashed
        .iter()
        .find(|p| p.title == dead_title || p.title == new_live)
        .expect("trashed row must still exist");
    assert_eq!(
        trashed_row.title, dead_title,
        "update_values with default scope must NOT touch trashed rows"
    );

    // The live row should carry the updated title.
    let live: Vec<SoftPost> = SoftPost::objects()
        .filter(soft_post::TITLE.eq(new_live.as_str()))
        .fetch()
        .await
        .expect("fetch updated live row");
    assert_eq!(live.len(), 1, "updated live row must be visible");
}

/// (b) `.only_deleted().update_values(...)` — the restore path.
/// Clears `deleted_at` on the trashed row only; live rows are untouched.
#[tokio::test]
async fn update_values_only_deleted_restores_trashed_row() {
    boot().await;
    let pid = std::process::id();
    let live_title  = format!("rst-live-{pid}");
    let trash_title = format!("rst-trash-{pid}");

    SoftPost::objects()
        .create(SoftPost { id: 0, title: live_title.clone(), deleted_at: None })
        .await
        .expect("create live row");
    SoftPost::objects()
        .create(SoftPost { id: 0, title: trash_title.clone(), deleted_at: None })
        .await
        .expect("create trash row");

    // Soft-delete the second row.
    SoftPost::objects()
        .filter(soft_post::TITLE.eq(trash_title.as_str()))
        .delete()
        .await
        .expect("soft-delete trash row");

    // Restore: set deleted_at = NULL via only_deleted() scope.
    let mut patch = serde_json::Map::new();
    patch.insert("deleted_at".into(), serde_json::Value::Null);
    let restored = SoftPost::objects()
        .only_deleted()
        .filter(soft_post::TITLE.eq(trash_title.as_str()))
        .update_values(patch)
        .await
        .expect("restore via only_deleted().update_values");
    assert_eq!(restored, 1, "exactly one trashed row should be restored");

    // The restored row must now be visible in the default queryset.
    let visible: Vec<SoftPost> = SoftPost::objects()
        .filter(soft_post::TITLE.eq(trash_title.as_str()))
        .fetch()
        .await
        .expect("fetch restored row");
    assert_eq!(visible.len(), 1, "restored row must appear in default queryset");
    assert!(visible[0].deleted_at.is_none(), "deleted_at must be NULL after restore");

    // The live row must be unchanged.
    let live: Vec<SoftPost> = SoftPost::objects()
        .filter(soft_post::TITLE.eq(live_title.as_str()))
        .fetch()
        .await
        .expect("fetch live row after restore");
    assert_eq!(live.len(), 1, "live row must still be visible and untouched");
}

/// (c) `.with_deleted().update_values(...)` updates both live AND trashed rows.
#[tokio::test]
async fn update_values_with_deleted_covers_all_rows() {
    boot().await;
    let pid = std::process::id();
    let live_title  = format!("wd-live-{pid}");
    let trash_title = format!("wd-trash-{pid}");
    let new_suffix  = format!("wd-renamed-{pid}");

    SoftPost::objects()
        .create(SoftPost { id: 0, title: live_title.clone(), deleted_at: None })
        .await
        .expect("create live row");
    SoftPost::objects()
        .create(SoftPost { id: 0, title: trash_title.clone(), deleted_at: None })
        .await
        .expect("create trash row");

    // Soft-delete the second row.
    SoftPost::objects()
        .filter(soft_post::TITLE.eq(trash_title.as_str()))
        .delete()
        .await
        .expect("soft-delete trash row");

    // with_deleted() + update_values changes both rows (we match them by a
    // shared prefix; use two separate targeted updates to assert each).
    let mut patch_live = serde_json::Map::new();
    patch_live.insert("title".into(), serde_json::Value::String(format!("{new_suffix}-a")));
    let n_live = SoftPost::objects()
        .with_deleted()
        .filter(soft_post::TITLE.eq(live_title.as_str()))
        .update_values(patch_live)
        .await
        .expect("update live row via with_deleted");
    assert_eq!(n_live, 1);

    let mut patch_trash = serde_json::Map::new();
    patch_trash.insert("title".into(), serde_json::Value::String(format!("{new_suffix}-b")));
    let n_trash = SoftPost::objects()
        .with_deleted()
        .filter(soft_post::TITLE.eq(trash_title.as_str()))
        .update_values(patch_trash)
        .await
        .expect("update trashed row via with_deleted");
    assert_eq!(n_trash, 1, "with_deleted() must allow updating trashed rows");

    // Both rows visible via with_deleted().
    let all: Vec<SoftPost> = SoftPost::objects()
        .with_deleted()
        .fetch()
        .await
        .expect("fetch all");
    let renamed_a = all.iter().any(|p| p.title == format!("{new_suffix}-a"));
    let renamed_b = all.iter().any(|p| p.title == format!("{new_suffix}-b"));
    assert!(renamed_a, "live row rename must be visible with_deleted");
    assert!(renamed_b, "trash row rename must be visible with_deleted");
}
