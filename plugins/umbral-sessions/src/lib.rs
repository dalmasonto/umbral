//! umbral-sessions — DB-backed session storage for umbral.
//!
//! Cookie-shaped sessions linked to umbral-auth's `AuthUser`. The
//! `Session` model lives in the `session` table; one row per
//! browser session, identified by a random UUID written to the
//! `umbral_session` cookie.
//!
//! ## Surface
//!
//! - `Session` model (id, user_id, data, created_at, expires_at)
//! - `SessionsPlugin` registers the model AND auto-applies
//!   `session_layer`. A session row is created **lazily on first
//!   write**: a cookie-less request that never writes
//!   the session (favicon, CSS, an anonymous read-only page) leaves no
//!   row and no cookie. The first write — anonymous or authed —
//!   materialises exactly one row and emits the `Set-Cookie`. Opt out
//!   of the auto-layer via `SessionsPlugin::default().without_auto_layer()`.
//! - `create_session(user_id, ttl)` -> new id (write to Set-Cookie).
//!   `user_id` is `Option<i64>`: `None` is anonymous, `Some(id)` is
//!   authenticated.
//! - `read_session(id)` -> `Option<Session>` (filters out expired)
//! - `destroy_session(id)` -> Delete
//! - `cookie_from_headers(headers)` -> extract session id from
//!   the request's `Cookie` header
//! - `set_cookie_header(id)` -> the Set-Cookie string for a login
//!   response. `Secure`, `HttpOnly`, `SameSite=Lax` by default,
//!   matching the security-defaults outline.
//! - `current_user(headers)` -> `Option<AuthUser>` — the one-call
//!   helper handlers use. Looks up the session via the cookie and
//!   hydrates the user via umbral-auth.
//!
//! Custom `data` per session (cart contents, flash messages, etc.)
//! is stored as a JSON string in the `data` column. Helpers
//! `get_data` / `set_data` round-trip through serde.
//!
//! ## v1 scope
//!
//! - One session backend: DB rows via the ambient pool.
//! - Cookie-only signed-store alternative is deferred until a real
//!   low-traffic case asks for it.
//! - No session middleware: handlers call `current_user(&headers)`
//!   explicitly. Adding `Plugin::middleware()` is the M7 deferral;
//!   sessions doesn't need it to be useful, and the extractor
//!   pattern keeps the auth checks visible in the handler signature.
//! - Periodic cleanup of expired rows isn't automated — a future
//!   `umbral-tasks` periodic job, or a `clearsessions` management
//!   command, lands when one or the other is real.

pub mod cookie_store;
#[cfg(feature = "redis")]
pub mod redis_store;
pub mod request_session;
pub mod store;

pub use cookie_store::CookieStore;
#[cfg(feature = "redis")]
pub use redis_store::RedisStore;
pub use request_session::{RequestSession, current, current_mut};
pub use store::{DbStore, SessionRecord, SessionStore, active_store, install_store};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use umbral::prelude::*;
use umbral::web::{HeaderMap, header};
use uuid::Uuid;

/// Ambient flag: true when the operator has called `.sliding_expiry()` on
/// `SessionsPlugin`. Set once in `on_ready`; read by `session_layer` on
/// every request. Default OFF — no extra write per request unless opted in.
static SLIDING_EXPIRY_ENABLED: OnceLock<bool> = OnceLock::new();

/// Ambient absolute session-lifetime cap in seconds (audit_2 plugin-sessions
/// #5). `0` (the sealed default when the builder isn't called) means "no cap".
/// Sealed from `SessionsPlugin::max_session_age` at `on_ready`; read by
/// [`read_session`] to expire sessions older than this regardless of sliding
/// expiry.
static MAX_SESSION_AGE_SECONDS: OnceLock<i64> = OnceLock::new();

/// The configured absolute session-age cap, or `None` when unset/`<= 0`.
fn max_session_age() -> Option<i64> {
    MAX_SESSION_AGE_SECONDS.get().copied().filter(|&n| n > 0)
}

/// Default cookie name. Users override via `set_cookie_header_named`
/// when they need a project-specific name.
pub const COOKIE_NAME: &str = "umbral_session";

/// Default session TTL: 14 days.
pub const DEFAULT_TTL_SECONDS: i64 = 14 * 24 * 60 * 60;

/// The session row.
///
/// `id` is a random UUID written to the client cookie. `user_id` is
/// the user this session belongs to (nullable so anonymous sessions
/// are possible). `data` is a free-form JSON string the application
/// stores per-session.
///
/// ## `user_id` is polymorphic
///
/// Gap #59: the column stores the user PK as a string regardless of
/// the active `UserModel`'s PK type. `AuthUser` (i64-keyed) writes
/// the integer's `Display` form; a custom user model with `Uuid` or
/// `String` PK writes that type's `Display` form. The built-in
/// helpers (`current_user`, `login_with_request`) are still
/// AuthUser-specific and round-trip through `i64 ↔ String`; callers
/// using a custom user model write their own resolver against the
/// `user_id` text and parse it however their PK type expects.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct Session {
    pub id: String,
    /// Indexed: `destroy_user` (audit_2 H7) and `clearsessions` filter on
    /// `user_id`, so at scale an unindexed column would force a full scan on
    /// every password-reset revocation.
    #[umbral(index)]
    pub user_id: Option<String>,
    pub data: String,
    pub created_at: DateTime<Utc>,
    /// Indexed: `clearsessions` deletes on `expires_at < now()` (audit_2
    /// plugin-sessions #4). At scale an unindexed column full-scans the whole
    /// session table on every sweep, contending with per-request PK reads.
    #[umbral(index)]
    pub expires_at: DateTime<Utc>,
}

/// The plugin. Registers the `Session` model and (by default)
/// auto-applies [`session_layer`], which creates a session row
/// lazily on the first write (see `session_layer`).
/// Opt out with [`Self::without_auto_layer`] if you want to control
/// session creation by hand (rare).
///
/// ## Sliding expiry (refresh the TTL on every request)
///
/// By default a session's `expires_at` is fixed at creation time:
/// a session started at noon on Monday with a 14-day TTL expires at
/// noon on Monday two weeks later, regardless of how many requests the
/// user made in between. Call `.sliding_expiry()` to enable the
/// rolling-window behaviour: each request that finds a live session
/// extends `expires_at` to `now + DEFAULT_TTL_SECONDS`, so an
/// actively-used session never hard-expires mid-use.
///
/// Default is OFF so the simpler fixed-expiry path stays cost-free
/// (zero extra writes per request). Toggle globally:
///
/// ```ignore
/// App::builder()
///     .plugin(SessionsPlugin::default().sliding_expiry())
///     .build()
/// ```
///
/// ## Custom session store
///
/// By default sessions are persisted to the database via [`DbStore`].
/// Supply any type that implements [`SessionStore`] to swap the backend:
///
/// ```ignore
/// App::builder()
///     .plugin(SessionsPlugin::default().store(MyRedisStore::new(...)))
///     .build()
/// ```
///
/// The store is installed into the ambient [`install_store`] slot during
/// [`Plugin::on_ready`] so every subsequent call to [`active_store`]
/// returns it. The install is idempotent — if two plugins (or two test
/// runs in the same process) call it, the first wins and a warning is
/// logged.
pub struct SessionsPlugin {
    auto_layer: bool,
    sliding_expiry: bool,
    /// Absolute session lifetime cap in seconds (audit_2 plugin-sessions #5).
    /// `None` = no cap (default). When set, a session older than this from its
    /// `created_at` is rejected and destroyed even if `expires_at` (which
    /// sliding expiry keeps bumping) is still in the future.
    max_age_seconds: Option<i64>,
    store: std::sync::Arc<dyn SessionStore>,
}

