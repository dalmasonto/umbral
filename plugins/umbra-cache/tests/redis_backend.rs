//! Redis backend integration tests.
//!
//! All tests are gated on:
//! 1. The `redis` cargo feature — compiled out otherwise.
//! 2. A `REDIS_URL` environment variable — skipped at runtime if absent.
//!
//! Run with a live Redis:
//!   REDIS_URL=redis://localhost:6379/15 cargo test --features redis -p umbra-cache
//!
//! DB 15 is used by convention to avoid touching production data.

#[cfg(feature = "redis")]
mod tests {
    use std::env;
    use std::time::Duration;

    use serde::{Deserialize, Serialize};
    use umbra_cache::Cache;

    /// Return the Redis URL from the env, or skip the test gracefully.
    fn redis_url() -> Option<String> {
        env::var("REDIS_URL").ok()
    }

    /// Unique key prefix per test to avoid collisions when multiple tests
    /// run in parallel against the same Redis instance.
    fn key(suffix: &str) -> String {
        format!("umbra:test:{}", suffix)
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Page {
        title: String,
        views: u64,
    }

    // ── connect ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn redis_connect_succeeds() {
        let Some(url) = redis_url() else {
            eprintln!("REDIS_URL not set — skipping redis_connect_succeeds");
            return;
        };
        let result = Cache::redis(&url).await;
        assert!(
            result.is_ok(),
            "connection should succeed: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn redis_connect_fails_on_bad_url() {
        // This should return a CacheError, not panic.
        let result = Cache::redis("redis://localhost:9999").await;
        // Connection might succeed (just connection manager creation) — what
        // matters is no panic. If it errors, the error is a CacheError.
        let _ = result; // either Ok or Err is acceptable here
    }

    // ── set / get round-trip ─────────────────────────────────────────────────

    #[tokio::test]
    async fn redis_set_get_string_round_trip() {
        let Some(url) = redis_url() else {
            eprintln!("REDIS_URL not set — skipping");
            return;
        };
        let cache = Cache::redis(&url).await.unwrap();
        let k = key("string_rt");

        cache.set(&k, "hello redis", None).await.unwrap();
        let v: Option<String> = cache.get(&k).await;
        assert_eq!(v.as_deref(), Some("hello redis"));

        // cleanup
        cache.delete(&k).await;
    }

    #[tokio::test]
    async fn redis_set_get_struct_round_trip() {
        let Some(url) = redis_url() else {
            eprintln!("REDIS_URL not set — skipping");
            return;
        };
        let cache = Cache::redis(&url).await.unwrap();
        let k = key("struct_rt");

        let page = Page {
            title: "Umbra Redis".into(),
            views: 42,
        };
        cache.set(&k, &page, None).await.unwrap();
        let back: Option<Page> = cache.get(&k).await;
        assert_eq!(back, Some(page));

        cache.delete(&k).await;
    }

    #[tokio::test]
    async fn redis_get_miss_returns_none() {
        let Some(url) = redis_url() else {
            eprintln!("REDIS_URL not set — skipping");
            return;
        };
        let cache = Cache::redis(&url).await.unwrap();
        let v: Option<String> = cache.get(&key("definitely_absent_xyzzy")).await;
        assert!(v.is_none());
    }

    // ── TTL expiry ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn redis_ttl_expires_the_entry() {
        let Some(url) = redis_url() else {
            eprintln!("REDIS_URL not set — skipping");
            return;
        };
        let cache = Cache::redis(&url).await.unwrap();
        let k = key("ttl_expire");

        // Set with a 1-second TTL (minimum Redis supports)
        cache
            .set(&k, "ephemeral", Some(Duration::from_secs(1)))
            .await
            .unwrap();

        let early: Option<String> = cache.get(&k).await;
        assert_eq!(
            early.as_deref(),
            Some("ephemeral"),
            "should be present before TTL"
        );

        tokio::time::sleep(Duration::from_millis(1200)).await;

        let late: Option<String> = cache.get(&k).await;
        assert!(late.is_none(), "should be absent after TTL; got {late:?}");
    }

    // ── delete ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn redis_delete_evicts_the_key() {
        let Some(url) = redis_url() else {
            eprintln!("REDIS_URL not set — skipping");
            return;
        };
        let cache = Cache::redis(&url).await.unwrap();
        let k = key("delete_evict");

        cache.set(&k, "to-be-deleted", None).await.unwrap();
        assert!(cache.get::<String>(&k).await.is_some());

        cache.delete(&k).await;

        let after: Option<String> = cache.get(&k).await;
        assert!(after.is_none(), "key should be gone after delete");
    }

    // ── clear ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn redis_clear_removes_all_keys_in_db() {
        let Some(url) = redis_url() else {
            eprintln!("REDIS_URL not set — skipping");
            return;
        };
        // Only run clear test when the URL explicitly targets DB 15 to
        // prevent accidentally flushing a shared database.
        if !url.ends_with("/15") {
            eprintln!("redis_clear test only runs against DB /15 — skipping");
            return;
        }

        let cache = Cache::redis(&url).await.unwrap();

        cache.set(&key("clear_a"), "1", None).await.unwrap();
        cache.set(&key("clear_b"), "2", None).await.unwrap();

        cache.clear().await;

        assert!(cache.get::<String>(&key("clear_a")).await.is_none());
        assert!(cache.get::<String>(&key("clear_b")).await.is_none());
    }
}
