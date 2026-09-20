use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use umbral_cache::{Cache, Computed, StoreSpec};

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