impl std::fmt::Debug for SessionsPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionsPlugin")
            .field("auto_layer", &self.auto_layer)
            .field("sliding_expiry", &self.sliding_expiry)
            .field("max_age_seconds", &self.max_age_seconds)
            .field("store", &self.store)
            .finish()
    }
}

impl Clone for SessionsPlugin {
    fn clone(&self) -> Self {
        Self {
            auto_layer: self.auto_layer,
            sliding_expiry: self.sliding_expiry,
            max_age_seconds: self.max_age_seconds,
            store: self.store.clone(),
        }
    }
}

impl Default for SessionsPlugin {
    fn default() -> Self {
        Self {
            auto_layer: true,
            sliding_expiry: false,
            max_age_seconds: None,
            store: std::sync::Arc::new(DbStore::default()),
        }
    }
}

impl SessionsPlugin {
    /// Disable auto-application of [`session_layer`]. Use when you
    /// want to scope session creation to a sub-router (e.g. apply
    /// the layer manually only to `/app/*` so unauthed REST routes
    /// don't get a session DB row on every health check).
    pub fn without_auto_layer(mut self) -> Self {
        self.auto_layer = false;
        self
    }

    /// Enable sliding (rolling) session expiry: each request that
    /// resolves a live session extends `expires_at` to
    /// `now + DEFAULT_TTL_SECONDS`. Off by default — the default
    /// fixed-expiry path incurs no extra write per request.
    ///
    /// Refresh the session TTL on every request.
    pub fn sliding_expiry(mut self) -> Self {
        self.sliding_expiry = true;
        self
    }

    /// Set an ABSOLUTE session lifetime cap in seconds (audit_2 plugin-sessions
    /// #5). Off by default. Without it, [`sliding_expiry`](Self::sliding_expiry)
    /// has no upper bound: a session (or a stolen cookie) used at least once per
    /// TTL window never expires. With a cap, [`read_session`] rejects and
    /// destroys any session older than `secs` from its `created_at`, no matter
    /// how far sliding expiry has pushed `expires_at`. A `0` or negative value
    /// disables the cap.
    ///
    /// ```ignore
    /// // Force re-authentication at least every 7 days even with sliding expiry.
    /// SessionsPlugin::default().sliding_expiry().max_session_age(7 * 24 * 60 * 60)
    /// ```
    pub fn max_session_age(mut self, secs: i64) -> Self {
        self.max_age_seconds = Some(secs);
        self
    }

    /// Override the session storage back-end. The supplied store is
    /// installed via [`install_store`] during [`Plugin::on_ready`].
    ///
    /// The default is [`DbStore`] (DB-backed, reproduces existing behaviour).
    /// Pass any type that implements [`SessionStore`] + `'static` to swap it:
    ///
    /// ```ignore
    /// SessionsPlugin::default().store(MyCustomStore::new())
    /// ```
    pub fn store(mut self, store: impl SessionStore + 'static) -> Self {
        self.store = std::sync::Arc::new(store);
        self
    }
}

impl Plugin for SessionsPlugin {
    fn name(&self) -> &'static str {
        "sessions"
    }

    fn dependencies(&self) -> &'static [&'static str] {
        // No hard dep on auth at the trait level. The current_user
        // helper does call umbral_auth, but the Session model itself
        // is independent — anonymous sessions are valid.
        &[]
    }

    fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
        vec![umbral::migrate::ModelMeta::for_::<Session>()]
    }

    fn wrap_router(&self, router: umbral::web::Router) -> umbral::web::Router {
        let mut router = router;
        if self.auto_layer {
            router = router.layer(axum::middleware::from_fn(session_layer));
        }
        router
    }

    fn on_ready(
        &self,
        ctx: &umbral::plugin::AppContext,
    ) -> Result<(), umbral::plugin::PluginError> {
        // Hard-fail boot when the configured store's security depends on the
        // ambient `secret_key` (a secret-derived CookieStore) but that secret
        // is empty or still the insecure dev default, in production. Such a
        // store seals every session cookie under a key an attacker can
        // reproduce — a full auth bypass. Fail closed at boot (loud, before the
        // first request) instead of serving forgeable sessions. Non-prod only
        // warns; CookieStore::resolve_ambient_key emits that warning lazily.
        if self.store.requires_ambient_secret()
            && matches!(ctx.settings.environment, umbral::Environment::Prod)
        {
            let secret = ctx.settings.secret_key.trim();
            // Kept in sync with umbral-core's `default_secret_key()` /
            // `check.rs::INSECURE_DEV_SECRET_KEY`.
            const INSECURE_DEV_SECRET_KEY: &str = "umbral-insecure-dev-key-change-me";
            if secret.is_empty() || secret == INSECURE_DEV_SECRET_KEY {
                let which = if secret.is_empty() {
                    "empty"
                } else {
                    "the insecure dev default"
                };
                return Err(format!(
                    "umbral-sessions: the configured session store (a secret-derived \
                     CookieStore) is stateless and encrypts/authenticates every session \
                     cookie with a key derived from `secret_key`, but `secret_key` is {which} \
                     in production. Such cookies are TRIVIALLY FORGEABLE (an attacker can mint \
                     a session for any user — full auth bypass). Set a real `secret_key` via \
                     umbral.toml or UMBRAL_SECRET_KEY before deploying, or pin an explicit key \
                     with CookieStore::with_secret(...)."
                )
                .into());
            }
        }

        // Seal the sliding-expiry flag into the ambient OnceLock so
        // session_layer can read it without carrying a reference to Self.
        // `set` is a no-op if another test already initialised the cell;
        // production binaries build once so the first (and only) call wins.
        let _ = SLIDING_EXPIRY_ENABLED.set(self.sliding_expiry);
        // Seal the absolute session-age cap (audit_2 plugin-sessions #5). `0`
        // means "no cap"; first boot wins (idempotent), matching the
        // sliding-expiry / ambient-store contract.
        let _ = MAX_SESSION_AGE_SECONDS.set(self.max_age_seconds.unwrap_or(0));

        // Install the configured store so `active_store()` returns it.
        // Idempotent: if a store was already installed (e.g. a second plugin
        // or a test that boots twice in the same process), `install_store`
        // warns and keeps the first. Same "first wins" contract as the
        // ambient pool.
        install_store(self.store.clone());
        Ok(())
    }

    fn commands(&self) -> Vec<Box<dyn umbral::cli::PluginCommand>> {
        vec![Box::new(ClearSessionsCommand)]
    }
}

