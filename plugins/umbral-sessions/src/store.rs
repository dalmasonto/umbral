//! `SessionStore` trait + `DbStore` implementation + ambient install.
//!
//! ## Design
//!
//! `SessionStore` is the pluggable storage abstraction for umbral sessions.
//! `DbStore` is the default implementation: it reads and writes the `session`
//! table through the ORM (`Session::objects()`), operating on the full
//! `SessionRecord` (id, user_id, data, created_at, expires_at).
//!
//! The ambient `OnceLock<Arc<dyn SessionStore>>` follows the same pattern as
//! umbral-core's ambient pool: set once at boot, read everywhere after that.
//! If no store was installed, `active_store()` returns a default `DbStore`
//! that uses the ambient ORM pool.

use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};

use crate::SessionError;

// =========================================================================
// Public: hash_token — exposed as `pub(crate)` for tests and within the
// crate. Tests reference it via the re-export `hash_token_pub`.
// =========================================================================

/// SHA-256 hash the raw token, hex-encoded. The DB column holds this
/// digest; the raw token only lives in the cookie.
///
/// Exposed as `pub(crate)` so `store.rs` and `lib.rs` share one copy.
/// Tests access it via [`hash_token_pub`].
pub(crate) fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Thin public re-export of [`hash_token`] for integration tests that need
/// to verify the stored digest matches the expected hash of a raw token.
/// Production code outside this crate has no need to call this directly.
pub fn hash_token_pub(raw: &str) -> String {
    hash_token(raw)
}

// =========================================================================
// SessionRecord — the data the store persists / retrieves.
// =========================================================================

/// All the data the store persists for one session. Does NOT include the
/// `id` column because the store derives the stored id from the token
/// internally (the raw token is hashed before storage so a DB leak doesn't
/// surrender live session tokens).
///
/// Derives `Serialize`/`Deserialize` so a stateless [`CookieStore`] can
/// encode the whole record into the encrypted cookie value and decode it
/// back out — the DB-backed [`DbStore`] never serialises the record (it
/// writes columns), but the cookie store needs the round-trip.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionRecord {
    /// The user PK serialised as a string (`None` for anonymous sessions).
    pub user_id: Option<String>,
    /// Free-form JSON string owned by the application (`"{}"` when empty).
    pub data: String,
    /// When the session was first created.
    pub created_at: DateTime<Utc>,
    /// When the session expires (after which `load` returns `None` and
    /// deletes the row — lazy cleanup, no scheduled job required).
    pub expires_at: DateTime<Utc>,
}

// =========================================================================
// SessionStore trait
// =========================================================================

/// Pluggable storage back-end for umbral sessions.
///
/// The default implementation is [`DbStore`] which reproduces the DB-backed
/// session behaviour in `lib.rs`. A future `CookieStore` (signed, stateless)
/// or a `RedisStore` can implement this trait and be installed via
/// [`install_store`].
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync + std::fmt::Debug {
    /// Load the session record for a raw cookie token. Returns `None` if the
    /// row is absent OR if it has expired (expired rows are deleted as a side
    /// effect — lazy cleanup, no scheduled job required).
    async fn load(&self, token: &str) -> Result<Option<SessionRecord>, SessionError>;

    /// Create-or-update the full record under `token`. The raw token is
    /// hashed before storage; the caller should never call this with the
    /// hashed form. Returns the cookie value to set (equal to `token` for
    /// `DbStore`; may differ for a future stateless `CookieStore`).
    async fn save(&self, token: &str, record: &SessionRecord) -> Result<String, SessionError>;

    /// Delete the session. Idempotent — deleting a non-existent token is
    /// treated as success.
    async fn destroy(&self, token: &str) -> Result<(), SessionError>;

    /// Delete EVERY session owned by `user_id` — the "log out everywhere"
    /// primitive behind password-reset revocation (audit_2 H7). Returns the
    /// number of sessions removed.
    ///
    /// The default fails with [`SessionError::RevocationUnsupported`] so a store
    /// that can't enumerate by user (a stateless [`crate::CookieStore`]) reports
    /// the gap LOUDLY rather than silently no-op'ing and leaving stolen cookies
    /// live. Server-side stores ([`DbStore`], a Redis store) override it.
    async fn destroy_user(&self, _user_id: &str) -> Result<u64, SessionError> {
        Err(SessionError::RevocationUnsupported)
    }

    /// Whether this store's security depends on the ambient `secret_key`.
    ///
    /// A stateless store that seals the whole session into the cookie (the
    /// secret-derived [`crate::CookieStore`]) returns `true`: an empty or
    /// insecure-default secret makes every cookie forgeable, so the
    /// `SessionsPlugin` boot check hard-fails on it in production. Stores that
    /// keep the session server-side ([`DbStore`], a Redis store) derive nothing
    /// from the secret and return `false` (the default).
    fn requires_ambient_secret(&self) -> bool {
        false
    }

    /// Invalidation hook: a session row with this **stored id** (the hashed
    /// token — the `session.id` column value) changed, through or outside
    /// this store. [`install_store`] subscribes to the ORM's
    /// `post_save:session` / `post_delete:session` (and bulk) signals and
    /// calls this for every ORM-mediated write, so caches stay correct even
    /// when the write did not pass through the store — the session helpers
    /// in `lib.rs` deliberately mix both routings.
    ///
    /// Caching stores ([`CachedDbStore`]) evict the key. The default is a
    /// no-op: a store with no cache has nothing to invalidate.
    ///
    /// Scope, stated honestly: this covers ORM-mediated writes **in this
    /// process**. A write from another process or raw SQL is invisible here
    /// and is bounded only by a caching store's TTL — keep the TTL short
    /// when several processes share the session table.
    fn invalidate(&self, _stored_id: &str) {}
}

