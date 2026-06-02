//! umbra-cache — pluggable cache for umbra.
//!
//! Django's cache framework, the slice that matters for production:
//! a [`Cache`] handle over a [`CacheBackend`] trait, three built-in
//! backends (in-memory, SQLite, Redis), and a [`cache_page`] view
//! middleware that caches full GET responses, matching Django's
//! `@cache_page` decorator.
//!
//! ```ignore
//! // Boot wiring (App::builder)
//! let cache = Cache::memory();
//! // … or for Redis in production:
//! // let cache = Cache::redis("redis://localhost:6379/0").await?;
//! CachePlugin::init(cache.clone());
//!
//! // In a handler — explicit cache access
//! cache.set("homepage:html", &rendered, Some(Duration::from_secs(60))).await;
//! if let Some(html) = cache.get::<String>("homepage:html").await {
//!     return Ok(Html(html));
//! }
//!
//! // View-level caching (wraps a Router subtree)
//! use umbra_cache::cache_page;
//! let public = Router::new()
//!     .route("/", get(home))
//!     .layer(cache_page(Duration::from_secs(60)));
//! ```
//!
//! ## Surface
//!
//! - [`CacheBackend`] — the trait. Bytes in, bytes out, async.
//! - [`CacheError`] — unified error type for backends that can fail.
//! - [`Cache`] — the handle. Generic-over-T methods wrap the backend
//!   with serde encoding so callers traffic in their own types.
//! - [`MemoryBackend`] — `tokio::sync::Mutex<HashMap>` with per-key
//!   expiry. Lost on process exit. Default choice for development
//!   and single-process deployments.
//! - [`SqliteBackend`] — table-backed, durable across restarts.
//!   Expired rows are lazily skipped on read and cleared on a
//!   background pass when [`SqliteBackend::sweep`] is called.
//! - [`RedisBackend`] — (feature = `"redis"`) production backend via
//!   `redis::aio::ConnectionManager`. Handles reconnect transparently.
//! - [`cache_page`] — tower [`Layer`] that caches full GET/HEAD responses.
//!   Only status 200 is cached; skips when `Cache-Control: no-store`
//!   or `Set-Cookie` appears on the response.
//! - [`CachePlugin`] — empty Plugin impl so other plugins can name
//!   "cache" as a dependency.
//!
//! ## Deferred past v0
//!
//! - `get_or_set` helper that fills on miss inside a single round-trip.
//! - Versioned keys + `incr/decr` atomic ops.
//! - Memcached backend.
//! - Distributed cache invalidation (tag-based).
//! - ETag / 304 conditional caching inside `cache_page` — the current
//!   implementation always serves the cached body in full.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use umbra::prelude::*;

pub mod cache_page;
pub use cache_page::cache_page;

// ── Ambient cache handle ─────────────────────────────────────────────────────

/// Process-wide ambient cache, set once during `App::build()` (or manually
/// by calling [`CachePlugin::init`]). `cache_page` reads this automatically.
static AMBIENT_CACHE: OnceLock<Cache> = OnceLock::new();

/// Return the ambient cache, or `None` if [`CachePlugin::init`] hasn't run.
pub fn ambient() -> Option<&'static Cache> {
    AMBIENT_CACHE.get()
}

// ── Error type ───────────────────────────────────────────────────────────────

/// Error variants emitted by cache backends that can fail (Redis, SQLite).
/// `MemoryBackend` is infallible — its methods are fire-and-forget.
#[derive(Debug)]
pub enum CacheError {
    /// A Redis-level error (connection, protocol, server).
    #[cfg(feature = "redis")]
    Redis(redis::RedisError),
    /// A SQLite-level error.
    Sqlx(sqlx::Error),
    /// Any other I/O or configuration error.
    Other(String),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "redis")]
            CacheError::Redis(e) => write!(f, "cache redis error: {e}"),
            CacheError::Sqlx(e) => write!(f, "cache sqlite error: {e}"),
            CacheError::Other(s) => write!(f, "cache error: {s}"),
        }
    }
}

impl std::error::Error for CacheError {}