// =========================================================================
// `clearsessions` management command.
//
// Deletes all session rows whose `expires_at < now()`. Rows accumulate
// forever without this; the lazy-cleanup in `read_session` only fires
// when an expired session is actively looked up. Run this periodically
// (cron / umbral-tasks) or on demand to keep the table lean.
// =========================================================================

struct ClearSessionsCommand;

#[async_trait::async_trait]
impl umbral::cli::PluginCommand for ClearSessionsCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("clearsessions")
            .about("Delete all expired session rows from the database.")
    }

    async fn run(&self, _matches: &clap::ArgMatches) -> Result<(), umbral::cli::CliError> {
        let now = Utc::now();
        let deleted = Session::objects()
            .filter(session::EXPIRES_AT.lt(now))
            .delete()
            .await
            .map_err(|e| format!("clearsessions: {e:?}"))?;
        println!("Deleted {deleted} expired session(s).");
        Ok(())
    }
}

/// Errors the helpers produce.
#[derive(Debug)]
pub enum SessionError {
    /// sqlx error executing one of the helper queries.
    Sqlx(sqlx::Error),
    /// `data` round-tripping through serde failed.
    Json(serde_json::Error),
    /// ORM write error — `create`, `update_values`, etc.
    Write(umbral::orm::write::WriteError),
    /// The encoded session-in-cookie blob exceeded the ~4 KB browser cookie
    /// limit. Raised by `CookieStore::save` so an oversized session fails
    /// loudly rather than silently producing a cookie the browser drops.
    /// Carries the encoded byte length that tripped the limit.
    CookieTooLarge(usize),
    /// The active store can't revoke by user id (audit_2 H7). A stateless
    /// [`crate::CookieStore`] seals the session into the cookie itself, so
    /// there's no server-side record to delete — "log out everywhere" is
    /// impossible without token rotation or a denylist. Returned by
    /// `SessionStore::destroy_user` so the caller (e.g. password-reset
    /// revocation) surfaces the gap LOUDLY instead of silently no-op'ing and
    /// leaving stolen cookies live.
    RevocationUnsupported,
    /// A Redis-level error (connection, protocol, server). Only produced by
    /// [`RedisStore`] when the `redis` feature is active.
    #[cfg(feature = "redis")]
    Redis(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Sqlx(e) => write!(f, "umbral-sessions: sqlx: {e}"),
            SessionError::Json(e) => write!(f, "umbral-sessions: json: {e}"),
            SessionError::Write(e) => write!(f, "umbral-sessions: write: {e:?}"),
            SessionError::CookieTooLarge(n) => write!(
                f,
                "umbral-sessions: encoded session cookie is {n} bytes, over the ~4 KB browser \
                 limit; store less in the session or switch to a server-side store"
            ),
            SessionError::RevocationUnsupported => write!(
                f,
                "umbral-sessions: the active session store cannot revoke by user id (a stateless \
                 CookieStore has no server-side session to delete). 'Log out everywhere' needs a \
                 server-side store (DbStore/RedisStore) or short-TTL cookies with rotation"
            ),
            #[cfg(feature = "redis")]
            SessionError::Redis(e) => write!(f, "umbral-sessions: redis: {e}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<sqlx::Error> for SessionError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl From<serde_json::Error> for SessionError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<umbral::orm::write::WriteError> for SessionError {
    fn from(e: umbral::orm::write::WriteError) -> Self {
        Self::Write(e)
    }
}

// =========================================================================
// Public helpers.
// =========================================================================

/// SHA-256 hash the raw token, hex-encoded. The DB column holds this
/// digest; the raw token only lives in the cookie. Constant-time
/// comparison falls out for free — sqlite's `=` on equal-length
/// hex digests doesn't leak meaningful timing across hash boundaries
/// (the hash function pre-image collision is the actual barrier).
///
/// Plain SHA-256 is sufficient here: the input is already a
/// 122-bit-entropy random token, not a low-entropy password, so the
/// usual "use argon2 / bcrypt" advice doesn't apply. The whole point
/// is that an attacker who exfiltrates the DB sees `hex(sha256(t))`
/// instead of `t` and can't replay sessions.
///
/// Implementation lives in `store.rs` (pub(crate)) and is re-used here
/// to avoid maintaining two copies of the same hash function.
fn hash_token(raw: &str) -> String {
    store::hash_token(raw)
}

/// Create a new session row. Returns the **raw** session token
/// which the caller writes into a Set-Cookie header via
/// [`set_cookie_header`]. The DB row stores `sha256(token)` so a
/// DB leak doesn't surrender live sessions.
///
/// `user_id` is the user's primary key serialised as a string
/// (gap #59). `AuthUser`'s i64 PK round-trips through `to_string()` /
/// `parse::<i64>()`; a `Uuid`-keyed custom user model uses
/// `Display` / `FromStr` on `Uuid`, etc. Passing `None` creates an
/// anonymous session.
///
/// `ttl` controls the row's `expires_at`. Pass `None` to use
/// [`DEFAULT_TTL_SECONDS`] (14 days).
pub async fn create_session(
    user_id: Option<String>,
    ttl: Option<Duration>,
) -> Result<String, SessionError> {
    let raw_token = Uuid::new_v4().to_string();
    let stored_id = hash_token(&raw_token);
    let now = Utc::now();
    let expires_at = now + ttl.unwrap_or_else(|| Duration::seconds(DEFAULT_TTL_SECONDS));
    Session::objects()
        .create(Session {
            id: stored_id,
            user_id,
            data: "{}".to_string(),
            created_at: now,
            expires_at,
        })
        .await?;
    Ok(raw_token)
}

/// Look up a session by its raw token (typically from the cookie).
/// The token is hashed before the lookup so the column the row's
/// indexed on can be looked up in O(1) without ever putting the raw
/// value in DB query memory. Returns `None` if the row doesn't exist
/// OR if it's expired (in which case the row is also deleted — lazy
/// cleanup, no scheduled job needed).
pub async fn read_session(token: &str) -> Result<Option<Session>, SessionError> {
    // audit_2 H7: read from the installed store, not always the SQL table, so a
    // RedisStore/CookieStore session resolves. The store hashes the token and
    // applies lazy expiry (deleting the stale record) itself. Rebuild the
    // `Session` model shape from the store's `SessionRecord` — `id` is the
    // stored token hash, the same identity the DB row's PK carried.
    match active_store().load(token).await? {
        None => Ok(None),
        Some(rec) => {
            // audit_2 plugin-sessions #5: enforce the absolute lifetime cap.
            // The store already applied sliding/`expires_at` expiry; this is the
            // separate idle-vs-absolute bound. A session older than the cap from
            // its `created_at` is dead no matter how far sliding expiry pushed
            // `expires_at` — destroy it (best-effort) and resolve anonymous.
            if let Some(max_age) = max_session_age() {
                let age = Utc::now() - rec.created_at;
                if age > Duration::seconds(max_age) {
                    if let Err(e) = active_store().destroy(token).await {
                        tracing::warn!(
                            "umbral-sessions: failed to destroy a session past its \
                             absolute max age: {e}"
                        );
                    }
                    return Ok(None);
                }
            }
            Ok(Some(Session {
                id: hash_token(token),
                user_id: rec.user_id,
                data: rec.data,
                created_at: rec.created_at,
                expires_at: rec.expires_at,
            }))
        }
    }
}

/// Delete every session row owned by `user_id_str` — the "log out
/// everywhere" primitive. Used after a password reset or change so
/// stolen session cookies stop working immediately. Anonymous sessions
/// (`user_id IS NULL`) are never matched because SQL's NULL semantics
/// exclude them from `=` comparisons. Returns the number of rows
/// removed.
///
/// `user_id_str` is the user PK serialised via `Display` — the same
/// string that was passed to [`create_session`] at login time. For an
/// `AuthUser` (i64 PK) call `revoke_user_sessions(&user.id.to_string())`.
pub async fn revoke_user_sessions(user_id_str: &str) -> Result<u64, SessionError> {
    // audit_2 H7: route through the installed store, not the raw `session`
    // table. Under RedisStore/CookieStore the old direct-SQL delete hit an
    // empty table and left the real sessions live — a password reset then
    // didn't invalidate a stolen session. `destroy_user` deletes from wherever
    // the sessions actually live (DB rows / Redis keys), or returns
    // `RevocationUnsupported` for a stateless CookieStore so the caller logs it.
    active_store().destroy_user(user_id_str).await
}

/// Delete a session row by its raw token. Used by logout. Idempotent:
/// a non-existent token is treated as success. The token is hashed
/// before the DELETE so the same hash-on-write/hash-on-read invariant
/// holds for destruction too.
pub async fn destroy_session(token: &str) -> Result<(), SessionError> {
    // audit_2 H7: logout must delete from the installed store (DB row / Redis
    // key / cookie), not always the SQL table. The store hashes the token
    // itself. Idempotent on every backend.
    active_store().destroy(token).await
}

/// Parse the `Cookie` header and return the umbral session id, if
/// present. The handler that wants to know who's calling reads this
/// then `read_session` then `umbral_auth::AuthUser::objects().filter(...)`.
/// Or just call [`current_user`] which does all three steps.
pub fn cookie_from_headers(headers: &HeaderMap) -> Option<String> {
    cookie_from_headers_named(headers, COOKIE_NAME)
}

/// Same as [`cookie_from_headers`] but with a custom cookie name.
pub fn cookie_from_headers_named(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(&format!("{name}=")) {
            return Some(value.to_string());
        }
    }
    None
}

/// Build the `Set-Cookie` header value for a newly-issued session.
/// `Secure`, `HttpOnly`, `SameSite=Lax`, `Path=/` by default.
///
/// `max_age` controls the cookie's lifetime in seconds; pass `None`
/// to default to [`DEFAULT_TTL_SECONDS`].
pub fn set_cookie_header(id: &str, max_age: Option<i64>) -> String {
    set_cookie_header_named(COOKIE_NAME, id, max_age)
}

/// The `Secure; ` cookie attribute — present in every environment
/// except `Dev`. A `Secure` cookie is only sent over HTTPS, which is
/// correct (and non-negotiable) in production but breaks cookie-based
/// auth over plain `http://` in local development: the browser silently
/// drops it, so every request resolves anonymous. Gating it on the
/// environment mirrors the framework's "HSTS off for local http dev"
/// posture. Defaults to `Secure` when settings aren't resolved yet
/// (secure-by-default).
fn secure_attr() -> &'static str {
    match umbral::settings::get_opt() {
        Some(s) if matches!(s.environment, umbral::Environment::Dev) => "",
        _ => "Secure; ",
    }
}

