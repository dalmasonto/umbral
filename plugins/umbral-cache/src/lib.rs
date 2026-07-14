//! umbral-cache — pluggable cache for umbral.
//!
//! A cache framework, the slice that matters for production:
//! a [`Cache`] handle over a [`CacheBackend`] trait, three built-in
//! backends (in-memory, SQLite, Redis), and a [`cache_page`] view
//! middleware that caches full GET responses.
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
//! use umbral_cache::cache_page;
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
use http::header::{CACHE_CONTROL, HeaderValue, VARY};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use umbral::prelude::*;

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
// The original blockers (no `Vec<u8>` field type, no `upsert`
// terminal) both shipped in subsequent commits — see
// `SqlType::Bytes` and `Manager::upsert`. The remaining reason this
// backend keeps `sqlx::query(...)` calls:
//
//   `SqliteBackend` takes an EXPLICIT `SqlitePool` by design (not
//   the framework's ambient pool). The ORM's `Manager` terminals
//   read `umbral::db::pool()` for ambient routing; binding them to
//   a different pool requires an `Manager::upsert_with(&pool, ...)`
//   escape hatch that doesn't yet exist. Adding it lands when the
//   first non-ambient-pool consumer asks for it.
//
// `Cache::sqlite(pool)` is the explicit-pool entry point — a user
// who calls it opted into SQLite by name AND into a pool that may
// be separate from the framework's main pool (cache I/O frequently
// runs against its own smaller, dedicated pool). The Redis backend
// below handles the non-SQLite case; an eventual `PgBackend` would
// be its own sibling.

/// SQLite-backed cache. Table: `umbral_cache(key TEXT PRIMARY KEY,
/// value BLOB NOT NULL, expires_at TIMESTAMP NULL)`. Expired rows
/// are skipped on read and removed by [`SqliteBackend::sweep`] for
/// periodic cleanup.
pub struct SqliteBackend {
    pool: SqlitePool,
}

impl SqliteBackend {
    pub async fn new(pool: SqlitePool) -> Result<Self, CacheError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS umbral_cache (
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
        let result = sqlx::query(
            "DELETE FROM umbral_cache WHERE expires_at IS NOT NULL AND expires_at <= ?",
        )
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
            sqlx::query_as("SELECT value, expires_at FROM umbral_cache WHERE key = ?")
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
        // BROKEN-12: log swallowed write errors. A cache backend is
        // best-effort (a failed write must not break the request), but a
        // locked SQLite / dead pool that no-ops every write forever should
        // not be invisible — the trait doc promised "and log them".
        if let Err(e) = sqlx::query(
            "INSERT INTO umbral_cache (key, value, expires_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at",
        )
        .bind(key)
        .bind(value)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        {
            tracing::warn!(error = %e, key, "umbral-cache: SQLite cache set failed (swallowed)");
        }
    }

    async fn delete(&self, key: &str) {
        if let Err(e) = sqlx::query("DELETE FROM umbral_cache WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await
        {
            tracing::warn!(error = %e, key, "umbral-cache: SQLite cache delete failed (swallowed)");
        }
    }

    async fn clear(&self) {
        if let Err(e) = sqlx::query("DELETE FROM umbral_cache")
            .execute(&self.pool)
            .await
        {
            tracing::warn!(error = %e, "umbral-cache: SQLite cache clear failed (swallowed)");
        }
    }
}

// ── RedisBackend ─────────────────────────────────────────────────────────────

/// Redis-backed cache. Requires the `redis` cargo feature.
///
/// Uses `redis::aio::ConnectionManager` for automatic reconnection. TTL
/// is stored natively via Redis `SETEX` when a duration is supplied, so
/// expiry is handled server-side and does not require a background sweep.
///
/// gaps4 #21: every key is namespaced under [`Self::DEFAULT_PREFIX`], and
/// `clear()` deletes only keys under that prefix (via `SCAN` + `UNLINK`) — NOT
/// `FLUSHDB`, which would wipe every co-tenant of a shared Redis (sessions,
/// rate limits, queues, another app). Point the backend at its own logical DB
/// too if you can, but the prefix means a shared DB is no longer a data-loss
/// footgun.
#[cfg(feature = "redis")]
pub struct RedisBackend {
    client: redis::aio::ConnectionManager,
    prefix: String,
}

#[cfg(feature = "redis")]
impl RedisBackend {
    /// Connect to Redis at `url`. Returns a ready-to-use backend or a
    /// [`CacheError::Redis`] if the initial connection fails.
    ///
    /// `url` form: `redis://[user:pass@]host:port/[db]`
    /// Example: `redis://localhost:6379/0`
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        Self::connect_with_prefix(url, Self::DEFAULT_PREFIX).await
    }

    /// The key namespace. Every entry is stored as `<prefix><key>`, and
    /// `clear()` deletes exactly `<prefix>*`.
    pub const DEFAULT_PREFIX: &'static str = "umbral:cache:";