#[cfg(feature = "redis")]
impl From<redis::RedisError> for CacheError {
    fn from(e: redis::RedisError) -> Self {
        CacheError::Redis(e)
    }
}

impl From<sqlx::Error> for CacheError {
    fn from(e: sqlx::Error) -> Self {
        CacheError::Sqlx(e)
    }
}

// ── CacheBackend trait ───────────────────────────────────────────────────────

/// Bytes-in / bytes-out backend. All methods are async because the
/// SQLite and Redis implementations need to be.
///
/// `get_bytes` / `set_bytes` / `delete` / `clear` are infallible at the
/// trait level — backends swallow errors internally and log them rather
/// than propagating. Constructors (`new`, `connect`) surface errors via
/// [`CacheError`] so misconfiguration is caught at boot.
#[async_trait]
pub trait CacheBackend: Send + Sync {
    async fn get(&self, key: &str) -> Option<Vec<u8>>;
    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>);
    async fn delete(&self, key: &str);
    async fn clear(&self);
}

// ── Cache handle ─────────────────────────────────────────────────────────────

/// Public handle. Owns its backend behind an Arc so views can clone
/// it freely (typically stashed in the request context or accessed via
/// the ambient [`AMBIENT_CACHE`]).
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
    pub async fn sqlite(pool: SqlitePool) -> Result<Self, CacheError> {
        let backend = SqliteBackend::new(pool).await?;
        Ok(Self {
            backend: Arc::new(backend),
        })
    }

    /// Build a cache backed by Redis.
    ///
    /// `url` is a Redis connection string: `redis://[user:pass@]host:port/[db]`.
    /// Examples: `redis://localhost:6379/0`, `redis://:password@redis.example.com:6379`.
    ///
    /// The underlying [`redis::aio::ConnectionManager`] reconnects automatically
    /// on dropped connections so the handle is safe to clone and reuse for the
    /// lifetime of the process.
    #[cfg(feature = "redis")]
    pub async fn redis(url: &str) -> Result<Self, CacheError> {
        let backend = RedisBackend::connect(url).await?;
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

    // ── Raw bytes access for cache_page (avoids double-serialisation) ──

    pub(crate) async fn get_bytes_raw(&self, key: &str) -> Option<Vec<u8>> {
        self.backend.get(key).await
    }

    pub(crate) async fn set_bytes_raw(&self, key: &str, bytes: Vec<u8>, ttl: Option<Duration>) {
        self.backend.set(key, bytes, ttl).await;
    }
}

// ── MemoryBackend ────────────────────────────────────────────────────────────

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

// ── SqliteBackend ────────────────────────────────────────────────────────────
//
// CLAUDE.md exception — backend-specific raw SQL is allowed here.
//
// Two reasons this backend keeps `sqlx::query(...)` calls instead of
// going through the ORM:
//
//   1. The `umbra_cache.value` column is `BLOB` / `bytea` — bytes,
//      not text. The ORM's field-type catalogue doesn't yet model
//      `Vec<u8>` (it lives in the deferred-features list); declaring
//      a `CacheEntry` model with a `Vec<u8>` field would fail at
//      `#[derive(Model)]` expansion.
//
//   2. The set path uses `INSERT ... ON CONFLICT(key) DO UPDATE SET
//      excluded.column` — the SQLite upsert syntax. The ORM doesn't
//      expose an upsert terminal at v1 (`get_or_create` exists for
//      the "INSERT or no-op" shape; "INSERT or update" is its own
//      operation and lands when a real consumer needs it).
//
// `SqliteBackend` is explicitly typed `pool: SqlitePool`, so the
// raw SQL has zero portability risk — a user who calls
// `Cache::sqlite(pool)` opted into SQLite by name. The Redis backend
// below handles the non-SQLite case; an eventual `PgBackend` would
// be its own sibling once `Vec<u8>` lands in the catalogue.

/// SQLite-backed cache. Table: `umbra_cache(key TEXT PRIMARY KEY,
/// value BLOB NOT NULL, expires_at TIMESTAMP NULL)`. Expired rows
/// are skipped on read and removed by [`SqliteBackend::sweep`] for
/// periodic cleanup.
pub struct SqliteBackend {
    pool: SqlitePool,
}

