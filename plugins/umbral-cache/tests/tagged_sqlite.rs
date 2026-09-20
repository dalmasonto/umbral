use std::time::Duration;
use umbral_cache::{Cache, Computed, StoreSpec};

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
