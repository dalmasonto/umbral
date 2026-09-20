//! `media_invalidate_on::<M>` on the DELETE path: a row deleted THROUGH THE
//! ORM (`Model::objects().filter(...).delete()`) fires the per-row
//! `post_delete:<table>` signal, which must bust the same tags a `post_save`
//! would — keeping a cached `media_access_cached` decision fresh without
//! waiting out the TTL.
//!
//! The per-row `post_delete` payload from a `QuerySet::delete()` carries
//! only the deleted row's primary key (`{"instance": {"<pk>": <id>}}`), not
//! the full row — the row is already gone by the time the signal fires, so
//! there's nothing else to serialize (see `delete()` in
//! `crates/umbral-core/src/orm/queryset/mod.rs`, gaps3 #29). This test's
//! model therefore has a single `id` field and its `media_invalidate_on`
//! map_fn keys the tag off `r.id` alone, so it deserializes cleanly from
//! that PK-only payload.
//!
//! Lives in its own test binary (not alongside
//! `media_invalidation.rs::source_row_change_busts_the_cached_decision`)
//! because both tests boot an `App` that sets process-wide `OnceLock`s (the
//! DB pool via `umbral::App::builder().database(...)`, the ambient tagged
//! cache via `umbral::cache::set_ambient_tagged_cache`) that can only be set
//! once per process; `cargo test` runs tests within one file's binary
//! concurrently on shared state, so a second `App::build()` in the same
//! binary would collide with the first.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use http::HeaderMap;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral_storage::{Decision, StoragePlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "roster_entry")]
pub struct RosterEntry {
    pub id: i64,
    // Present so the model has a non-PK column to INSERT; `#[serde(default)]`
    // means it still deserializes fine from the PK-only `post_delete`
    // payload (`{"id": <n>}`), which carries no `note` key at all.
    #[serde(default)]
    pub note: String,
}

#[tokio::test]
async fn deleting_the_source_row_busts_the_cached_decision() {
    umbral::cache::set_ambient_tagged_cache(Arc::new(umbral_cache::Cache::memory()));

    let settings = umbral::Settings::from_env().expect("figment defaults");
    let dbtmp = tempfile::tempdir().expect("tempdir");
    let db_path = dbtmp.path().join("media_invalidation_delete.sqlite");
    std::mem::forget(dbtmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(5))
                .filename(&db_path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    // The row is created AFTER `App::build()` (the plugin closure has to be
    // wired before build), so its autoassigned id isn't known yet when the
    // closure is written. Share it through an atomic instead of capturing a
    // fixed tag string.
    let seeded_id = Arc::new(AtomicI64::new(0));

    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let tag_id = seeded_id.clone();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_cached(move |_caller, _key: &str| {
            let c = c.clone();
            let tag = format!("entry:{}", tag_id.load(Ordering::SeqCst));
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Decision::of(true).depends_on(vec![tag])
            }
        })
        .media_invalidate_on::<RosterEntry, _>(|r| vec![format!("entry:{}", r.id)]);

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<RosterEntry>()
        .plugin(plugin.clone())
        .build()
        .expect("App::build");

    umbral::migrate::create_tables_for_tests()
        .await
        .expect("create schema");

    // Seed the source row through the ORM, then publish its id for the
    // closure above to read.
    let row = RosterEntry::objects()
        .create(RosterEntry {
            id: 0,
            note: "seed".into(),
        })
        .await
        .expect("create roster entry");
    seeded_id.store(row.id, Ordering::SeqCst);

    let access = plugin.resolve_access().expect("access fn");
    let h = HeaderMap::new();

    let _ = access(&h, "f").await; // miss → closure runs, caches under "entry:<id>"
    assert_eq!(calls.load(Ordering::SeqCst), 1, "first access recomputes");

    let _ = access(&h, "f").await; // hit → closure does NOT run
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "second identical access hits cache"
    );

    // DELETE the source row THROUGH THE ORM — fires post_delete:roster_entry
    // (PK-only payload), which the subscriber maps to the same tag and busts.
    RosterEntry::objects()
        .filter(roster_entry::ID.eq(row.id))
        .delete()
        .await
        .expect("delete roster entry");

    let _ = access(&h, "f").await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the ORM delete fired post_delete, busting the tag — access recomputes"
    );
}
