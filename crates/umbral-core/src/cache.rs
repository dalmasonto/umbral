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
}

static AMBIENT: OnceLock<Arc<dyn TaggedCache>> = OnceLock::new();

pub fn ambient_tagged_cache() -> Option<Arc<dyn TaggedCache>> {
    AMBIENT.get().cloned()
}

/// Register the process-wide ambient tagged cache (set-once; a later call is ignored).
pub fn set_ambient_tagged_cache(cache: Arc<dyn TaggedCache>) {
    let _ = AMBIENT.set(cache);
}