    /// [`Self::connect`] with a custom key prefix — set one per app when
    /// several umbral apps share one Redis DB so their caches don't collide
    /// and each `clear()` stays scoped to its own app.
    pub async fn connect_with_prefix(url: &str, prefix: &str) -> Result<Self, CacheError> {
        let client = redis::Client::open(url).map_err(CacheError::Redis)?;
        let manager = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(CacheError::Redis)?;
        Ok(Self {
            client: manager,
            prefix: prefix.to_string(),
        })
    }

    /// Namespace a caller key under this backend's prefix.
    fn k(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }
}

#[cfg(feature = "redis")]
#[async_trait]
impl CacheBackend for RedisBackend {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        use redis::AsyncCommands;
        let mut conn = self.client.clone();
        conn.get::<_, Option<Vec<u8>>>(self.k(key))
            .await
            .ok()
            .flatten()
    }

    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) {
        use redis::AsyncCommands;
        let mut conn = self.client.clone();
        let key = self.k(key);
        // BROKEN-12: log swallowed errors — a dead Redis that no-ops every
        // write should not be silent (the trait doc promised "and log them").
        let res: Result<(), _> = if let Some(dur) = ttl {
            let secs = dur.as_secs().max(1);
            conn.set_ex(&key, value, secs).await
        } else {
            conn.set(&key, value).await
        };
        if let Err(e) = res {
            tracing::warn!(error = %e, key, "umbral-cache: Redis cache set failed (swallowed)");
        }
    }

    async fn delete(&self, key: &str) {
        use redis::AsyncCommands;
        let mut conn = self.client.clone();
        let key = self.k(key);
        if let Err(e) = conn.del::<_, ()>(&key).await {
            tracing::warn!(error = %e, key, "umbral-cache: Redis cache delete failed (swallowed)");
        }
    }

    async fn clear(&self) {
        // gaps4 #21: delete only THIS cache's keys, never `FLUSHDB`. SCAN the
        // keyspace for `<prefix>*` in batches and UNLINK (non-blocking DEL) each
        // batch, so a Redis DB shared with sessions / rate-limits / another app
        // keeps its unrelated keys.
        use redis::AsyncCommands;
        let mut conn = self.client.clone();
        let pattern = format!("{}*", self.prefix);
        let mut cursor: u64 = 0;
        loop {
            let scan: redis::RedisResult<(u64, Vec<String>)> = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(512)
                .query_async(&mut conn)
                .await;
            let (next, keys) = match scan {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "umbral-cache: Redis cache clear SCAN failed (swallowed)");
                    return;
                }
            };
            if !keys.is_empty() {
                if let Err(e) = conn.unlink::<_, ()>(keys).await {
                    tracing::warn!(error = %e, "umbral-cache: Redis cache clear UNLINK failed (swallowed)");
                }
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
    }
}

// ── CacheHeaders config ──────────────────────────────────────────────────────

/// Opt-in HTTP response-header config for `CachePlugin`.
///
/// Both knobs are **off by default** — wiring a `CachePlugin` without calling
/// [`CachePlugin::with_compression`] or [`CachePlugin::cache_control`] leaves
/// the response pipeline unchanged.
///
/// They are independent of and composable with the server-side `cache_page`
/// store: `cache_page` caches full response bodies, while these knobs emit
/// HTTP headers that tell downstream clients and proxies how to treat responses.
///
/// # Example
///
/// ```ignore
/// App::builder()
///     .plugin(
///         CachePlugin::new(Cache::memory())
///             .with_compression()
///             .cache_control("public, max-age=3600")
///             .vary("Accept-Encoding"),
///     )
///     .build()
///     .await?;
/// ```
#[derive(Debug, Clone, Default)]
pub struct CacheHeaders {
    /// When `true`, applies `tower_http::compression::CompressionLayer` to the
    /// router. The layer negotiates encoding with the client via
    /// `Accept-Encoding` and compresses responses with gzip, brotli, deflate,
    /// or zstd as available.
    pub compression: bool,
    /// When `Some(value)`, emits a `Cache-Control` response header on every
    /// response (using `SetResponseHeaderLayer::overriding`). The value is the
    /// raw directive string, e.g. `"public, max-age=3600"` or `"no-store"`.
    pub cache_control: Option<String>,
    /// When `Some(value)`, emits a `Vary` response header. Common value:
    /// `"Accept-Encoding"` to tell caches that responses differ by encoding.
    pub vary: Option<String>,
}

// ── CachePlugin ──────────────────────────────────────────────────────────────