/// [`set_cookie_header`] with an explicit cookie name. `Secure` is set
/// in every environment except `Dev` (see [`secure_attr`]).
pub fn set_cookie_header_named(name: &str, id: &str, max_age: Option<i64>) -> String {
    let max_age = max_age.unwrap_or(DEFAULT_TTL_SECONDS);
    format!(
        "{name}={id}; Path=/; HttpOnly; {secure}SameSite=Lax; Max-Age={max_age}",
        secure = secure_attr()
    )
}

/// Build the Set-Cookie header that deletes the session cookie.
/// Used on logout: the client sees an immediately-expired cookie and
/// drops the local value.
pub fn clear_cookie_header() -> String {
    clear_cookie_header_named(COOKIE_NAME)
}

/// [`clear_cookie_header`] with an explicit cookie name.
pub fn clear_cookie_header_named(name: &str) -> String {
    format!(
        "{name}=; Path=/; HttpOnly; {secure}SameSite=Lax; Max-Age=0",
        secure = secure_attr()
    )
}

/// Read the request's session cookie and return the active `Session`
/// row, if any. Returns `None` for: no cookie, expired session, or a
/// session that was revoked / destroyed.
///
/// User-agnostic — the row's `user_id` field is a `String` (the user
/// PK serialised via `Display`), with no knowledge of which user
/// model owns the value. For an `AuthUser`-shaped wrapper that
/// hydrates the row, see `umbral_auth::current_user` (lives there so
/// `umbral-sessions` stays free of any user-model dependency).
pub async fn current_session(headers: &HeaderMap) -> Result<Option<Session>, SessionError> {
    let cookie_id = cookie_from_headers(headers);

    // In-request fast path: serve a `Session` view from the in-memory
    // record (no DB) when a request scope is active AND the request's
    // cookie addresses the scoped session (or there's no cookie — a fresh
    // request reading its own freshly-written session). A handler that
    // wrote the session this request sees its own writes back without a
    // re-query. A *different* cookie value falls through to the DB read so
    // an explicit cross-session lookup still works.
    if let Some(view) = current(|s| {
        let same_session = match &cookie_id {
            Some(c) => c == s.token(),
            None => true,
        };
        if same_session {
            // `Some(None)` = scoped + matched but no row yet (anonymous
            // fresh request) -> report absence WITHOUT a DB read.
            Some(session_view_from_record(s.token(), s.record()))
        } else {
            None
        }
    })
    .flatten()
    {
        return Ok(view);
    }

    // Fallback: today's cookie -> read_session path (out-of-request, or a
    // cookie that doesn't match the scoped session).
    let Some(id) = cookie_id else {
        return Ok(None);
    };
    read_session(&id).await
}

/// Build a `Session` row view from the in-memory request-scoped record.
/// Returns `None` when the record hasn't materialised yet (an anonymous
/// fresh request that never wrote the session) so callers see the same
/// "no session" answer they'd get from the DB. The `id` is the hashed
/// token, matching how the row is keyed at rest.
fn session_view_from_record(token: &str, record: Option<&SessionRecord>) -> Option<Session> {
    record.map(|r| Session {
        id: hash_token(token),
        user_id: r.user_id.clone(),
        data: r.data.clone(),
        created_at: r.created_at,
        expires_at: r.expires_at,
    })
}

