use std::time::Duration;
use umbral_cache::{Cache, CacheBackend, Computed, SqliteBackend, StoreSpec};

async fn sqlite_backend() -> SqliteBackend {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    SqliteBackend::new(pool).await.unwrap()
}

#[tokio::test]
async fn sqlite_bust_tag_removes_indexed_keys() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let cache = Cache::sqlite(pool).await.unwrap();
    let compute = |v: bool| {
        move || async move {
            Computed {
                value: v,
                store: Some(StoreSpec {
                    tags: vec!["chan:9".into()],
                    ttl: Some(Duration::from_secs(60)),
                }),
            }
        }
    };
    let _: bool = cache
        .get_or_compute_tagged("mediaacc:f:u1", compute(true))
        .await;
    // hit:
    assert_eq!(cache.get::<bool>("mediaacc:f:u1").await, Some(true));
    cache.bust_tag("chan:9").await;
    assert_eq!(
        cache.get::<bool>("mediaacc:f:u1").await,
        None,
        "busted key gone"
    );
}

#[tokio::test]
async fn sqlite_retag_drops_key_from_its_old_tag_set() {
    let backend = sqlite_backend().await;
    backend
        .set_tagged("k", b"v1".to_vec(), None, &["t:a".to_string()])
        .await;
    // Re-tag the same key under a different tag, dropping "t:a".
    backend
        .set_tagged("k", b"v2".to_vec(), None, &["t:b".to_string()])
        .await;

    backend.bust_tag("t:a").await;
    assert!(
        backend.get("k").await.is_some(),
        "bust_tag on a stale (no-longer-current) tag must not evict a re-tagged key"
    );

    backend.bust_tag("t:b").await;
    assert!(
        backend.get("k").await.is_none(),
        "bust_tag on the key's current tag must still evict it"
    );
}

#[tokio::test]
async fn sqlite_delete_prunes_the_key_from_the_tag_index() {
    let backend = sqlite_backend().await;
    backend
        .set_tagged("k", b"v".to_vec(), None, &["t:a".to_string()])
        .await;
    backend.delete("k").await;

    // Reuse the same key name, untagged this time. If delete() left a
    // stale "t:a" -> {"k"} entry behind, this later bust_tag("t:a") would
    // wrongly evict a value that was never tagged "t:a" in this generation.
    backend.set("k", b"v2".to_vec(), None).await;
    backend.bust_tag("t:a").await;
    assert!(
        backend.get("k").await.is_some(),
        "delete() must prune the tag index so a reused key can't be evicted by a stale tag"
    );
}

#[tokio::test]
async fn sqlite_clear_drops_the_whole_tag_index() {
    let backend = sqlite_backend().await;
    backend
        .set_tagged("k", b"v".to_vec(), None, &["t:a".to_string()])
        .await;
    backend.clear().await;

    // Reinsert "k" untagged (post-clear). The stale tag index must not
    // remember it as belonging to "t:a" from before the clear.
    backend.set("k", b"v2".to_vec(), None).await;
    backend.bust_tag("t:a").await;
    assert!(
        backend.get("k").await.is_some(),
        "clear() must drop the tag index too, not just the values"
    );
}
