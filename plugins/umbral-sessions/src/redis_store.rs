//! `RedisStore` — a feature-gated, Redis-backed [`SessionStore`] (Phase 2c).
//!
//! ## Design
//!
//! `RedisStore` is a **keyed store** like [`DbStore`]: the raw token lives only
//! in the browser cookie; the server holds the record under a hashed key.
//! Unlike `DbStore`, storage is Redis rather than SQL — `GET` / `SET` /
//! `DEL` with a server-side TTL so Redis auto-evicts expired sessions without a
//! scheduled cleanup job.
//!
//! ```text
//!   save:  record --serde_json--> JSON string
//!          SET umbral:session:<sha256hex(token)>  <json>  EX <ttl_secs>
//!   load:  GET umbral:session:<sha256hex(token)>
//!          -> None (nil reply, or expires_at < now)
//!          -> Some(serde_json::from_str(json))
//!   destroy: DEL umbral:session:<sha256hex(token)>
//! ```
//!
//! ## Expiry
//!
//! `save` calls `SET … EX <seconds>` where the TTL is derived from
//! `record.expires_at - now()`. Redis auto-evicts the key when the TTL
//! fires, so there is no equivalent of `clearsessions` needed for Redis.
//! `load` also double-checks the in-record `expires_at` (clocks can drift
//! between processes) and issues a `DEL` if the record is past its time
//! even though Redis hasn't evicted it yet.
//!
//! ## Connection
//!
//! Uses `redis::aio::ConnectionManager` (the same pattern as `umbral-cache`'s
//! `RedisBackend`). The manager handles reconnection transparently and is
//! cheap to clone (internally Arc-backed), so each command clones it without
//! allocating a new connection.
//!
//! ## Feature gate
//!
//! This module is compiled only when the `redis` cargo feature is active.
//! Enable with:
//!
//! ```toml
//! umbral-sessions = { …, features = ["redis"] }
//! ```
//!
//! Install the store at boot:
//!
//! ```ignore
//! let store = RedisStore::connect("redis://localhost:6379/0").await?;
//! App::builder()
//!     .plugin(SessionsPlugin::default().store(store))
//!     .build()
//!     .await?;
//! ```

use chrono::Utc;

use crate::{
    SessionError,
    store::{SessionRecord, SessionStore, hash_token},
};

/// Key prefix for all session keys in Redis. Namespaces umbral sessions
/// away from other data in the same Redis database.
const KEY_PREFIX: &str = "umbral:session:";

// =========================================================================
// RedisStore
// =========================================================================

/// Redis-backed session store. Requires the `redis` cargo feature.
///
/// Sessions are stored as JSON strings under keys of the form
/// `umbral:session:<sha256hex(token)>` with a native Redis TTL derived
/// from `record.expires_at`. Redis auto-evicts expired keys server-side,
/// eliminating the need for a `clearsessions` sweep job.
///
/// `RedisStore` implements [`SessionStore`] exactly like [`DbStore`]:
/// it is a **keyed store** — the raw session token lives only in the
/// cookie; the JSON record lives in Redis under its hash. `save` returns
/// the raw `token` unchanged (the cookie value stays the opaque token).
///
/// Construct via [`RedisStore::connect`] (async) or the
/// [`RedisStore::from_env`] convenience that reads `UMBRAL_REDIS_URL`.
#[derive(Clone)]
pub struct RedisStore {
    client: redis::aio::ConnectionManager,
}

/// `ConnectionManager` doesn't implement `Debug` (it wraps internal async
/// machinery). We implement it manually so `RedisStore` satisfies the
/// `SessionStore: Debug` bound without exposing internal Redis state.
impl std::fmt::Debug for RedisStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisStore")
            .field("client", &"redis::aio::ConnectionManager")
            .finish()
    }
}