/// Read the request's session cookie and return the stringified user
/// id stashed on the active session, if any. The result is the value
/// `umbral_auth::login_with_request` (or any other login helper)
/// stored via [`create_session`]; for an `AuthUser`-shaped i64 PK,
/// `.parse::<i64>().ok()` round-trips it back.
///
/// Returns `None` on any of: no cookie, expired session, anonymous
/// session (`user_id IS NULL`). The user-id parse failure mode is
/// reserved for the caller, since each user model knows its own PK
/// shape.
pub async fn current_user_id_str(headers: &HeaderMap) -> Result<Option<String>, SessionError> {
    let cookie_id = cookie_from_headers(headers);

    // In-request fast path: read `user_id` straight off the in-memory
    // record (no DB). This is the per-request hot path the benchmark hit —
    // resolving the logged-in user used to cost a `read_session` query on
    // every authed request. Guard on the cookie matching the scoped token
    // so a cross-session lookup still goes to the DB.
    //
    // The outer `Option` distinguishes "handled in memory" (`Some`, even
    // when the inner user id is `None` for an anonymous session) from "not
    // scoped / different cookie -> fall through to the DB" (`None`).
    let handled: Option<Option<String>> = current(|s| {
        let same_session = match &cookie_id {
            Some(c) => c == s.token(),
            None => true,
        };
        if same_session {
            Some(s.user_id().map(|u| u.to_string()))
        } else {
            None
        }
    })
    .flatten();
    if let Some(uid) = handled {
        return Ok(uid);
    }

    // Fallback: out-of-request, or a cookie that doesn't match the scoped
    // session — today's read_session path.
    Ok(current_session(headers).await?.and_then(|s| s.user_id))
}

// =========================================================================
// Per-session data round-trip. The `data` column stores a JSON object
// the application owns. `get` reads one key; `set` writes one key.
// =========================================================================

/// Read a typed value from a session's `data` map by key. Returns
/// `Ok(None)` if the key isn't set; returns an error only if the
/// stored JSON is malformed.
pub fn get_data<T: serde::de::DeserializeOwned>(
    session: &Session,
    key: &str,
) -> Result<Option<T>, SessionError> {
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&session.data)?;
    match map.get(key) {
        None => Ok(None),
        Some(value) => Ok(Some(serde_json::from_value(value.clone())?)),
    }
}

/// Write a typed value into a session's `data` map by key.
///
/// ## In-request fast path (no mid-request DB write)
///
/// When called inside a request whose [`session_layer`]-scoped session
/// matches `session_token`, this mutates the in-memory record via
/// `current_mut(|s| s.set_raw(...))` and marks it dirty. The actual DB
/// write happens once, at layer exit (`store.save`). This keeps a
/// handler that writes several keys to a single persist at the end of
/// the request instead of one upsert per key.
///
/// ## Fallback (out-of-request / token mismatch)
///
/// For a background task, a different session's token, or any caller
/// outside a request scope, falls back to the direct upsert
/// (`upsert_session_data_key`) so the row is written immediately — the
/// original direct-write semantics, unchanged.
///
/// `session_token` is the raw token from the cookie; hashed before the
/// WHERE clause like every other session-lookup path.
pub async fn set_data<T: Serialize>(
    session_token: &str,
    key: &str,
    value: &T,
) -> Result<(), SessionError> {
    let json_value = serde_json::to_value(value)?;

    // In-request fast path: if the live request-scoped session is the one
    // this token addresses, mutate the in-memory record and let the layer
    // persist it at exit. `current_mut` returns `None` outside a request
    // scope (background task), so we fall through to the direct write.
    let handled = current_mut(|s| {
        if s.token() == session_token {
            s.set_raw(key, json_value.clone());
            true
        } else {
            false
        }
    })
    .unwrap_or(false);
    if handled {
        return Ok(());
    }

    // Fallback: direct upsert (preserves the out-of-request write
    // semantics for background callers / a non-active session token).
    let stored_id = hash_token(session_token);
    let encoded_value = serde_json::to_string(&json_value)?;
    upsert_session_data_key(&stored_id, key, &encoded_value).await
}

fn sqlite_json_path(key: &str) -> String {
    let escaped = key.replace('\\', "\\\\").replace('"', "\\\"");
    format!("$.\"{escaped}\"")
}