// =========================================================================
// DbStore — DB-backed implementation
// =========================================================================

/// DB-backed session store. Holds no state itself — all reads and writes go
/// through the ambient ORM pool installed at `App::build()`.
///
/// This is the default implementation and reproduces the exact SQL behaviour
/// of the helper functions in `lib.rs`.
#[derive(Debug, Default, Clone)]
pub struct DbStore;

#[async_trait::async_trait]
impl SessionStore for DbStore {
    /// Reproduces the logic of `read_session` in `lib.rs`:
    /// 1. Hash the raw token.
    /// 2. Query the `session` table by the hash.
    /// 3. If the row exists but is expired, delete it and return `None`.
    async fn load(&self, token: &str) -> Result<Option<SessionRecord>, SessionError> {
        use crate::{Session, session};
        let stored_id = hash_token(token);
        let row: Option<Session> = Session::objects()
            .filter(session::ID.eq(&stored_id))
            .first()
            .await?;
        match row {
            None => Ok(None),
            Some(s) if s.expires_at < Utc::now() => {
                // Lazy expiry: delete the stale row and report absence.
                Session::objects()
                    .filter(session::ID.eq(&stored_id))
                    .delete()
                    .await?;
                Ok(None)
            }
            Some(s) => Ok(Some(SessionRecord {
                user_id: s.user_id,
                data: s.data,
                created_at: s.created_at,
                expires_at: s.expires_at,
            })),
        }
    }

    /// Upserts the FULL record (id, user_id, data, created_at, expires_at)
    /// via the ORM's `Session::objects().upsert(...)` — conflicts on the PK
    /// and updates the non-PK columns — rather than patching a single JSON key.
    ///
    /// Dispatches on `pool_dispatched()` to emit the correct placeholder
    /// syntax for SQLite (`?N`) vs Postgres (`$N`) and the right
    /// JSON upsert shape for each backend.
    async fn save(&self, token: &str, record: &SessionRecord) -> Result<String, SessionError> {
        use crate::Session;
        let stored_id = hash_token(token);
        // Route through the ORM's `upsert` (conflicts on the PK, updates every
        // non-PK column, dispatches SQLite/Postgres itself) instead of two
        // hand-rolled per-backend `INSERT … ON CONFLICT` blocks. CLAUDE.md:
        // plugins use the ORM, not raw dialect SQL — and a future upsert fix
        // now reaches the session table like every other write.
        Session::objects()
            .upsert(Session {
                id: stored_id,
                user_id: record.user_id.clone(),
                data: record.data.clone(),
                created_at: record.created_at,
                expires_at: record.expires_at,
            })
            .await?;
        // Return the raw token unchanged; the cookie value doesn't change
        // on a save (unlike a future CookieStore that encodes the record
        // into the cookie value).
        Ok(token.to_string())
    }

