#![cfg(feature = "redis")]
use std::time::Duration;
use umbral_cache::{Cache, Computed, StoreSpec};

#[tokio::test]
async fn redis_bust_tag_removes_indexed_keys() {
    let Ok(url) = std::env::var("UMBRAL_TEST_REDIS_URL") else {
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
