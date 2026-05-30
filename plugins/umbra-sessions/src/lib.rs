//! umbra-sessions — DB-backed session storage for umbra.
//!
//! Cookie-shaped sessions linked to umbra-auth's `AuthUser`. The
//! `Session` model lives in the `session` table; one row per
//! browser session, identified by a random UUID written to the
//! `umbra_session` cookie.
//!
//! ## Surface
//!
//! - `Session` model (id, user_id, data, created_at, expires_at)
//! - `SessionsPlugin` registers the model
//! - `create_session(user_id, ttl)` -> new id (write to Set-Cookie)
//! - `read_session(id)` -> `Option<Session>` (filters out expired)
//! - `destroy_session(id)` -> Delete
//! - `cookie_from_headers(headers)` -> extract session id from
//!   the request's `Cookie` header
//! - `set_cookie_header(id)` -> the Set-Cookie string for a login
//!   response. `Secure`, `HttpOnly`, `SameSite=Lax` by default,
//!   matching the security-defaults outline.
//! - `current_user(headers)` -> `Option<AuthUser>` — the one-call
//!   helper handlers use. Looks up the session via the cookie and
//!   hydrates the user via umbra-auth.
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
//!   `umbra-tasks` periodic job, or a `clearsessions` management
//!   command, lands when one or the other is real.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use umbra::prelude::*;
use umbra::web::{HeaderMap, header};
use umbra_auth::AuthUser;
use uuid::Uuid;

/// Default cookie name. Users override via `set_cookie_header_named`
/// when they need a project-specific name.
pub const COOKIE_NAME: &str = "umbra_session";

/// Default session TTL: 14 days. Matches Django's
/// `SESSION_COOKIE_AGE` default.
pub const DEFAULT_TTL_SECONDS: i64 = 14 * 24 * 60 * 60;

/// The session row.
///
/// `id` is a random UUID written to the client cookie. `user_id` is
/// the umbra-auth user this session belongs to (nullable so anonymous
/// sessions are possible, though v1 doesn't surface that path). `data`
/// is a free-form JSON string the application stores per-session.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct Session {
    pub id: String,
    pub user_id: Option<i64>,
    pub data: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// The plugin. Registers the `Session` model so `makemigrations`
/// generates the right CREATE TABLE.
#[derive(Debug, Default)]
pub struct SessionsPlugin;

impl Plugin for SessionsPlugin {
    fn name(&self) -> &'static str {
        "sessions"
    }

    fn dependencies(&self) -> &'static [&'static str] {
        // No hard dep on auth at the trait level. The current_user
        // helper does call umbra_auth, but the Session model itself
        // is independent — anonymous sessions are valid.
        &[]
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![umbra::migrate::ModelMeta::for_::<Session>()]
    }
}

