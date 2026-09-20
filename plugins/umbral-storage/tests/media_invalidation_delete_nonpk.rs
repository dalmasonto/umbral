//! gaps6 #14: `media_invalidate_on::<M>` tags keyed on a NON-PK field must
//! still bust on DELETE. Before the fix, `QuerySet::delete()`'s per-row
//! `post_delete:<table>` payload carried only `{"<pk>": <id>}` under
//! `instance` — any `map_fn` that reads a non-PK field (like `channel`
//! here) silently failed to deserialize `M` and returned no tags, so the
//! cached decision never busted. After the fix, a subscribed `post_delete`
//! gets the FULL row, so `map_fn` sees `channel` and busts the right tag.
//!
//! Own test binary — see `media_invalidation_delete.rs`'s header for why
//! (each test boots an `App` with process-wide `OnceLock`s).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use http::HeaderMap;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral_storage::{Decision, StoragePlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "membership")]
pub struct Membership {
    pub id: i64,
    // Deliberately NOT `#[serde(default)]` and NOT `Option` — a PK-only
    // post_delete payload (`{"id": <n>}`) fails to deserialize into this
    // struct, which is exactly the bug this test proves is fixed.
    pub channel: String,
}

#[tokio::test]
async fn deleting_the_source_row_busts_a_non_pk_keyed_tag() {
    umbral::cache::set_ambient_tagged_cache(Arc::new(umbral_cache::Cache::memory()));

    let settings = umbral::Settings::from_env().expect("figment defaults");
    let dbtmp = tempfile::tempdir().expect("tempdir");
    let db_path = dbtmp.path().join("media_invalidation_delete_nonpk.sqlite");
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

    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_cached(move |_caller, _key: &str| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Decision::of(true).depends_on(vec!["chan:9".to_string()])
            }
        })
        .media_invalidate_on::<Membership, _>(|m| vec![format!("chan:{}", m.channel)]);

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Membership>()
        .plugin(plugin.clone())
        .build()
        .expect("App::build");

    umbral::migrate::create_tables_for_tests()
        .await
        .expect("create schema");

    let row = Membership::objects()
        .create(Membership {
            id: 0,
            channel: "9".into(),
        })
        .await
        .expect("create membership");

    let access = plugin.resolve_access().expect("access fn");
    let h = HeaderMap::new();

    let _ = access(&h, "f").await; // miss → caches under "chan:9"
    assert_eq!(calls.load(Ordering::SeqCst), 1, "first access recomputes");

    let _ = access(&h, "f").await; // hit
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "second identical access hits cache"
    );

    // DELETE the source row THROUGH THE ORM. The tag "chan:9" is derived
    // from the non-PK `channel` field, so busting it requires the FULL row
    // in the post_delete payload, not just the PK.
    Membership::objects()
        .filter(membership::ID.eq(row.id))
        .delete()
        .await
        .expect("delete membership");

    let _ = access(&h, "f").await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the ORM delete fired post_delete with the FULL row, busting the non-PK tag"
    );
}
