//! `media_invalidate_on::<M>`: wires `post_save:<M::TABLE>` /
//! `post_delete:<M::TABLE>` signals so a source-row change busts the tags a
//! `media_access_cached` decision depended on — keeping cached decisions
//! fresh below the TTL without waiting it out.

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
    pub chan: i64,
}

#[tokio::test]
async fn source_row_change_busts_the_cached_decision() {
    umbral::cache::set_ambient_tagged_cache(Arc::new(umbral_cache::Cache::memory()));

    let settings = umbral::Settings::from_env().expect("figment defaults");
    let dbtmp = tempfile::tempdir().expect("tempdir");
    let db_path = dbtmp.path().join("media_invalidation.sqlite");
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
        .media_invalidate_on::<Membership, _>(|m| vec![format!("chan:{}", m.chan)]);

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

    let access = plugin.resolve_access().expect("access fn");
    let h = HeaderMap::new();

    let _ = access(&h, "f").await; // miss → closure runs
    assert_eq!(calls.load(Ordering::SeqCst), 1, "first access recomputes");

    let _ = access(&h, "f").await; // hit → closure does NOT run
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "second identical access hits cache"
    );

    // Create a source row THROUGH THE ORM — fires post_save:membership,
    // which the subscriber maps to tag "chan:9" and busts.
    Membership::objects()
        .create(Membership { id: 0, chan: 9 })
        .await
        .expect("create membership row");

    let _ = access(&h, "f").await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the ORM create fired post_save, busting chan:9 — access recomputes"
    );
}
