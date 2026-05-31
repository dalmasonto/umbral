//! umbra-cache — pluggable cache for umbra.
//!
//! Django's cache framework, the small slice that matters for v0:
//! a [`Cache`] handle over a [`CacheBackend`] trait, plus two
//! built-in backends (in-memory and SQLite). Plugins and views call
//! `cache.get` / `cache.set` against `Cache`; the backend choice is
//! plumbed once at app boot and never appears at the call site.
//!
//! ```ignore
//! let cache = Cache::memory();
//! cache.set("homepage:html", &rendered, Some(Duration::from_secs(60))).await;
//! if let Some(html) = cache.get::<String>("homepage:html").await {
//!     return Ok(Html(html));
//! }
//! ```
//!
//! ## Surface
//!
//! - [`CacheBackend`] — the trait. Bytes in, bytes out, async.
//! - [`Cache`] — the handle. Generic-over-T methods wrap the backend
//!   with serde encoding so callers traffic in their own types.
//! - [`MemoryBackend`] — `tokio::sync::Mutex<HashMap>` with per-key
//!   expiry. Lost on process exit. Default choice for development
//!   and single-process deployments.
//! - [`SqliteBackend`] — table-backed, durable across restarts.
//!   Expired rows are lazily skipped on read and cleared on a
//!   background pass when [`SqliteBackend::sweep`] is called.
//! - [`CachePlugin`] — empty Plugin impl so other plugins can name
//!   "cache" as a dependency.
//!
//! ## Deferred past v0
//!
//! - Redis backend (lands as a separate crate, same trait).
//! - `get_or_set` helper that fills on miss inside a single
//!   round-trip. Lands once a real read path needs it.
//! - Versioned keys + `incr/decr` atomic ops.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use umbra::prelude::*;

/// Bytes-in / bytes-out backend. All methods are async because the
/// SQLite (and future Redis) implementations need to be.
#[async_trait]
pub trait CacheBackend: Send + Sync {
    async fn get(&self, key: &str) -> Option<Vec<u8>>;
    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>);
    async fn delete(&self, key: &str);
    async fn clear(&self);
}

/// Public handle. Owns its backend behind an Arc so views can clone
/// it freely (it's typically stashed in the request context).
#[derive(Clone)]
pub struct Cache {
    backend: Arc<dyn CacheBackend>,
}

impl Cache {
    /// Build a cache backed by a freshly-allocated [`MemoryBackend`].
    pub fn memory() -> Self {
        Self {
            backend: Arc::new(MemoryBackend::default()),
        }
    }

    /// Build a cache backed by a SQLite table. The constructor
    /// creates the table on first call; it's idempotent.
    pub async fn sqlite(pool: SqlitePool) -> Result<Self, sqlx::Error> {
        let backend = SqliteBackend::new(pool).await?;
        Ok(Self {
            backend: Arc::new(backend),
        })
    }

    /// Wrap an arbitrary backend.
    pub fn with_backend(backend: Arc<dyn CacheBackend>) -> Self {
        Self { backend }
    }

    /// Look up a key, deserialise to T. Returns None on miss, on
    /// expiry, or on a decode error (the entry is treated as
    /// poisoned and ignored rather than crashing the caller).
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let bytes = self.backend.get(key).await?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Set a key. The value is serialised with serde_json. `ttl =
    /// None` means no expiry.
    pub async fn set<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> Result<(), serde_json::Error> {
        let bytes = serde_json::to_vec(value)?;
        self.backend.set(key, bytes, ttl).await;
        Ok(())
    }

    pub async fn delete(&self, key: &str) {
        self.backend.delete(key).await;
    }

    pub async fn clear(&self) {
        self.backend.clear().await;
    }
}

struct MemoryEntry {
    value: Vec<u8>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
pub struct MemoryBackend {
    inner: Mutex<HashMap<String, MemoryEntry>>,
}

#[async_trait]
impl CacheBackend for MemoryBackend {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        let mut map = self.inner.lock().await;
        if let Some(entry) = map.get(key) {
            if let Some(exp) = entry.expires_at {
                if Utc::now() >= exp {
                    map.remove(key);
                    return None;
                }
            }
            return Some(entry.value.clone());
        }
        None
    }

    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) {
        let expires_at = ttl.and_then(|d| {
            chrono::Duration::from_std(d)
                .ok()
                .and_then(|cd| Utc::now().checked_add_signed(cd))
        });
        self.inner
            .lock()
            .await
            .insert(key.to_string(), MemoryEntry { value, expires_at });
    }

    async fn delete(&self, key: &str) {
        self.inner.lock().await.remove(key);
    }

    async fn clear(&self) {
        self.inner.lock().await.clear();
    }
}

/// SQLite-backed cache. Table: `umbra_cache(key TEXT PRIMARY KEY,
/// value BLOB NOT NULL, expires_at TIMESTAMP NULL)`. Expired rows
/// are skipped on read and removed by [`SqliteBackend::sweep`] for
/// periodic cleanup.
pub struct SqliteBackend {
    pool: SqlitePool,
}

impl SqliteBackend {
    pub async fn new(pool: SqlitePool) -> Result<Self, sqlx::Error> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS umbra_cache (
                key        TEXT PRIMARY KEY,
                value      BLOB NOT NULL,
                expires_at TIMESTAMP NULL
            )",
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }

    /// Remove every expired row. Call from a periodic task; reads
    /// already skip expired rows so a call is never required for
    /// correctness, only for keeping the table small.
    pub async fn sweep(&self) -> Result<u64, sqlx::Error> {
        let result =
            sqlx::query("DELETE FROM umbra_cache WHERE expires_at IS NOT NULL AND expires_at <= ?")
                .bind(Utc::now())
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected())
    }
}

#[async_trait]
impl CacheBackend for SqliteBackend {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        let row: Option<(Vec<u8>, Option<DateTime<Utc>>)> =
            sqlx::query_as("SELECT value, expires_at FROM umbra_cache WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await
                .ok()?;
        let (value, expires_at) = row?;
        if let Some(exp) = expires_at {
            if Utc::now() >= exp {
                // Treat the read as a miss and let `sweep` clean up
                // the row eventually. Doing the DELETE here would
                // turn every GET against an expired key into a
                // write, which is a poor tradeoff.
                return None;
            }
        }
        Some(value)
    }

    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) {
        let expires_at = ttl.and_then(|d| {
            chrono::Duration::from_std(d)
                .ok()
                .and_then(|cd| Utc::now().checked_add_signed(cd))
        });
        let _ = sqlx::query(
            "INSERT INTO umbra_cache (key, value, expires_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at",
        )
        .bind(key)
        .bind(value)
        .bind(expires_at)
        .execute(&self.pool)
        .await;
    }

    async fn delete(&self, key: &str) {
        let _ = sqlx::query("DELETE FROM umbra_cache WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await;
    }

    async fn clear(&self) {
        let _ = sqlx::query("DELETE FROM umbra_cache")
            .execute(&self.pool)
            .await;
    }
}

/// The plugin. Carries no models, no routes; its job is to be
/// nameable as a dependency by other plugins that need to know the
/// cache subsystem is wired up (e.g. a future rate-limiter plugin).
#[derive(Debug, Default)]
pub struct CachePlugin;

impl Plugin for CachePlugin {
    fn name(&self) -> &'static str {
        "cache"
    }
}