async fn upsert_session_data_key(
    stored_id: &str,
    key: &str,
    encoded_value: &str,
) -> Result<(), SessionError> {
    let now = Utc::now();
    let expires_at = now + Duration::seconds(DEFAULT_TTL_SECONDS);
    match umbral::db::pool_dispatched() {
        umbral::db::DbPool::Sqlite(pool) => {
            let path = sqlite_json_path(key);
            sqlx::query(
                r#"
                INSERT INTO session (id, user_id, data, created_at, expires_at)
                VALUES (?1, NULL, json_set('{}', ?2, json(?3)), ?4, ?5)
                ON CONFLICT(id) DO UPDATE SET
                    data = json_set(COALESCE(NULLIF(session.data, ''), '{}'), ?2, json(?3))
                "#,
            )
            .bind(stored_id)
            .bind(path)
            .bind(encoded_value)
            .bind(now)
            .bind(expires_at)
            .execute(pool)
            .await?;
        }
        umbral::db::DbPool::Postgres(pool) => {
            let path = vec![key.to_string()];
            sqlx::query(
                r#"
                INSERT INTO session (id, user_id, data, created_at, expires_at)
                VALUES ($1, NULL, jsonb_set('{}'::jsonb, $2::text[], $3::jsonb, true)::text, $4, $5)
                ON CONFLICT (id) DO UPDATE SET
                    data = jsonb_set(
                        COALESCE(NULLIF(session.data, '')::jsonb, '{}'::jsonb),
                        $2::text[],
                        $3::jsonb,
                        true
                    )::text
                "#,
            )
            .bind(stored_id)
            .bind(path)
            .bind(encoded_value)
            .bind(now)
            .bind(expires_at)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

// =========================================================================
// Login / logout — user-agnostic primitives. Apps and plugins that
// know which user model they're working with (umbral-auth's AuthUser,
// or a custom UserModel impl) call into these to mint / destroy
// session rows + set the Set-Cookie header. The PK is stringified
// at the call site (`user.id.to_string()`), keeping `umbral-sessions`
// free of any user-model dependency.
//
// For the AuthUser-aware version that bumps `last_login` and
// hydrates the user, see `umbral_auth::login_with_request`.
// =========================================================================

/// Establish a session pinned to `user_id_str` and set the
/// `Set-Cookie` header on the outgoing response. Runs the session-
/// fixation defense (destroys any anonymous session the request
/// carries before minting the new authenticated one) and carries
/// over the old session's `data` (flash messages, cart, etc.) so
/// they survive the rotation.
///
/// `user_id_str` is the user PK serialised via `Display` — `AuthUser`
/// (i64) uses `.to_string()`; a `Uuid`-keyed custom user model uses
/// `Display` on `Uuid`. Pass `None` to mint an anonymous session
/// (rare — `session_layer` already creates one on first visit).
///
/// Returns the raw session token so the caller can log it or fixture
/// it. Production code typically ignores the return.
pub async fn login_user_id(
    request_headers: &HeaderMap,
    response_headers: &mut HeaderMap,
    user_id_str: Option<String>,
) -> Result<String, SessionError> {
    // In-request fast path: rotate the request-scoped session in memory.
    // The layer persists the new record (dirty) at exit; we still destroy
    // the OLD row here (fixation defense) and write the Set-Cookie now so
    // the rotated token reaches the client.
    if let Some((new_token, old_token)) = login_user_id_in_request(&user_id_str) {
        // Session fixation defense: destroy the row keyed by the old token
        // so a leaked cookie can't grant authed access. Mirrors the
        // fallback path's unconditional destroy.
        if let Some(old) = old_token {
            let _ = destroy_session(&old).await;
        }
        let cookie = set_cookie_header(&new_token, None);
        response_headers.insert(
            header::SET_COOKIE,
            cookie.parse().expect("cookie value parses"),
        );
        return Ok(new_token);
    }

    // Fallback (out-of-request): today's destroy-old + create-new + carry
    // path, writing directly to the DB.
    //
    // Capture data from the anonymous session before destroying it,
    // so flash messages etc. don't vanish across login.
    let carry_over_data: Option<String> =
        if let Some(old_token) = cookie_from_headers(request_headers) {
            let data = match read_session(&old_token).await {
                Ok(Some(s)) => Some(s.data),
                _ => None,
            };
            // Session fixation defense: destroy the row keyed by the
            // old (potentially attacker-known) token so a leaked
            // cookie can't grant authed access.
            let _ = destroy_session(&old_token).await;
            data
        } else {
            None
        };

    let token = create_session(user_id_str, None).await?;

    // Restore carry-over data onto the new session, if any.
    if let Some(data) = carry_over_data
        && data != "{}"
    {
        let stored_id = hash_token(&token);
        let mut patch = serde_json::Map::new();
        patch.insert("data".to_string(), serde_json::Value::String(data));
        let _ = Session::objects()
            .filter(session::ID.eq(&stored_id))
            .update_values(patch)
            .await;
    }

    let cookie = set_cookie_header(&token, None);
    response_headers.insert(
        header::SET_COOKIE,
        cookie.parse().expect("cookie value parses"),
    );
    Ok(token)
}

/// In-request half of [`login_user_id`]: rotate the request-scoped
/// session in memory and report `(new_token, old_token)`. Returns `None`
/// when called outside a request scope (so the caller falls back to the
/// direct DB path).
///
/// `rotate` mints a NEW token and a fresh record pinned to `user_id`,
/// carrying the old record's `data` string over **only if it isn't
/// `"{}"`** (the carry-if-not-empty rule, handled inside `rotate`). The
/// rotation marks the record dirty + fresh so the layer's exit `save`
/// persists the new authed row and the layer leaves the fresh-cookie
/// emission to us (we write the Set-Cookie in `login_user_id`).
fn login_user_id_in_request(user_id_str: &Option<String>) -> Option<(String, Option<String>)> {
    current_mut(|s| {
        // The token before rotation = the old session's token (the row we
        // must destroy for the fixation defense). A `fresh` request that
        // never materialised a row still has a candidate token, but no DB
        // row keyed by it, so destroying it is a harmless no-op.
        let old_token = if s.record().is_some() {
            Some(s.token().to_string())
        } else {
            None
        };
        // Carry the old data string across the rotation (flash messages,
        // cart). `rotate` applies the != "{}" filter internally.
        s.rotate(user_id_str.clone(), true);
        (s.token().to_string(), old_token)
    })
}

/// End a session. Reads the session token from the request headers,
/// destroys the row, and sets a `Set-Cookie` header on the response
/// that immediately expires the client-side cookie.
///
/// Both halves are safe to call without a current session — if the
/// user isn't logged in, the destroy is a no-op and the cookie
/// expiration still lands harmlessly.
///
/// ```ignore
/// async fn logout_handler(headers: HeaderMap) -> Response {
///     let mut response = Redirect::to("/").into_response();
///     umbral_sessions::logout(&headers, response.headers_mut())
///         .await
///         .ok();
///     response
/// }
/// ```
pub async fn logout(
    request_headers: &HeaderMap,
    response_headers: &mut HeaderMap,
) -> Result<(), SessionError> {
    if let Some(token) = cookie_from_headers(request_headers) {
        destroy_session(&token).await?;
    }
    response_headers.insert(
        header::SET_COOKIE,
        clear_cookie_header().parse().expect("cookie value parses"),
    );
    Ok(())
}

// User-aware extractors (User / OptionalUser) live in umbral-auth.
// umbral-sessions stays free of any user-model dependency; the
// extractors there call into umbral-sessions's `current_session` to
// read the row and then hydrate `AuthUser` themselves.

// =========================================================================
// Messages: one-shot flash messages.
// =========================================================================

/// Flash messages that survive one redirect cycle, stored under a
/// reserved key in the session's `data` map. Any plugin or app can
/// reach for `Messages` from a handler:
///
/// ```ignore
/// async fn save_post(messages: Messages, Form(form): Form<PostForm>) -> Response {
///     Post::objects().create(...).await?;
///     messages.success("Post saved.").await;
///     Redirect::to("/posts").into_response()
/// }
///
/// async fn list_posts(messages: Messages) -> Html<String> {
///     let flash = messages.drain().await;  // pulls all + clears
///     render("list.html", &context!(messages => flash, ...))
/// }
/// ```
///
/// Requires a session cookie — anonymous requests silently no-op
/// (the message can't outlive the request without somewhere to
/// store it). Apps that want anonymous flash messages create an
/// anonymous session on first visit.
pub mod messages {
    use axum_core::extract::FromRequestParts;
    use http::request::Parts;
    use serde::{Deserialize, Serialize};

    /// The reserved key inside `session.data` where the flash queue
    /// lives. Don't write to this key directly via `set_data` — use
    /// the `Messages` API.
    pub const SESSION_KEY: &str = "_umbral_messages";

    /// Severity / intent of a flash message.
    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum MessageLevel {
        Debug,
        Info,
        Success,
        Warning,
        Error,
    }

    impl MessageLevel {
        /// The CSS-class-friendly lowercase string. Templates can do
        /// `<div class="alert alert-{{ msg.level }}">` directly.
        pub fn as_str(self) -> &'static str {
            match self {
                Self::Debug => "debug",
                Self::Info => "info",
                Self::Success => "success",
                Self::Warning => "warning",
                Self::Error => "error",
            }
        }
    }

    /// One flash message. Serialised into the session's `data` JSON.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Message {
        pub level: MessageLevel,
        pub text: String,
    }

    /// Handle for adding and draining flash messages. Extracted from
    /// the request — see the module-level example.
    ///
    /// The handle captures the session token at extractor time. All
    /// `add` / `success` / etc. calls write to that session;
    /// `drain` reads + clears. No background flush, no on-drop
    /// magic — what you call is what hits the DB.
    #[derive(Debug, Clone)]
    pub struct Messages {
        token: Option<String>,
    }

    impl Messages {
        /// Construct manually. Use the extractor in real code;
        /// this constructor is public for tests.
        pub fn new(token: Option<String>) -> Self {
            Self { token }
        }

        /// True when there's a session backing this handle. Useful
        /// for templates that want to fall back to a different UX
        /// when flash storage isn't available.
        pub fn is_active(&self) -> bool {
            self.token.is_some()
        }

        /// Append a flash message. Silently no-ops if there's no
        /// session backing this request, or if the session was
        /// destroyed concurrently.
        pub async fn add(&self, level: MessageLevel, text: impl Into<String>) {
            let Some(token) = self.token.as_deref() else {
                return;
            };
            let mut current = self.read(token).await.unwrap_or_default();
            current.push(Message {
                level,
                text: text.into(),
            });
            let _ = super::set_data(token, SESSION_KEY, &current).await;
        }

        /// Convenience for `add(Success, text)`.
        pub async fn success(&self, text: impl Into<String>) {
            self.add(MessageLevel::Success, text).await;
        }

        /// Convenience for `add(Info, text)`.
        pub async fn info(&self, text: impl Into<String>) {
            self.add(MessageLevel::Info, text).await;
        }

        /// Convenience for `add(Warning, text)`.
        pub async fn warning(&self, text: impl Into<String>) {
            self.add(MessageLevel::Warning, text).await;
        }

        /// Convenience for `add(Error, text)`.
        pub async fn error(&self, text: impl Into<String>) {
            self.add(MessageLevel::Error, text).await;
        }

        /// Convenience for `add(Debug, text)`.
        pub async fn debug(&self, text: impl Into<String>) {
            self.add(MessageLevel::Debug, text).await;
        }

        /// Read every pending message, then clear them from the
        /// session. The classic "show once, then forget" shape.
        /// Returns an empty Vec when there's no session backing
        /// this handle.
        pub async fn drain(&self) -> Vec<Message> {
            let Some(token) = self.token.as_deref() else {
                return Vec::new();
            };
            // Only clear the queue when the read actually succeeded. The
            // old `unwrap_or_default()` cleared even on a DB error, which
            // would destroy pending messages the user never saw.
            let current = match self.read(token).await {
                Ok(msgs) => msgs,
                Err(e) => {
                    tracing::warn!("messages: failed to read flash queue; not clearing: {e}");
                    return Vec::new();
                }
            };
            if !current.is_empty() {
                let _ = super::set_data(token, SESSION_KEY, &Vec::<Message>::new()).await;
            }
            current
        }

        /// Read without clearing. Useful when a partial pipeline
        /// (e.g. a template fragment) wants to inspect but not
        /// consume the queue.
        pub async fn peek(&self) -> Vec<Message> {
            let Some(token) = self.token.as_deref() else {
                return Vec::new();
            };
            self.read(token).await.unwrap_or_default()
        }

        async fn read(&self, token: &str) -> Result<Vec<Message>, super::SessionError> {
            let session = match super::read_session(token).await? {
                Some(s) => s,
                None => return Ok(Vec::new()),
            };
            match super::get_data::<Vec<Message>>(&session, SESSION_KEY)? {
                Some(v) => Ok(v),
                None => Ok(Vec::new()),
            }
        }
    }

    impl<S> FromRequestParts<S> for Messages
    where
        S: Send + Sync,
    {
        type Rejection = std::convert::Infallible;

        async fn from_request_parts(
            parts: &mut Parts,
            _state: &S,
        ) -> Result<Self, Self::Rejection> {
            // Prefer the SessionToken extension set by session_layer
            // — that's the live session for THIS request. Fall back
            // to the raw cookie for handlers that don't wire the
            // middleware (still useful for ad-hoc auth flows).
            let token = parts
                .extensions
                .get::<super::SessionToken>()
                .map(|t| t.0.clone())
                .or_else(|| super::cookie_from_headers(&parts.headers));
            Ok(Self::new(token))
        }
    }
}

pub use messages::{Message, MessageLevel, Messages};

// =========================================================================
// SessionLayer middleware — lazy session creation (gaps2 #46).
//
// The architectural principle:
// - A SESSION identifies the BROWSER. Anonymous (user_id = NULL) or
//   authenticated (user_id = Some(id)) — same row, same cookie, just a
//   different value in one column.
// - A session ROW is created LAZILY, on the first WRITE — never on bare
//   request entry. The layer mints a candidate token in memory and
//   injects it; the row only materialises when a handler writes the
//   session (via `set_data`, `Messages`, login, etc.). A cookie-less
//   request that never writes (favicon, CSS, an anonymous read page)
//   leaves zero rows and sets no cookie. This is the intended behaviour and
//   it kills the "fresh browser load randomly leaves 3 anonymous rows"
//   bug that eager per-request INSERTs caused.
// - Login transforms an anonymous session into an authenticated one
//   (with a fresh token, see the session-fixation defense in `login`).
// =========================================================================

/// The session token injected into request extensions by
/// [`session_layer`]. Extractors prefer this over the raw cookie so
/// the middleware is the single source of truth.
///
/// Newtype wrapper so it doesn't collide with any other `String`
/// extension a downstream layer might insert.
#[derive(Debug, Clone)]
pub struct SessionToken(pub String);

/// Marker injected when the session was freshly created by this
/// request (i.e. the cookie was missing or stale on entry).
/// SessionLayer reads this on the response side to decide whether to
/// emit a `Set-Cookie` header.
#[derive(Debug, Clone, Copy)]
struct SessionFresh;

/// axum middleware that gives every request a request-scoped session, and
/// lets the session ROW be created lazily on first write.
///
/// On entry:
/// 1. Read the session cookie from the request.
/// 2. `load` the record from the ambient [`SessionStore`]. If it resolves
///    to a live row, reuse the token (`fresh = false`). If it's absent OR
///    stale/expired/destroyed, mint a candidate token IN MEMORY only
///    (`fresh = true`) — **no DB row is inserted here**.
/// 3. Park a [`RequestSession`] in the `CURRENT_SESSION` task-local for the
///    duration of the handler so [`current`] / [`current_mut`] reach it.
///    The [`SessionToken`] (+ [`SessionFresh`]) extensions are still
///    inserted for back-compat with extractors and the `set_data` /
///    `Messages` helpers that resolve the token from extensions.
///
/// On exit (after the handler future resolves):
/// 4. Recover the (possibly mutated) `RequestSession`. If it's `dirty`,
///    `save` it through the store (this is the lazy materialisation — the
///    row is written here, never on entry).
/// 5. For a `fresh` request that didn't set its own cookie, emit
///    `Set-Cookie` **only if a row now exists** for the token — i.e. the
///    in-memory `dirty` flag is set, OR a side-channel write (`set_data` /
///    `Messages`) materialised the row. If nothing wrote the session, no
///    cookie is set and no row is left behind.
///
/// Apply to your router with `axum::middleware::from_fn`:
///
/// ```ignore
/// use axum::{middleware, Router, routing::get};
/// let router = Router::new()
///     .route("/", get(home))
///     .layer(middleware::from_fn(umbral_sessions::session_layer));
/// ```
///
/// Or — the typical case — let [`SessionsPlugin`] apply it
/// automatically via its `wrap_router` hook (default behaviour).
pub async fn session_layer(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::header;
    use request_session::{CURRENT_SESSION, RequestSession};
    use std::cell::RefCell;

    let cookie_token = cookie_from_headers(req.headers());
    let store = active_store();

    // Load via the store at entry. `fresh = record.is_none()`.
    let (token, fresh, record) = match cookie_token {
        Some(t) => match store.load(&t).await {
            Ok(Some(rec)) => (t, false, Some(rec)),
            // Cookie present but stale/expired/destroyed (or a load
            // error). Mint a candidate token IN MEMORY only — no DB row
            // yet. A row materialises lazily iff the handler writes the
            // session. Until then this request leaves no
            // trace.
            _ => (Uuid::new_v4().to_string(), true, None),
        },
        // No cookie. Same lazy treatment: candidate token in memory, no
        // INSERT on entry. Favicon / asset / anonymous-read requests that
        // never write the session leave zero rows behind.
        None => (Uuid::new_v4().to_string(), true, None),
    };

    // Back-compat: extractors, `set_data`, and `Messages` resolve the
    // token from these extensions. Keep them so the side-channel write
    // paths keep working untouched.
    req.extensions_mut().insert(SessionToken(token.clone()));
    if fresh {
        req.extensions_mut().insert(SessionFresh);
    }

    let mut rs_value = RequestSession::new(token.clone(), fresh, record);

    // Sliding expiry: when enabled and a LIVE record was loaded, extend
    // expires_at to now + TTL so an actively-used session never
    // hard-expires mid-use. Applied IN MEMORY and marked dirty so the
    // single exit-time `store.save` persists it — DB-free on entry, and
    // crucially it shares the one exit save with any handler `set_data`
    // write (otherwise the exit save would clobber the bump back to the
    // loaded value). Fires only for a live loaded session, so the
    // lazy-creation contract (#46) is untouched: a fresh request never
    // gets a sliding write. This is the one extra write per request the
    // default-off flag avoids for everyone who doesn't need rolling
    // windows.
    if !fresh && *SLIDING_EXPIRY_ENABLED.get().unwrap_or(&false) {
        rs_value.bump_expiry(Utc::now() + Duration::seconds(DEFAULT_TTL_SECONDS));
    }

    let rs = RefCell::new(rs_value);

    // Scope the session task-local around the handler future, then recover
    // the (possibly mutated) RequestSession after it resolves.
    let (mut response, rs) = CURRENT_SESSION
        .scope(rs, async move {
            let response = next.run(req).await;
            (response, CURRENT_SESSION.with(|cell| cell.borrow().clone()))
        })
        .await;

    // Lazy materialisation: persist the record only if a handler mutated
    // it through `current_mut`. This is the ONLY write on the
    // RequestSession path — nothing was written on entry.
    //
    // `save` returns the COOKIE VALUE to set: the raw token for `DbStore`
    // (server holds the row), or the encrypted blob for `CookieStore` (the
    // cookie carries the whole record, zero DB round-trip). Capture it and
    // use it for the Set-Cookie below instead of the bare token — that's
    // what lets a stateless store work without touching the integration
    // points (fresh guard, login rotation, sliding expiry) at all.
    let mut cookie_value: Option<String> = None;
    if rs.is_dirty() {
        if let Some(rec) = rs.record() {
            match store.save(rs.token(), rec).await {
                Ok(value) => cookie_value = Some(value),
                Err(e) => tracing::warn!("session_layer: store.save failed: {e}"),
            }
        }
    }

    // Set-Cookie on the way out only for FRESH sessions that the request
    // actually wrote a row for.
    //
    // "A row now exists" is signalled by EITHER the in-memory `dirty` flag
    // (the RequestSession path just `save`d above) OR a side-channel write
    // (`set_data` / `Messages`) that materialised the row without touching
    // the RequestSession — caught by the `read_session` probe. The probe
    // runs only for fresh requests that didn't already set their own
    // cookie, so it's minimal overhead.
    //
    // The handler-set-cookie guard stays: `login_with_request` sets its
    // own Set-Cookie to rotate the token after the credential check
    // (session-fixation defense). Without this guard the layer would
    // clobber the authenticated cookie with the anonymous one, breaking
    // every cookie-based login.
    if fresh && !response.headers().contains_key(header::SET_COOKIE) {
        let row_exists = rs.is_dirty() || matches!(read_session(&token).await, Ok(Some(_)));
        if row_exists {
            // Use the value `save` returned when it ran (the encrypted blob
            // for `CookieStore`); fall back to the raw token for the
            // side-channel `set_data` path that materialised a row without
            // going through the RequestSession `save` (so `cookie_value` is
            // still `None`). `DbStore::save` returns the token, so this is a
            // no-op for the DB-backed default.
            let value_to_set = cookie_value.as_deref().unwrap_or(&token);
            let cookie = set_cookie_header(value_to_set, None);
            if let Ok(value) = cookie.parse() {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
        }
    } else if let Some(value) = cookie_value.as_deref() {
        // A NON-fresh request mutated a loaded session (sliding expiry, or a
        // `current_mut` write on a returning session). For a stateless
        // `CookieStore` the blob now differs from the cookie the browser
        // sent, so it MUST be re-set — otherwise the mutation is lost on the
        // next request. For `DbStore`, `save` returned the unchanged raw
        // token, which equals the cookie already present, so we skip the
        // redundant Set-Cookie to preserve the existing no-cookie-churn
        // behaviour. The handler-set-cookie guard still wins (login rotation
        // owns its own Set-Cookie).
        if value != token && !response.headers().contains_key(header::SET_COOKIE) {
            let cookie = set_cookie_header(value, None);
            if let Ok(parsed) = cookie.parse() {
                response.headers_mut().insert(header::SET_COOKIE, parsed);
            }
        }
    }
    response
}

// The `user_context_layer` that injected the current user into the
// `umbral::templates::CURRENT_USER` task-local moved to umbral-auth
// alongside the rest of the AuthUser-aware surface. It now hangs off
// `AuthPlugin::with_user_in_templates()` instead of
// `SessionsPlugin::with_user_in_templates()` — the layer needs to
// hydrate the AuthUser row, which umbral-sessions can no longer name.
