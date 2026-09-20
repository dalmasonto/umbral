//! Task C: the ambient `umbral::cache::TaggedCache` contract, implemented
//! and registered by umbral-cache. This is its own test binary because
//! `set_ambient_tagged_cache` is a process-wide OnceLock (set-once).

use std::time::Duration;
use umbral::cache::{ambient_tagged_cache, set_ambient_tagged_cache};
use umbral_cache::Cache;

#[tokio::test]
async fn ambient_tagged_cache_round_trips_and_busts() {
    set_ambient_tagged_cache(std::sync::Arc::new(Cache::memory()));
    let c = ambient_tagged_cache().expect("ambient tagged cache registered");
    c.set_tagged_bool(
        "mediaacc:f:u1",
        true,
        Some(Duration::from_secs(60)),
        &["chan:1".into()],
    )
    .await;
    assert_eq!(c.get_bool("mediaacc:f:u1").await, Some(true));
    c.bust_tag("chan:1").await;
    assert_eq!(c.get_bool("mediaacc:f:u1").await, None);
}
