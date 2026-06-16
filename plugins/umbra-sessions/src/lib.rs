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
//! - `SessionsPlugin` registers the model AND auto-applies
//!   `session_layer`. A session row is created **lazily on first
//!   write** (Django-style): a cookie-less request that never writes
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
use umbra::prelude::*;
use umbra::web::{HeaderMap, header};
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
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct Session {
    pub id: String,
    pub user_id: Option<String>,
    pub data: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// The plugin. Registers the `Session` model and (by default)
/// auto-applies [`session_layer`], which creates a session row
/// lazily on the first write (Django-style — see `session_layer`).
/// Opt out with [`Self::without_auto_layer`] if you want to control
/// session creation by hand (rare).
#[derive(Debug, Clone)]
pub struct SessionsPlugin {
    auto_layer: bool,
}

impl Default for SessionsPlugin {
    fn default() -> Self {
        Self { auto_layer: true }
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
}

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

    fn wrap_router(&self, router: umbra::web::Router) -> umbra::web::Router {
        let mut router = router;
        if self.auto_layer {
            router = router.layer(axum::middleware::from_fn(session_layer));
        }
        router
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
    Write(umbra::orm::write::WriteError),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Sqlx(e) => write!(f, "umbra-sessions: sqlx: {e}"),
            SessionError::Json(e) => write!(f, "umbra-sessions: json: {e}"),
            SessionError::Write(e) => write!(f, "umbra-sessions: write: {e:?}"),
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

impl From<umbra::orm::write::WriteError> for SessionError {
    fn from(e: umbra::orm::write::WriteError) -> Self {
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
fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    format!("{:x}", hasher.finalize())
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
    let stored_id = hash_token(token);
    let row: Option<Session> = Session::objects()
        .filter(session::ID.eq(&stored_id))
        .first()
        .await?;
    if let Some(s) = &row
        && s.expires_at < Utc::now()
    {
        destroy_session_by_hash(&stored_id).await?;
        return Ok(None);
    }
    Ok(row)
}

/// Delete a session row by its raw token. Used by logout. Idempotent:
/// a non-existent token is treated as success. The token is hashed
/// before the DELETE so the same hash-on-write/hash-on-read invariant
/// holds for destruction too.
pub async fn destroy_session(token: &str) -> Result<(), SessionError> {
    let stored_id = hash_token(token);
    destroy_session_by_hash(&stored_id).await
}

/// Internal: takes the already-hashed stored id, not the raw token.
/// Used by `read_session`'s expiry-cleanup branch and `destroy_session`.
async fn destroy_session_by_hash(stored_id: &str) -> Result<(), SessionError> {
    Session::objects()
        .filter(session::ID.eq(stored_id))
        .delete()
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

/// The `Secure; ` cookie attribute — present in every environment
/// except `Dev`. A `Secure` cookie is only sent over HTTPS, which is
/// correct (and non-negotiable) in production but breaks cookie-based
/// auth over plain `http://` in local development: the browser silently
/// drops it, so every request resolves anonymous. Gating it on the
/// environment mirrors the framework's "HSTS off for local http dev"
/// posture. Defaults to `Secure` when settings aren't resolved yet
/// (secure-by-default).
fn secure_attr() -> &'static str {
    match umbra::settings::get_opt() {
        Some(s) if matches!(s.environment, umbra::Environment::Dev) => "",
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
/// hydrates the row, see `umbra_auth::current_user` (lives there so
/// `umbra-sessions` stays free of any user-model dependency).
pub async fn current_session(headers: &HeaderMap) -> Result<Option<Session>, SessionError> {
    let Some(id) = cookie_from_headers(headers) else {
        return Ok(None);
    };
    read_session(&id).await
}

/// Read the request's session cookie and return the stringified user
/// id stashed on the active session, if any. The result is the value
/// `umbra_auth::login_with_request` (or any other login helper)
/// stored via [`create_session`]; for an `AuthUser`-shaped i64 PK,
/// `.parse::<i64>().ok()` round-trips it back.
///
/// Returns `None` on any of: no cookie, expired session, anonymous
/// session (`user_id IS NULL`). The user-id parse failure mode is
/// reserved for the caller, since each user model knows its own PK
/// shape.
pub async fn current_user_id_str(headers: &HeaderMap) -> Result<Option<String>, SessionError> {
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

/// Write a typed value into a session's `data` map by key. Reads the
/// existing map, sets the key, writes the row back. `session_token`
/// is the raw token from the cookie; hashed before the WHERE clause
/// like every other session-lookup path.
pub async fn set_data<T: Serialize>(
    session_token: &str,
    key: &str,
    value: &T,
) -> Result<(), SessionError> {
    let stored_id = hash_token(session_token);
    // Pull the current row so we don't clobber other keys.
    let row: Option<Session> = Session::objects()
        .filter(session::ID.eq(&stored_id))
        .first()
        .await?;
    let current = match row {
        Some(current) => current,
        None => {
            // Lazy materialisation (gaps2 #46): the session hasn't been
            // persisted yet (the middleware mints the token in memory
            // and only the first WRITE creates the row). CREATE an
            // anonymous row now, then apply the write below.
            let now = Utc::now();
            let fresh = Session {
                id: stored_id.clone(),
                user_id: None,
                data: "{}".to_string(),
                created_at: now,
                expires_at: now + Duration::seconds(DEFAULT_TTL_SECONDS),
            };
            match Session::objects().create(fresh).await {
                Ok(created) => created,
                // Race tolerance: a concurrent write on the same token
                // already created the row. Treat the PK collision as
                // "already exists" and re-read so we modify the live
                // row rather than erroring. The net effect is exactly
                // one row for the first write to materialise a session.
                Err(umbra::orm::write::WriteError::UniqueViolation { .. }) => Session::objects()
                    .filter(session::ID.eq(&stored_id))
                    .first()
                    .await?
                    .ok_or_else(|| {
                        SessionError::Sqlx(sqlx::Error::Protocol(
                            "set_data: row vanished after a concurrent create".to_string(),
                        ))
                    })?,
                Err(e) => return Err(e.into()),
            }
        }
    };
    let mut map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&current.data)?;
    map.insert(key.to_string(), serde_json::to_value(value)?);
    let updated = serde_json::to_string(&map)?;
    let mut patch = serde_json::Map::new();
    patch.insert("data".to_string(), serde_json::Value::String(updated));
    Session::objects()
        .filter(session::ID.eq(&stored_id))
        .update_values(patch)
        .await?;
    Ok(())
}

// =========================================================================
// Login / logout — user-agnostic primitives. Apps and plugins that
// know which user model they're working with (umbra-auth's AuthUser,
// or a custom UserModel impl) call into these to mint / destroy
// session rows + set the Set-Cookie header. The PK is stringified
// at the call site (`user.id.to_string()`), keeping `umbra-sessions`
// free of any user-model dependency.
//
// For the AuthUser-aware version that bumps `last_login` and
// hydrates the user, see `umbra_auth::login_with_request`.
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
///     umbra_sessions::logout(&headers, response.headers_mut())
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

// User-aware extractors (User / OptionalUser) live in umbra-auth.
// umbra-sessions stays free of any user-model dependency; the
// extractors there call into umbra-sessions's `current_session` to
// read the row and then hydrate `AuthUser` themselves.

// =========================================================================
// Messages — Django's `contrib.messages` shape.
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
    pub const SESSION_KEY: &str = "_umbra_messages";

    /// Severity / intent of a flash message. Matches Django's
    /// constants so existing templates port over.
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
            let current = self.read(token).await.unwrap_or_default();
            let _ = super::set_data(token, SESSION_KEY, &Vec::<Message>::new()).await;
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
//   leaves zero rows and sets no cookie. This is Django's behaviour and
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

/// axum middleware that gives every request a session TOKEN, and lets
/// the session ROW be created lazily on first write (Django-style).
///
/// On entry:
/// 1. Read the session cookie from the request.
/// 2. If it resolves to a live row, reuse it (`fresh = false`). If it's
///    absent OR stale/expired/destroyed, mint a candidate token IN
///    MEMORY only (`fresh = true`) — **no DB row is inserted here**.
/// 3. Inject the resolved [`SessionToken`] into request extensions so
///    extractors and handlers find it. The first write through that
///    token (`set_data`, `Messages`, login) materialises the row.
///
/// On exit:
/// 4. For a `fresh` request that didn't set its own cookie, emit
///    `Set-Cookie` **only if a row now exists** for the token (a write
///    during the request materialised it). If nothing wrote the
///    session, no cookie is set and no row is left behind.
///
/// Apply to your router with `axum::middleware::from_fn`:
///
/// ```ignore
/// use axum::{middleware, Router, routing::get};
/// let router = Router::new()
///     .route("/", get(home))
///     .layer(middleware::from_fn(umbra_sessions::session_layer));
/// ```
///
/// Or — the typical case — let [`SessionsPlugin`] apply it
/// automatically via its `wrap_router` hook (default behaviour).
pub async fn session_layer(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::header;
    let cookie_token = cookie_from_headers(req.headers());

    let (token, fresh) = match cookie_token {
        Some(t) => match read_session(&t).await {
            // A live row exists and the client already holds the
            // cookie — reuse it, nothing to persist or set.
            Ok(Some(_)) => (t, false),
            // Cookie present but stale/expired/destroyed. Mint a
            // candidate token IN MEMORY only — no DB row yet. A row
            // materialises lazily iff the handler writes the session
            // (Django-style). Until then this request leaves no trace.
            _ => (Uuid::new_v4().to_string(), true),
        },
        // No cookie. Same lazy treatment: candidate token in memory,
        // no INSERT on entry. Favicon / asset / anonymous-read requests
        // that never write the session leave zero rows behind.
        None => (Uuid::new_v4().to_string(), true),
    };

    req.extensions_mut().insert(SessionToken(token.clone()));
    if fresh {
        req.extensions_mut().insert(SessionFresh);
    }

    let mut response = next.run(req).await;

    // Set-Cookie on the way out only for FRESH sessions that the
    // handler actually wrote (i.e. a row now exists for this token).
    //
    // The handler-set-cookie guard stays: `login_with_request` sets
    // its own Set-Cookie to rotate the token after the credential
    // check (session-fixation defense). Without this guard the layer
    // would clobber the authenticated cookie with the anonymous one,
    // breaking every cookie-based login.
    //
    // The `read_session` here runs only for fresh requests that didn't
    // set their own cookie — minimal overhead. If no row materialised
    // (favicon / asset / anonymous page with no session write), we set
    // NO cookie and leave NO row.
    if fresh
        && !response.headers().contains_key(header::SET_COOKIE)
        && matches!(read_session(&token).await, Ok(Some(_)))
    {
        let cookie = set_cookie_header(&token, None);
        if let Ok(value) = cookie.parse() {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
    }
    response
}

// The `user_context_layer` that injected the current user into the
// `umbra::templates::CURRENT_USER` task-local moved to umbra-auth
// alongside the rest of the AuthUser-aware surface. It now hangs off
// `AuthPlugin::with_user_in_templates()` instead of
// `SessionsPlugin::with_user_in_templates()` — the layer needs to
// hydrate the AuthUser row, which umbra-sessions can no longer name.