    /// Reproduces `destroy_session_by_hash` from `lib.rs`: hash the token
    /// then delete by the hash. Idempotent — no error if the row is absent.
    async fn destroy(&self, token: &str) -> Result<(), SessionError> {
        use crate::{Session, session};
        let stored_id = hash_token(token);
        Session::objects()
            .filter(session::ID.eq(&stored_id))
            .delete()
            .await?;
        Ok(())
    }

    /// Delete every session row owned by `user_id` (audit_2 H7). Anonymous
    /// sessions (`user_id IS NULL`) never match a `=` predicate, so they're
    /// left alone. Returns the number of rows removed.
    async fn destroy_user(&self, user_id: &str) -> Result<u64, SessionError> {
        use crate::{Session, session};
        let removed = Session::objects()
            .filter(session::USER_ID.eq(user_id))
            .delete()
            .await?;
        Ok(removed)
    }
}

// =========================================================================
// CachedDbStore — an in-process read cache in front of DbStore
// =========================================================================

/// Tuning for [`CachedDbStore`].
#[derive(Debug, Clone)]
pub struct SessionCacheConfig {
    /// How long a cached record may be served without re-reading the DB when
    /// nothing has invalidated it. This is the staleness bound for writes
    /// this process cannot see (another process, raw SQL) — keep it short
    /// when several processes share the session table.
    pub ttl: std::time::Duration,
    /// Soft cap on resident entries. On overflow the cache drops expired
    /// entries first, then an arbitrary slice — it is a cache, and thrashing
    /// under an oversized working set is acceptable and bounded.
    pub max_entries: usize,
}

impl Default for SessionCacheConfig {
    fn default() -> Self {
        Self {
            ttl: std::time::Duration::from_secs(300),
            max_entries: 65_536,
        }
    }
}

#[derive(Debug, Clone)]
struct CacheEntry {
    record: SessionRecord,
    /// `min(inserted + ttl, record.expires_at)` — a cached hit never serves
    /// past the session's own expiry.
    valid_until: DateTime<Utc>,
}

/// DB-backed sessions with an in-process read cache — the `cached_db` shape.
///
/// Reads that hit the cache issue **zero statements**; the framework's
/// statement-level measurements put the session read at one statement per
/// authenticated request, so this is where the read disappears. Correctness
/// rests on two mechanisms:
///
/// 1. **Signal-driven invalidation** — [`install_store`] subscribes to
///    `post_save:session` / `post_delete:session` (and the bulk variants)
///    and evicts the key for every ORM-mediated write, wherever it came
///    from. A logout, a rotation, an admin revocation all land here.
/// 2. **A TTL** — the bound on everything signals cannot see (another
///    process's writes, raw SQL).
///
/// The cache is per-process; a multi-process deployment invalidates across
/// processes only through the TTL (documented on `SessionStore::invalidate`).
#[derive(Debug)]
pub struct CachedDbStore {
    inner: DbStore,
    config: SessionCacheConfig,
    cache: std::sync::Mutex<std::collections::HashMap<String, CacheEntry>>,
}