/// The plugin. Carries no models, no routes — just a `Cache` handle it
/// installs as the ambient cache at boot, so `cache_page` and any handler
/// that calls [`ambient()`] find it without explicit dependency injection.
///
/// Idiomatic registration (the carried cache is wired in `on_ready`):
///
/// ```ignore
/// App::builder()
///     .plugin(CachePlugin::new(Cache::memory()))
///     // or: CachePlugin::new(Cache::redis("redis://localhost:6379/0").await?)
///     .build()?;
/// ```
///
/// `CachePlugin::init(cache)` remains for manual/test wiring outside the
/// plugin lifecycle.
///
/// ## Opt-in compression and Cache-Control headers
///
/// ```ignore
/// CachePlugin::new(Cache::memory())
///     .with_compression()               // enables gzip/br/zstd negotiation
///     .cache_control("public, max-age=3600")
///     .vary("Accept-Encoding")
/// ```
#[derive(Default)]
pub struct CachePlugin {
    /// Cache to install as the ambient handle in [`Plugin::on_ready`].
    /// `None` for the legacy unit-style registration (where the ambient
    /// cache is wired separately via [`CachePlugin::init`]).
    cache: Option<Cache>,
    /// Opt-in HTTP header + compression config. Default: nothing applied.
    headers: CacheHeaders,
}

impl CachePlugin {
    /// Build the plugin carrying `cache`. The idiomatic
    /// `App::builder().plugin(CachePlugin::new(Cache::memory()))` then
    /// installs it as the ambient handle at boot (BROKEN-9) — no separate
    /// `init` call, so `cache_page` actually caches.
    pub fn new(cache: Cache) -> Self {
        Self {
            cache: Some(cache),
            headers: CacheHeaders::default(),
        }
    }

    /// Store `cache` as the ambient handle directly, outside the plugin
    /// lifecycle. Prefer [`CachePlugin::new`] in app code; this stays for
    /// manual / test wiring. Calling it twice panics (same contract as
    /// `settings::init`).
    pub fn init(cache: Cache) {
        if AMBIENT_CACHE.set(cache).is_err() {
            panic!("CachePlugin::init called more than once");
        }
    }

    /// Enable response compression. Applies `tower_http::compression::CompressionLayer`
    /// to the router; negotiates gzip / brotli / deflate / zstd via `Accept-Encoding`.
    /// Default: off.
    pub fn with_compression(mut self) -> Self {
        self.headers.compression = true;
        self
    }

    /// Emit a `Cache-Control` header on every response. `value` is the raw
    /// directive string (e.g. `"public, max-age=3600"`, `"no-store"`).
    /// Default: not set.
    pub fn cache_control(mut self, value: impl Into<String>) -> Self {
        self.headers.cache_control = Some(value.into());
        self
    }

    /// Emit a `Vary` header on every response. Typically paired with
    /// [`with_compression`][Self::with_compression]: `"Accept-Encoding"` tells
    /// caches that different encodings are distinct variants of the same URL.
    /// Default: not set.
    pub fn vary(mut self, value: impl Into<String>) -> Self {
        self.headers.vary = Some(value.into());
        self
    }
}

impl Plugin for CachePlugin {
    fn name(&self) -> &'static str {
        "cache"
    }

    fn wrap_router(&self, router: Router) -> Router {
        let h = &self.headers;
        let mut router = router;

        // Cache-Control header (overriding — the plugin's policy takes
        // precedence over whatever a handler set).
        if let Some(ref val) = h.cache_control {
            if let Ok(hv) = HeaderValue::from_str(val) {
                router = router.layer(SetResponseHeaderLayer::overriding(CACHE_CONTROL, hv));
            } else {
                tracing::warn!(
                    value = %val,
                    "CachePlugin: cache_control value contains invalid header characters; \
                     Cache-Control header will NOT be emitted"
                );
            }
        }

        // Vary header (overriding).
        if let Some(ref val) = h.vary {
            if let Ok(hv) = HeaderValue::from_str(val) {
                router = router.layer(SetResponseHeaderLayer::overriding(VARY, hv));
            } else {
                tracing::warn!(
                    value = %val,
                    "CachePlugin: vary value contains invalid header characters; \
                     Vary header will NOT be emitted"
                );
            }
        }

        // Compression (outermost so the body is already compressed before any
        // header-setter above runs on the response on the way out).
        if h.compression {
            router = router.layer(CompressionLayer::new());
        }

        router
    }

    fn on_ready(
        &self,
        _ctx: &umbral::plugin::AppContext,
    ) -> Result<(), umbral::plugin::PluginError> {
        // BROKEN-9: registering the plugin must actually wire the cache,
        // otherwise `cache_page` silently no-ops on every request. If a
        // cache was supplied via `new`, install it as the ambient handle.
        match &self.cache {
            Some(cache) => {
                if AMBIENT_CACHE.set(cache.clone()).is_err() {
                    tracing::warn!(
                        "CachePlugin::new: an ambient cache was already installed (via \
                         CachePlugin::init or another CachePlugin); ignoring this one."
                    );
                }
            }
            None if AMBIENT_CACHE.get().is_none() => {
                tracing::warn!(
                    "CachePlugin registered with no cache and none set via CachePlugin::init — \
                     cache_page layers will silently no-op. Use \
                     CachePlugin::new(Cache::memory())."
                );
            }
            None => {}
        }
        Ok(())
    }
}
