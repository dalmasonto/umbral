//! `CachedDbStore` — the in-process session cache, and the invalidation
//! contract it exists under.
//!
//! Proof strategy, stated because they look odd: "a cache hit issued zero
//! statements" is proven by **deleting the row underneath the cache with raw
//! SQL** (which, unlike the ORM, emits no signals) and asserting the next
//! `load` still answers from memory — a DB read would have found nothing.
//! Signal-driven invalidation is proven the other way: an **ORM-mediated**
//! write (which does emit) must be visible on the very next `load`.
//!
//! Own binary (separate process → separate ambient `OnceLock` + signal
//! registry), per this crate's test convention.

use chrono::{Duration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::sync::Arc;
use tokio::sync::OnceCell;

use umbral_sessions::store::{
    CachedDbStore, SessionCacheConfig, SessionRecord, SessionStore, active_store, hash_token_pub,
    install_store,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

fn record(user: &str, data: &str) -> SessionRecord {
    let now = Utc::now();
    SessionRecord {
        user_id: Some(user.to_string()),
        data: data.to_string(),
        created_at: now,
        expires_at: now + Duration::hours(1),
    }
}

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("cached_store.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<umbral_sessions::Session>()
            .build()
            .expect("App::build");
        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // The installed store is what the signal subscription invalidates
        // through (`active_store().invalidate`), so the signal test must use
        // it rather than a locally-constructed store.
        install_store(Arc::new(CachedDbStore::default()));
    })
    .await;
}

/// Delete a session row with RAW SQL — no ORM, therefore no signals. The
/// only way a subsequent `load` can still answer is from the cache.
async fn nuke_row_raw(token: &str) {
    let pool = umbral::db::pool_dispatched();
    let umbral::db::DbPool::Sqlite(pool) = pool else {
        panic!("this suite runs on SQLite");
    };
    sqlx::query("DELETE FROM session WHERE id = ?")
        .bind(hash_token_pub(token))
        .execute(pool)
        .await
        .expect("raw delete");
}

#[tokio::test]
async fn a_cache_hit_serves_without_touching_the_database() {
    boot().await;
    let store = CachedDbStore::default();
    store
        .save("tok-hit", &record("7", r#"{"n":1}"#))
        .await
        .expect("save");
    // Warm the cache, then remove the row behind its back (raw SQL = no
    // signals, so nothing tells the store).
    let first = store.load("tok-hit").await.expect("load").expect("present");
    assert_eq!(first.user_id.as_deref(), Some("7"));
    nuke_row_raw("tok-hit").await;

    let hit = store.load("tok-hit").await.expect("load");
    assert!(
        hit.is_some(),
        "the row is gone from the DB; a Some here is only possible from the cache"
    );

    // Control: a store with no memory of this token reads the DB and finds
    // nothing — same database, same moment.
    let cold = CachedDbStore::default();
    assert!(
        cold.load("tok-hit").await.expect("load").is_none(),
        "control: the row really is gone"
    );
}

#[tokio::test]
async fn an_orm_mediated_write_invalidates_through_the_signals() {
    boot().await;
    let store = active_store();
    store
        .save("tok-signal", &record("7", r#"{"n":1}"#))
        .await
        .expect("save");
    let warm = store
        .load("tok-signal")
        .await
        .expect("load")
        .expect("present");
    assert_eq!(warm.data, r#"{"n":1}"#);

    // A DIFFERENT writer — the ORM directly, as an admin/rotation path
    // would. Its `post_save:session` signal must reach the installed store
    // and evict the key, so the change is visible on the very next load.
    // (`upsert` gained its signal emits in the same change as this cache —
    // before that, this exact write was invisible to every subscriber.)
    let now = Utc::now();
    umbral_sessions::Session::objects()
        .upsert(umbral_sessions::Session {
            id: hash_token_pub("tok-signal"),
            user_id: Some("7".to_string()),
            data: r#"{"n":2}"#.to_string(),
            created_at: now,
            expires_at: now + Duration::hours(1),
        })
        .await
        .expect("orm upsert");

    let after = store
        .load("tok-signal")
        .await
        .expect("load")
        .expect("present");
    assert_eq!(
        after.data, r#"{"n":2}"#,
        "the ORM write must invalidate the cached key via post_save:session"
    );
}

#[tokio::test]
async fn destroy_evicts_so_the_next_load_is_none() {
    boot().await;
    let store = CachedDbStore::default();
    store
        .save("tok-destroy", &record("7", "{}"))
        .await
        .expect("save");
    assert!(store.load("tok-destroy").await.expect("load").is_some());

    store.destroy("tok-destroy").await.expect("destroy");
    assert!(
        store.load("tok-destroy").await.expect("load").is_none(),
        "a destroyed session must not be served from the cache"
    );
}

#[tokio::test]
async fn destroy_user_evicts_that_users_entries_only() {
    boot().await;
    let store = CachedDbStore::default();
    store
        .save("tok-a", &record("11", "{}"))
        .await
        .expect("save a");
    store
        .save("tok-b", &record("11", "{}"))
        .await
        .expect("save b");
    store
        .save("tok-c", &record("22", "{}"))
        .await
        .expect("save c");
    // Warm all three.
    for t in ["tok-a", "tok-b", "tok-c"] {
        assert!(store.load(t).await.expect("load").is_some());
    }

    let removed = store.destroy_user("11").await.expect("destroy_user");
    assert_eq!(removed, 2);
    assert!(store.load("tok-a").await.expect("load").is_none());
    assert!(store.load("tok-b").await.expect("load").is_none());
    assert!(
        store.load("tok-c").await.expect("load").is_some(),
        "another user's session must survive a destroy_user"
    );
}

#[tokio::test]
async fn ttl_bounds_staleness_for_writes_signals_cannot_see() {
    boot().await;
    let store = CachedDbStore::new(SessionCacheConfig {
        ttl: std::time::Duration::from_millis(120),
        ..Default::default()
    });
    store
        .save("tok-ttl", &record("7", "{}"))
        .await
        .expect("save");
    assert!(store.load("tok-ttl").await.expect("load").is_some());
    nuke_row_raw("tok-ttl").await;

    assert!(
        store.load("tok-ttl").await.expect("load").is_some(),
        "inside the TTL the cache still answers"
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        store.load("tok-ttl").await.expect("load").is_none(),
        "past the TTL the stale entry must be dropped and the DB consulted"
    );
}
