//! Ambient, object-safe cache contract for boolean, tag-invalidated decisions
//! (e.g. media-access). Implemented by the umbral-cache plugin and consumed by
//! other plugins via `umbral::cache` — so no plugin depends on umbral-cache.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;

#[async_trait]
pub trait TaggedCache: Send + Sync {
    async fn get_bool(&self, key: &str) -> Option<bool>;
    async fn set_tagged_bool(&self, key: &str, value: bool, ttl: Option<Duration>, tags: &[String]);
    async fn bust_tag(&self, tag: &str);

    /// A monotonically increasing counter that advances on every [`bust_tag`].
    /// Capture it BEFORE computing a cacheable decision, then pass it to
    /// [`set_tagged_bool_guarded`] so a bust that races the computation can be
    /// detected at store time.
    ///
    /// [`bust_tag`]: Self::bust_tag
    /// [`set_tagged_bool_guarded`]: Self::set_tagged_bool_guarded
    async fn bust_epoch(&self) -> u64 {
        0
    }

    /// Store `key → value` (tagged, TTL-bounded) UNLESS one of `tags` was
    /// busted after `since_epoch` — the epoch captured before the value was
    /// computed. Returns `true` if the value was stored, `false` if the store
    /// was skipped because a racing bust already invalidated it.
    ///
    /// This closes the miss-compute/bust write-skew race (gaps6 #12): without
    /// it, a revocation landing between a cache miss and its store is silently
    /// overwritten by the stale, just-computed decision, which then lives until
    /// TTL. The default impl does an unguarded store (backward-compatible for
    /// caches that don't track a bust epoch); a real cache overrides it.
    async fn set_tagged_bool_guarded(
        &self,
        key: &str,
        value: bool,
        ttl: Option<Duration>,
        tags: &[String],
        since_epoch: u64,
    ) -> bool {
        let _ = since_epoch;
        self.set_tagged_bool(key, value, ttl, tags).await;
        true
    }
}

static AMBIENT: OnceLock<Arc<dyn TaggedCache>> = OnceLock::new();

pub fn ambient_tagged_cache() -> Option<Arc<dyn TaggedCache>> {
    AMBIENT.get().cloned()
}

/// Register the process-wide ambient tagged cache (set-once; a later call is ignored).
pub fn set_ambient_tagged_cache(cache: Arc<dyn TaggedCache>) {
    let _ = AMBIENT.set(cache);
}