impl RedisStore {
    /// Connect to Redis at `url`. Returns a ready-to-use store, or a
    /// [`SessionError::Redis`] if the initial connection fails.
    ///
    /// `url` form: `redis://[user:pass@]host:port/[db]`
    /// Example: `redis://localhost:6379/0`
    ///
    /// The underlying [`redis::aio::ConnectionManager`] reconnects
    /// automatically on dropped connections, so the handle is safe to clone
    /// and reuse for the lifetime of the process.
    pub async fn connect(url: &str) -> Result<Self, SessionError> {
        let client = redis::Client::open(url).map_err(|e| SessionError::Redis(e.to_string()))?;
        let manager = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(|e| SessionError::Redis(e.to_string()))?;
        Ok(Self { client: manager })
    }

    /// Convenience: reads the Redis URL from the `UMBRAL_REDIS_URL`
    /// environment variable and calls [`RedisStore::connect`].
    ///
    /// Returns a [`SessionError::Redis`] when the variable is absent or the
    /// connection fails.
    pub async fn from_env() -> Result<Self, SessionError> {
        let url = std::env::var("UMBRAL_REDIS_URL").map_err(|_| {
            SessionError::Redis("UMBRAL_REDIS_URL environment variable not set".to_string())
        })?;
        Self::connect(&url).await
    }

    /// The Redis key for a raw session token.
    fn key(token: &str) -> String {
        format!("{}{}", KEY_PREFIX, hash_token(token))
    }
}

#[async_trait::async_trait]
impl SessionStore for RedisStore {
    /// Load the session record for a raw cookie token.
    ///
    /// 1. Compute the Redis key: `umbral:session:<sha256(token)>`.
    /// 2. `GET` the key. `nil` reply → `Ok(None)`.
    /// 3. `serde_json::from_str::<SessionRecord>` the value.
    /// 4. If `expires_at < now()` (clock drift / late eviction), `DEL`
    ///    the key and return `Ok(None)`.
    /// 5. Otherwise return `Ok(Some(record))`.
    async fn load(&self, token: &str) -> Result<Option<SessionRecord>, SessionError> {
        use redis::AsyncCommands;
        let key = Self::key(token);
        let mut conn = self.client.clone();
        let raw: Option<String> = conn
            .get(&key)
            .await
            .map_err(|e| SessionError::Redis(e.to_string()))?;
        let json = match raw {
            None => return Ok(None),
            Some(s) => s,
        };
        let record: SessionRecord = serde_json::from_str(&json).map_err(SessionError::Json)?;
        // Double-check: Redis TTL may not have fired yet (clock skew).
        if record.expires_at < Utc::now() {
            let _: () = conn
                .del(&key)
                .await
                .map_err(|e| SessionError::Redis(e.to_string()))?;
            return Ok(None);
        }
        Ok(Some(record))
    }

    /// Create-or-update the full record under `token`.
    ///
    /// Serialises `record` to JSON, then calls
    /// `SET umbral:session:<hash> <json> EX <ttl_secs>`.
    /// The TTL is `max(0, expires_at - now())` seconds; if it is zero
    /// (the record has already expired) we still write a 1-second key so
    /// the subsequent `load` sees it, applies the expiry branch, and
    /// DELs it — consistent with `DbStore`'s lazy-cleanup behaviour.
    ///
    /// Returns `token` unchanged; the cookie value stays the opaque token.
    async fn save(&self, token: &str, record: &SessionRecord) -> Result<String, SessionError> {
        use redis::AsyncCommands;
        let key = Self::key(token);
        let json = serde_json::to_string(record).map_err(SessionError::Json)?;
        let ttl_secs = (record.expires_at - Utc::now()).num_seconds().max(1) as u64; // at least 1s so Redis accepts the EXPIRE
        let mut conn = self.client.clone();
        conn.set_ex::<_, _, ()>(&key, json, ttl_secs)
            .await
            .map_err(|e| SessionError::Redis(e.to_string()))?;
        Ok(token.to_string())
    }

    /// Delete the session from Redis. Idempotent — `DEL` on a
    /// non-existent key is a no-op in Redis.
    async fn destroy(&self, token: &str) -> Result<(), SessionError> {
        use redis::AsyncCommands;
        let key = Self::key(token);
        let mut conn = self.client.clone();
        conn.del::<_, ()>(&key)
            .await
            .map_err(|e| SessionError::Redis(e.to_string()))?;
        Ok(())
    }
}
