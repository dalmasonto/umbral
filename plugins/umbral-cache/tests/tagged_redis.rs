#![cfg(feature = "redis")]
use std::time::Duration;
use umbral_cache::{Cache, CacheBackend, Computed, RedisBackend, StoreSpec};

fn redis_url() -> Option<String> {
    std::env::var("UMBRAL_TEST_REDIS_URL").ok()
}

#[tokio::test]
async fn redis_bust_tag_removes_indexed_keys() {
    let Some(url) = redis_url() else {
        eprintln!("skip: no UMBRAL_TEST_REDIS_URL");
        return;
    };
    let cache = Cache::redis(&url).await.unwrap();
    let compute = || async {
        Computed {
            value: true,
            store: Some(StoreSpec {
                tags: vec!["chan:redis".into()],
                ttl: Some(Duration::from_secs(60)),
            }),
        }
    };
    let _: bool = cache.get_or_compute_tagged("mediaacc:rf:u1", compute).await;
    assert_eq!(cache.get::<bool>("mediaacc:rf:u1").await, Some(true));
    cache.bust_tag("chan:redis").await;
    assert_eq!(cache.get::<bool>("mediaacc:rf:u1").await, None);
}

// gaps6 #11 regression tests — mirror tagged_sqlite.rs's retag/delete/clear
// prune cases against the reverse-index (`ukeytags:<key>`) fix.

#[tokio::test]
async fn redis_retag_drops_key_from_its_old_tag_set() {
    let Some(url) = redis_url() else {
        eprintln!("skip: no UMBRAL_TEST_REDIS_URL");
        return;
    };
    let backend = RedisBackend::connect_with_prefix(&url, "umbral:cachetest:gaps6-11a:")
        .await
        .unwrap();
    backend.clear().await;

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
async fn redis_delete_prunes_the_key_from_the_tag_index() {
    let Some(url) = redis_url() else {
        eprintln!("skip: no UMBRAL_TEST_REDIS_URL");
        return;
    };
    let backend = RedisBackend::connect_with_prefix(&url, "umbral:cachetest:gaps6-11b:")
        .await
        .unwrap();
    backend.clear().await;

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
async fn redis_clear_drops_the_whole_tag_index() {
    let Some(url) = redis_url() else {
        eprintln!("skip: no UMBRAL_TEST_REDIS_URL");
        return;
    };
    let backend = RedisBackend::connect_with_prefix(&url, "umbral:cachetest:gaps6-11c:")
        .await
        .unwrap();
    backend.clear().await;

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

#[tokio::test]
async fn redis_bust_tag_prunes_key_from_other_tag_sets() {
    let Some(url) = redis_url() else {
        eprintln!("skip: no UMBRAL_TEST_REDIS_URL");
        return;
    };
    let backend = RedisBackend::connect_with_prefix(&url, "umbral:cachetest:gaps6-11d:")
        .await
        .unwrap();
    backend.clear().await;

    // "k" tagged under both "t:a" and "t:b".
    backend
        .set_tagged(
            "k",
            b"v".to_vec(),
            None,
            &["t:a".to_string(), "t:b".to_string()],
        )
        .await;
    backend.bust_tag("t:a").await;
    assert!(backend.get("k").await.is_none(), "bust_tag(t:a) evicts k");

    // "k" is reused, tagged only "t:c" this time. If bust_tag(t:a) left a
    // dangling "t:b" -> {"k"} entry, a later bust_tag("t:b") would wrongly
    // evict this fresh, differently-tagged value.
    backend
        .set_tagged("k", b"v2".to_vec(), None, &["t:c".to_string()])
        .await;
    backend.bust_tag("t:b").await;
    assert!(
        backend.get("k").await.is_some(),
        "bust_tag must prune the evicted key out of its OTHER tag sets too"
    );
}
