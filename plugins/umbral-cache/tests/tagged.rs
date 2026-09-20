use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use umbral_cache::{Cache, CacheBackend, Computed, MemoryBackend, StoreSpec};

#[tokio::test]
async fn get_or_compute_tagged_computes_once_then_hits_cache() {
    let cache = Cache::memory();
    let calls = AtomicUsize::new(0);
    let compute = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Computed {
            value: true,
            store: Some(StoreSpec {
                tags: vec!["t:1".into()],
                ttl: Some(Duration::from_secs(60)),
            }),
        }
    };
    let a: bool = cache.get_or_compute_tagged("k", compute).await;
    let b: bool = cache.get_or_compute_tagged("k", compute).await;
    assert!(a && b);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "second call must hit cache"
    );

    cache.bust_tag("t:1").await;
    let _c: bool = cache.get_or_compute_tagged("k", compute).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "bust_tag must force recompute"
    );
}

#[tokio::test]
async fn no_store_computed_is_never_cached() {
    let cache = Cache::memory();
    let calls = AtomicUsize::new(0);
    let compute = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Computed {
            value: false,
            store: None,
        }
    };
    let _: bool = cache.get_or_compute_tagged("k2", compute).await;
    let _: bool = cache.get_or_compute_tagged("k2", compute).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "store: None must never cache"
    );
}

#[tokio::test]
async fn retag_drops_key_from_its_old_tag_set() {
    let backend = MemoryBackend::default();
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
async fn delete_prunes_the_key_from_the_tag_index() {
    let backend = MemoryBackend::default();
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
async fn clear_drops_the_whole_tag_index() {
    let backend = MemoryBackend::default();
    backend
        .set_tagged("k", b"v".to_vec(), None, &["t:a".to_string()])
        .await;
    backend.clear().await;

    // Reinsert "k" untagged (post-clear). The stale tags map must not
    // remember it as belonging to "t:a" from before the clear.
    backend.set("k", b"v2".to_vec(), None).await;
    backend.bust_tag("t:a").await;
    assert!(
        backend.get("k").await.is_some(),
        "clear() must drop the tag index too, not just the values"
    );
}
