//! `SessionStore` trait + `DbStore` implementation + ambient install.
//!
//! ## Design
//!
//! `SessionStore` is the pluggable storage abstraction for umbral sessions.
//! `DbStore` is the default implementation: it reproduces the SQL logic from
//! `read_session` / `upsert_session_data_key` / `destroy_session_by_hash` in
//! `lib.rs` exactly, but operates on the full `SessionRecord` (id, user_id,
//! data, created_at, expires_at) rather than per-key data.
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

    /// Reproduces the INSERT … ON CONFLICT shape from `upsert_session_data_key`
    /// in `lib.rs`, but writes the FULL record (id, user_id, data,
    /// created_at, expires_at) rather than patching a single JSON key.
    ///
    /// Dispatches on `pool_dispatched()` to emit the correct placeholder
    /// syntax for SQLite (`?N`) vs Postgres (`$N`) and the right
    /// JSON upsert shape for each backend.
    async fn save(&self, token: &str, record: &SessionRecord) -> Result<String, SessionError> {
        let stored_id = hash_token(token);
        match umbral::db::pool_dispatched() {
            umbral::db::DbPool::Sqlite(pool) => {
                sqlx::query(
                    r#"
                    INSERT INTO session (id, user_id, data, created_at, expires_at)
                    VALUES (?1, ?2, ?3, ?4, ?5)
                    ON CONFLICT(id) DO UPDATE SET
                        user_id    = excluded.user_id,
                        data       = excluded.data,
                        expires_at = excluded.expires_at
                    "#,
                )
                .bind(&stored_id)
                .bind(&record.user_id)
                .bind(&record.data)
                .bind(record.created_at)
                .bind(record.expires_at)
                .execute(pool)
                .await?;
            }
            umbral::db::DbPool::Postgres(pool) => {
                sqlx::query(
                    r#"
                    INSERT INTO session (id, user_id, data, created_at, expires_at)
                    VALUES ($1, $2, $3, $4, $5)
                    ON CONFLICT (id) DO UPDATE SET
                        user_id    = EXCLUDED.user_id,
                        data       = EXCLUDED.data,
                        expires_at = EXCLUDED.expires_at
                    "#,
                )
                .bind(&stored_id)
                .bind(&record.user_id)
                .bind(&record.data)
                .bind(record.created_at)
                .bind(record.expires_at)
                .execute(pool)
                .await?;
            }
        }
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
    }
}

/// Return the installed `SessionStore`, or a default [`DbStore`] if none
/// has been installed. The default is constructed on each call (it holds
/// no state) and uses the ambient ORM pool.
pub fn active_store() -> Arc<dyn SessionStore> {
    STORE
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(DbStore))
}