impl CachedDbStore {
    pub fn new(config: SessionCacheConfig) -> Self {
        Self {
            inner: DbStore,
            config,
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The cache's resident entry count — for tests and diagnostics.
    pub fn len(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Lock helper: a poisoned cache mutex is recovered — every critical
    /// section here is a plain map operation, so the invariant is intact.
    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, CacheEntry>> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn insert(&self, stored_id: String, record: SessionRecord, now: DateTime<Utc>) {
        let ttl_deadline = now
            + chrono::Duration::from_std(self.config.ttl)
                .unwrap_or_else(|_| chrono::Duration::seconds(300));
        let valid_until = ttl_deadline.min(record.expires_at);
        let mut cache = self.lock();
        if cache.len() >= self.config.max_entries {
            cache.retain(|_, e| e.valid_until > now);
            if cache.len() >= self.config.max_entries {
                let excess = cache.len() / 8 + 1;
                let victims: Vec<String> = cache.keys().take(excess).cloned().collect();
                for k in victims {
                    cache.remove(&k);
                }
            }
        }
        cache.insert(
            stored_id,
            CacheEntry {
                record,
                valid_until,
            },
        );
    }
}

impl Default for CachedDbStore {
    fn default() -> Self {
        Self::new(SessionCacheConfig::default())
    }
}

#[async_trait::async_trait]
impl SessionStore for CachedDbStore {
    async fn load(&self, token: &str) -> Result<Option<SessionRecord>, SessionError> {
        let stored_id = hash_token(token);
        let now = Utc::now();
        // Cache probe: clone out, never hold the lock across an await.
        {
            let mut cache = self.lock();
            match cache.get(&stored_id) {
                Some(e) if e.valid_until > now => return Ok(Some(e.record.clone())),
                Some(_) => {
                    cache.remove(&stored_id);
                }
                None => {}
            }
        }
        let loaded = self.inner.load(token).await?;
        if let Some(record) = &loaded {
            self.insert(stored_id, record.clone(), now);
        }
        Ok(loaded)
    }

    async fn save(&self, token: &str, record: &SessionRecord) -> Result<String, SessionError> {
        let out = self.inner.save(token, record).await?;
        // Write-through: the row now holds exactly this record. (The inner
        // save also emits `post_save:session`, which evicts via the signal
        // subscription — insert AFTER, so the freshest state wins.)
        self.insert(hash_token(token), record.clone(), Utc::now());
        Ok(out)
    }

    async fn destroy(&self, token: &str) -> Result<(), SessionError> {
        self.inner.destroy(token).await?;
        self.lock().remove(&hash_token(token));
        Ok(())
    }

    async fn destroy_user(&self, user_id: &str) -> Result<u64, SessionError> {
        let removed = self.inner.destroy_user(user_id).await?;
        // Evict this user's entries locally; the per-row signal fires too,
        // but a bulk delete path that skips signals must not leave the cache
        // serving revoked sessions.
        self.lock()
            .retain(|_, e| e.record.user_id.as_deref() != Some(user_id));
        Ok(removed)
    }

    fn invalidate(&self, stored_id: &str) {
        self.lock().remove(stored_id);
    }
}

// =========================================================================
// Ambient install — mirrors how umbral-core installs the ambient DB pool.
// =========================================================================

static STORE: OnceLock<Arc<dyn SessionStore>> = OnceLock::new();

/// Install the ambient `SessionStore`. Idempotent: if a store is already
/// installed, a warning is emitted and the first store is kept (same
/// behaviour as umbral-core's pool install).
///
/// Call this during `App::build()` or in a plugin's `on_ready()` hook.
/// If no store is installed, [`active_store`] falls back to a default
/// [`DbStore`].
pub fn install_store(store: Arc<dyn SessionStore>) {
    if STORE.set(store).is_err() {
        tracing::warn!(
            "umbral-sessions: install_store called more than once; \
             keeping the first installed store"
        );
        return;
    }
    // The invalidation contract: whenever a session row changes, the
    // installed store is told, so a cache cannot serve a revoked or rotated
    // session. This rides the ORM's own signals — the same events an audit
    // subscriber sees — which is what makes it independent of the write's
    // route: the session helpers in `lib.rs` mix store calls and direct ORM
    // writes, and the subscription covers both. `upsert` (the store's own
    // save path) gained its emits alongside this feature; without them this
    // contract would have had a hole exactly where sessions are written.
    let table = <crate::Session as umbral::orm::Model>::TABLE;
    let evict_instance = |payload: &serde_json::Value| {
        if let Some(id) = payload
            .get("instance")
            .and_then(|i| i.get("id"))
            .and_then(|v| v.as_str())
        {
            active_store().invalidate(id);
        }
    };
    let evict_ids = |payload: &serde_json::Value| {
        if let Some(ids) = payload.get("ids").and_then(|v| v.as_array()) {
            for id in ids {
                if let Some(id) = id.as_str() {
                    active_store().invalidate(id);
                }
            }
        }
    };
    let evict_instance = std::sync::Arc::new(evict_instance);
    let evict_ids = std::sync::Arc::new(evict_ids);
    for name in ["post_save", "post_delete"] {
        let evict = evict_instance.clone();
        umbral::signals::subscribe(&format!("{name}:{table}"), move |payload| evict(payload));
    }
    for name in ["bulk_post_save", "bulk_post_delete"] {
        let evict = evict_ids.clone();
        umbral::signals::subscribe(&format!("{name}:{table}"), move |payload| evict(payload));
    }
}

/// Return the installed `SessionStore`, or a default [`DbStore`] if none
/// has been installed. The default is constructed on each call (it holds
/// no state) and uses the ambient ORM pool.
pub fn active_store() -> Arc<dyn SessionStore> {
    STORE.get().cloned().unwrap_or_else(|| Arc::new(DbStore))
}