/// Errors the helpers produce.
#[derive(Debug)]
pub enum SessionError {
    /// sqlx error executing one of the helper queries.
    Sqlx(sqlx::Error),
    /// `data` round-tripping through serde failed.
    Json(serde_json::Error),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Sqlx(e) => write!(f, "umbra-sessions: sqlx: {e}"),
            SessionError::Json(e) => write!(f, "umbra-sessions: json: {e}"),
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

// =========================================================================
// Public helpers.
// =========================================================================

/// Create a new session row for the given user. Returns the session
/// id, which the caller writes into a Set-Cookie header via
/// [`set_cookie_header`].
///
/// `ttl` controls the row's `expires_at`. Pass `None` to use
/// [`DEFAULT_TTL_SECONDS`] (14 days).
pub async fn create_session(user_id: i64, ttl: Option<Duration>) -> Result<String, SessionError> {
    let pool = umbra::db::pool();
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let expires_at = now + ttl.unwrap_or_else(|| Duration::seconds(DEFAULT_TTL_SECONDS));
    sqlx::query(
        "INSERT INTO session (id, user_id, data, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind("{}")
    .bind(now)
    .bind(expires_at)
    .execute(&pool)
    .await?;
    Ok(id)
}

/// Look up a session by id. Returns `None` if the row doesn't exist
/// OR if it's expired (in which case the row is also deleted — lazy
/// cleanup, no scheduled job needed).
pub async fn read_session(id: &str) -> Result<Option<Session>, SessionError> {
    let pool = umbra::db::pool();
    let row: Option<Session> = sqlx::query_as::<_, Session>("SELECT * FROM session WHERE id = ?")
        .bind(id)
        .fetch_optional(&pool)
        .await?;
    if let Some(s) = &row
        && s.expires_at < Utc::now()
    {
        destroy_session_with_pool(&pool, id).await?;
        return Ok(None);
    }
    Ok(row)
}

/// Delete a session row. Used by logout. Idempotent: a non-existent
/// id is treated as success.
pub async fn destroy_session(id: &str) -> Result<(), SessionError> {
    let pool = umbra::db::pool();
    destroy_session_with_pool(&pool, id).await
}

async fn destroy_session_with_pool(pool: &SqlitePool, id: &str) -> Result<(), SessionError> {
    sqlx::query("DELETE FROM session WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Parse the `Cookie` header and return the umbra session id, if
/// present. The handler that wants to know who's calling reads this
/// then `read_session` then `umbra_auth::AuthUser::objects().filter(...)`.
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

/// [`set_cookie_header`] with an explicit cookie name. The `Secure`
/// flag stays on; HTTPS-only is non-negotiable for session cookies.
pub fn set_cookie_header_named(name: &str, id: &str, max_age: Option<i64>) -> String {
    let max_age = max_age.unwrap_or(DEFAULT_TTL_SECONDS);
    format!("{name}={id}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={max_age}")
}

/// Build the Set-Cookie header that deletes the session cookie.
/// Used on logout: the client sees an immediately-expired cookie and
/// drops the local value.
pub fn clear_cookie_header() -> String {
    clear_cookie_header_named(COOKIE_NAME)
}

/// [`clear_cookie_header`] with an explicit cookie name.
pub fn clear_cookie_header_named(name: &str) -> String {
    format!("{name}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0")
}

/// One-call helper: read the session cookie, look up the session,
/// hydrate the user via the umbra-auth schema. Returns `None` if any
/// step fails (no cookie, no session, expired session, no user).
///
/// The handler signature reads like a real authenticated route:
///
/// ```ignore
/// async fn dashboard(headers: HeaderMap) -> Result<Html<String>, StatusCode> {
///     let Some(user) = umbra_sessions::current_user(&headers).await? else {
///         return Err(StatusCode::UNAUTHORIZED);
///     };
///     // ...
/// }
/// ```
pub async fn current_user(headers: &HeaderMap) -> Result<Option<AuthUser>, SessionError> {
    let Some(id) = cookie_from_headers(headers) else {
        return Ok(None);
    };
    let Some(session) = read_session(&id).await? else {
        return Ok(None);
    };
    let Some(user_id) = session.user_id else {
        return Ok(None);
    };
    let pool = umbra::db::pool();
    let user: Option<AuthUser> =
        sqlx::query_as::<_, AuthUser>("SELECT * FROM auth_user WHERE id = ? AND is_active = 1")
            .bind(user_id)
            .fetch_optional(&pool)
            .await?;
    Ok(user)
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
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&session.data).unwrap_or_default();
    match map.get(key) {
        None => Ok(None),
        Some(value) => Ok(Some(serde_json::from_value(value.clone())?)),
    }
}

/// Write a typed value into a session's `data` map by key. Reads the
/// existing map, sets the key, writes the row back.
pub async fn set_data<T: Serialize>(
    session_id: &str,
    key: &str,
    value: &T,
) -> Result<(), SessionError> {
    let pool = umbra::db::pool();
    // Pull the current row so we don't clobber other keys.
    let row: Option<(String,)> = sqlx::query_as("SELECT data FROM session WHERE id = ?")
        .bind(session_id)
        .fetch_optional(&pool)
        .await?;
    let Some((current,)) = row else {
        // Session was destroyed between get_session and set_data.
        // Treat as success silently rather than erroring; the data
        // would have been lost when the session expired anyway.
        return Ok(());
    };
    let mut map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&current).unwrap_or_default();
    map.insert(key.to_string(), serde_json::to_value(value)?);
    let updated = serde_json::to_string(&map)?;
    sqlx::query("UPDATE session SET data = ? WHERE id = ?")
        .bind(&updated)
        .bind(session_id)
        .execute(&pool)
        .await?;
    Ok(())
}
