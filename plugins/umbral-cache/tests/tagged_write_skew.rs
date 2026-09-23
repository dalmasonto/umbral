//! gaps6 #12 — the media-access cache miss-compute/bust write-skew race.
//!
//! `media_access_cached` reads the cache, and on a miss runs the developer's
//! closure (the DB work) and only THEN stores `key → allow`. If a revocation's
//! `bust_tag` lands in that window — between the miss-read and the store — the
//! just-computed, now-stale `allow` was written AFTER the bust and survived
//! until the next TTL expiry. Bounded (≤ one TTL) but real: a revoked grant
//! kept working for up to a minute.
//!
//! The fix is a guarded store: capture the cache's monotonic bust epoch BEFORE
//! running the closure, and at store time skip the write if any of the
//! decision's tags was busted after that epoch. These tests drive the exact
//! sequence `media_access_cached` performs, with a bust interleaved.

use std::time::Duration;

use umbral::cache::TaggedCache;
use umbral_cache::Cache;

/// The crux: a `bust_tag` that lands after the epoch capture but before the
/// guarded store must WIN — the stale `allow` must not be cached.
#[tokio::test]
async fn a_bust_between_epoch_capture_and_store_defeats_the_stale_allow() {
    let cache = Cache::memory();
    let key = "mediaacc:doc1:u1";
    let tag = "media:doc:1".to_string();

    // Exactly what `media_access_cached` does, in order:
    let epoch = cache.bust_epoch().await; // T0 — before the closure
    // ...closure runs the DB work, decides allow=true, tags=[tag]...
    cache.bust_tag(&tag).await; // a revocation lands mid-flight
    let stored = cache
        .set_tagged_bool_guarded(
            key,
            true,
            Some(Duration::from_secs(60)),
            std::slice::from_ref(&tag),
            epoch,
        )
        .await;

    assert!(
        !stored,
        "a decision whose tag was busted after the epoch must NOT be cached"
    );
    assert_eq!(
        cache.get_bool(key).await,
        None,
        "the stale allow must not survive the interleaved bust — else it lingers until TTL",
    );
}

/// No interleaving bust → the decision caches normally and is served, and a
/// LATER bust still evicts it (the guard doesn't break ordinary invalidation).
#[tokio::test]
async fn a_clean_compute_is_cached_and_still_bustable() {
    let cache = Cache::memory();
    let key = "mediaacc:doc2:u1";
    let tag = "media:doc:2".to_string();

    let epoch = cache.bust_epoch().await;
    let stored = cache
        .set_tagged_bool_guarded(
            key,
            true,
            Some(Duration::from_secs(60)),
            std::slice::from_ref(&tag),
            epoch,
        )
        .await;

    assert!(stored, "an uncontended compute must be cached");
    assert_eq!(cache.get_bool(key).await, Some(true));

    cache.bust_tag(&tag).await;
    assert_eq!(
        cache.get_bool(key).await,
        None,
        "a later bust of the decision's tag still evicts the cached allow",
    );
}

/// The guard is per-tag: a bust of some OTHER tag between the epoch capture and
/// the store must not force a needless recompute of an unrelated decision.
#[tokio::test]
async fn a_bust_of_an_unrelated_tag_does_not_block_the_store() {
    let cache = Cache::memory();
    let key = "mediaacc:doc3:u1";
    let tag = "media:doc:3".to_string();

    let epoch = cache.bust_epoch().await;
    cache.bust_tag("media:doc:999").await; // unrelated revocation
    let stored = cache
        .set_tagged_bool_guarded(
            key,
            true,
            Some(Duration::from_secs(60)),
            std::slice::from_ref(&tag),
            epoch,
        )
        .await;

    assert!(
        stored,
        "an unrelated tag's bust must not defeat the store — the guard is per-tag, not global",
    );
    assert_eq!(cache.get_bool(key).await, Some(true));
}