impl SqliteBackend {
    pub async fn new(pool: SqlitePool) -> Result<Self, CacheError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS umbra_cache (
                key        TEXT PRIMARY KEY,
                value      BLOB NOT NULL,
                expires_at TIMESTAMP NULL
            )",
        )
        .execute(&pool)
        .await
        .map_err(CacheError::Sqlx)?;
        Ok(Self { pool })
    }

    /// Remove every expired row. Call from a periodic task; reads
    /// already skip expired rows so a call is never required for
    /// correctness, only for keeping the table small.
    pub async fn sweep(&self) -> Result<u64, CacheError> {
        let result =
            sqlx::query("DELETE FROM umbra_cache WHERE expires_at IS NOT NULL AND expires_at <= ?")
                .bind(Utc::now())
                .execute(&self.pool)
                .await
                .map_err(CacheError::Sqlx)?;
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

// ── RedisBackend ─────────────────────────────────────────────────────────────

/// Redis-backed cache. Requires the `redis` cargo feature.
///
/// Uses `redis::aio::ConnectionManager` for automatic reconnection. TTL
/// is stored natively via Redis `SETEX` when a duration is supplied, so
/// expiry is handled server-side and does not require a background sweep.
///
/// `clear()` uses `FLUSHDB` which removes ALL keys in the selected
/// database — use a dedicated Redis database (e.g. `/1`) when sharing
/// a Redis instance with other data.
#[cfg(feature = "redis")]
pub struct RedisBackend {
    client: redis::aio::ConnectionManager,
}

#[cfg(feature = "redis")]
impl RedisBackend {
    /// Connect to Redis at `url`. Returns a ready-to-use backend or a
    /// [`CacheError::Redis`] if the initial connection fails.
    ///
    /// `url` form: `redis://[user:pass@]host:port/[db]`
    /// Example: `redis://localhost:6379/0`
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        let client = redis::Client::open(url).map_err(CacheError::Redis)?;
        let manager = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(CacheError::Redis)?;
        Ok(Self { client: manager })
    }
}

#[cfg(feature = "redis")]
#[async_trait]
impl CacheBackend for RedisBackend {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        use redis::AsyncCommands;
        let mut conn = self.client.clone();
        conn.get::<_, Option<Vec<u8>>>(key).await.ok().flatten()
    }

    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) {
        use redis::AsyncCommands;
        let mut conn = self.client.clone();
        if let Some(dur) = ttl {
            let secs = dur.as_secs().max(1);
            let _: Result<(), _> = conn.set_ex(key, value, secs).await;
        } else {
            let _: Result<(), _> = conn.set(key, value).await;
        }
    }

    async fn delete(&self, key: &str) {
        use redis::AsyncCommands;
        let mut conn = self.client.clone();
        let _: Result<(), _> = conn.del(key).await;
    }

    async fn clear(&self) {
        use redis::AsyncCommands;
        let mut conn = self.client.clone();
        // FLUSHDB removes all keys in the currently selected database.
        // Document this prominently: use a dedicated Redis DB for cache.
        let _: Result<(), _> = redis::cmd("FLUSHDB").query_async::<()>(&mut conn).await;
    }
}

// ── CachePlugin ──────────────────────────────────────────────────────────────

/// The plugin. Carries no models, no routes. Initialise it with a
/// ready-to-use `Cache` handle at app boot so `cache_page` and any
/// handler that calls [`ambient()`] can find it without explicit
/// dependency injection.
///
/// ```ignore
/// CachePlugin::init(Cache::memory());
/// // or for Redis:
/// CachePlugin::init(Cache::redis("redis://localhost:6379/0").await?);
/// ```
#[derive(Debug, Default)]
pub struct CachePlugin;

impl CachePlugin {
    /// Store `cache` as the ambient handle. Must be called before the
    /// first request; calling it twice panics (same contract as
    /// `settings::init`).
    pub fn init(cache: Cache) {
        if AMBIENT_CACHE.set(cache).is_err() {
            panic!("CachePlugin::init called more than once");
        }
    }
}

impl Plugin for CachePlugin {
    fn name(&self) -> &'static str {
        "cache"
    }
}
