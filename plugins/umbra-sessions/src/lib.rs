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
//!   `session_layer` so every browser gets a session on first visit
//!   (anonymous or authed — same row, same cookie). Opt out via
//!   `SessionsPlugin::default().without_auto_layer()`.
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
use umbra_auth::{AuthUser, auth_user};
use uuid::Uuid;

pub mod session_auth;
pub use session_auth::SessionAuthentication;

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
/// auto-applies [`session_layer`] so every browser gets a session
/// on first visit. Opt out with [`Self::without_auto_layer`] if
/// you want to control session creation by hand (rare).
#[derive(Debug, Clone)]
pub struct SessionsPlugin {
    auto_layer: bool,
    user_in_templates: bool,
}

impl Default for SessionsPlugin {
    fn default() -> Self {
        Self {
            auto_layer: true,
            user_in_templates: false,
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

    /// Make the current session user available as `{{ user }}` in
    /// every template render — mirrors Django's `request.user`. When
    /// enabled, [`user_context_layer`] is auto-applied to the router
    /// so every request resolves the user (one DB read each) and
    /// stashes the resulting `minijinja::Value` into the task-local
    /// [`umbra::templates::CURRENT_USER`] for the request's duration.
    /// `render` then merges that value into the ctx under key `user`
    /// unless the handler supplied its own. Templates can use
    /// `{% if user.is_authenticated %}` uniformly: the layer always
    /// injects *something* — either the serialized user augmented
    /// with `is_authenticated: true`, or the anonymous sentinel
    /// `{ is_authenticated: false }`.
    ///
    /// Off by default because most non-HTML endpoints (REST APIs,
    /// static assets, health checks) don't benefit from the per-
    /// request DB lookup. Turn it on for an HTML-heavy app.
    pub fn with_user_in_templates(mut self) -> Self {
        self.user_in_templates = true;
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
        if self.user_in_templates {
            router = router.layer(axum::middleware::from_fn(user_context_layer));
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
    let Some(user_id_str) = session.user_id else {
        return Ok(None);
    };
    // Session.user_id is polymorphic text (gap #59). `AuthUser` is
    // i64-keyed, so parse the stored string back to i64. A malformed
    // value (string that doesn't parse as i64) means this session
    // was written by code that uses a different `UserModel` shape —
    // treat as anonymous from `AuthUser`'s perspective.
    let Ok(user_id) = user_id_str.parse::<i64>() else {
        return Ok(None);
    };
    let user: Option<AuthUser> = AuthUser::objects()
        .filter(auth_user::ID.eq(user_id) & auth_user::IS_ACTIVE.eq(true))
        .first()
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
    let Some(current) = row else {
        // Session was destroyed between get_session and set_data.
        // Treat as success silently rather than erroring; the data
        // would have been lost when the session expired anyway.
        return Ok(());
    };
    let mut map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&current.data).unwrap_or_default();
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
// Login / logout bundles. The three-call dance — authenticate ->
// create_session -> set_cookie_header — collapsed into one function so
// route handlers don't have to reimplement it.
// =========================================================================

/// Establish a session for the given authenticated user and set the
/// `Set-Cookie` header on the outgoing response. Updates
/// `last_login` on the user row as a side effect.
///
/// `response_headers` is mutated in place — pass the headers from an
/// already-constructed response (e.g. `redirect.headers_mut()`).
/// `user` should come from `umbra_auth::authenticate(...)`; this
/// helper does not re-verify credentials.
///
/// Returns the raw session token so the caller can log it or stash
/// it in a test fixture. In production code you can ignore the
/// returned value.
///
/// ```ignore
/// async fn login_handler(Form(form): Form<LoginForm>)
///     -> Result<Response, StatusCode>
/// {
///     let user = umbra_auth::authenticate(&form.username, &form.password)
///         .await
///         .map_err(|_| StatusCode::UNAUTHORIZED)?;
///     let mut response = Redirect::to("/").into_response();
///     umbra_sessions::login(response.headers_mut(), &user)
///         .await
///         .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
///     Ok(response)
/// }
/// ```
pub async fn login(
    response_headers: &mut HeaderMap,
    user: &AuthUser,
) -> Result<String, SessionError> {
    login_with_request(&HeaderMap::new(), response_headers, user).await
}

/// `login` variant that takes the **request** headers too. Used to
/// defend against session fixation: if the request already carries
/// an anonymous session (created by `session_layer`), the row is
/// destroyed before the new authenticated session is created.
/// Carries over the existing session's `data` (flash messages, cart
/// contents, etc.) so they survive the login.
///
/// Most apps call [`login`] directly; this variant exists for
/// handlers that have a `HeaderMap` extractor and want the fixation
/// defense to apply.
pub async fn login_with_request(
    request_headers: &HeaderMap,
    response_headers: &mut HeaderMap,
    user: &AuthUser,
) -> Result<String, SessionError> {
    // Capture data from the anonymous session before destroying it,
    // so flash messages etc. don't vanish across login.
    let carry_over_data: Option<String> =
        if let Some(old_token) = cookie_from_headers(request_headers) {
            let data = match read_session(&old_token).await {
                Ok(Some(s)) => Some(s.data),
                _ => None,
            };
            // Session fixation defense: destroy the row keyed by the old
            // (potentially attacker-known) token so a leaked cookie
            // can't grant authed access.
            let _ = destroy_session(&old_token).await;
            data
        } else {
            None
        };

    // Stringify the user's PK before stashing it in the session row
    // (gap #59). AuthUser is i64-keyed so this is just `.to_string()`;
    // custom user models with `Uuid` or string PKs would call the
    // same helper since `Display` is what writes the canonical form.
    let token = create_session(Some(user.id.to_string()), None).await?;

    // Restore the carry-over data onto the new session, if any.
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
    // Update last_login. Best-effort — a failure here doesn't
    // invalidate the login (the session was created and the cookie
    // was set), so we swallow the error after logging.
    let mut patch = serde_json::Map::new();
    patch.insert(
        "last_login".to_string(),
        serde_json::to_value(chrono::Utc::now()).unwrap_or(serde_json::Value::Null),
    );
    if let Err(e) = AuthUser::objects()
        .filter(auth_user::ID.eq(user.id))
        .update_values(patch)
        .await
    {
        tracing::warn!(
            error = ?e,
            user_id = user.id,
            "umbra-sessions::login: failed to update last_login (session still active)",
        );
    }
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

// =========================================================================
// Extractors — `User` and `OptionalUser` for `request.user` ergonomics.
// =========================================================================

/// axum extractors that resolve the current session cookie into an
/// `AuthUser`.
///
/// - [`User`]: required. Returns 401 if the request is anonymous —
///   the route literally cannot run without a logged-in user.
/// - [`OptionalUser`]: optional. Wraps `Option<AuthUser>` — `None`
///   for anonymous; `Some` for authenticated. The route runs either
///   way and decides what to do.
///
/// Both implementations share one helper. The handler signature
/// becomes the request.user-like ergonomics you'd see in Django:
///
/// ```ignore
/// async fn dashboard(User(user): User) -> Html<String> {
///     Html(format!("Welcome, {}!", user.username))
/// }
///
/// async fn home(OptionalUser(maybe): OptionalUser) -> Html<String> {
///     match maybe {
///         Some(u) => Html(format!("Hi, {}", u.username)),
///         None    => Html("<a href=\"/login\">Log in</a>".into()),
///     }
/// }
/// ```
pub mod extractors {
    use axum_core::extract::FromRequestParts;
    use http::StatusCode;
    use http::request::Parts;
    use umbra_auth::AuthUser;

    /// Required-user extractor. 401 on anonymous requests.
    #[derive(Debug, Clone)]
    pub struct User(pub AuthUser);

    /// Optional-user extractor. Anonymous requests get `None`.
    #[derive(Debug, Clone)]
    pub struct OptionalUser(pub Option<AuthUser>);

    /// Helper that does the actual session-lookup. Both extractors
    /// route through this so the resolution order stays one
    /// definition.
    ///
    /// Order:
    /// 1. `SessionToken` extension (set by `session_layer`) — single
    ///    source of truth when the middleware is wired.
    /// 2. Cookie header — backward-compat for handlers that don't
    ///    use the middleware.
    ///
    /// Anonymous sessions resolve to `None` here (the session row
    /// has `user_id = NULL`).
    async fn resolve(parts: &Parts) -> Option<AuthUser> {
        let token = parts
            .extensions
            .get::<super::SessionToken>()
            .map(|t| t.0.clone())
            .or_else(|| super::cookie_from_headers(&parts.headers))?;
        let session = super::read_session(&token).await.ok().flatten()?;
        // Session.user_id is text (gap #59); parse back to i64 for
        // the AuthUser extractor. A non-parseable value means a
        // different UserModel wrote the session — return None.
        let user_id: i64 = session.user_id?.parse().ok()?;
        super::AuthUser::objects()
            .filter(super::auth_user::ID.eq(user_id) & super::auth_user::IS_ACTIVE.eq(true))
            .first()
            .await
            .ok()
            .flatten()
    }

    impl<S> FromRequestParts<S> for User
    where
        S: Send + Sync,
    {
        type Rejection = (StatusCode, &'static str);

        async fn from_request_parts(
            parts: &mut Parts,
            _state: &S,
        ) -> Result<Self, Self::Rejection> {
            match resolve(parts).await {
                Some(u) => Ok(User(u)),
                None => Err((StatusCode::UNAUTHORIZED, "authentication required")),
            }
        }
    }

    impl<S> FromRequestParts<S> for OptionalUser
    where
        S: Send + Sync,
    {
        type Rejection = std::convert::Infallible;

        async fn from_request_parts(
            parts: &mut Parts,
            _state: &S,
        ) -> Result<Self, Self::Rejection> {
            Ok(OptionalUser(resolve(parts).await))
        }
    }
}

pub use extractors::{OptionalUser, User};

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
// SessionLayer middleware — auto-creates anonymous sessions.
//
// The architectural principle:
// - A SESSION identifies the BROWSER. Anonymous (user_id = NULL) or
//   authenticated (user_id = Some(id)) — same row, same cookie, just a
//   different value in one column.
// - Every browser gets a session on first visit. Cart contents, flash
//   messages, CSRF tokens — they all live somewhere now.
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

/// axum middleware that ensures every request has a session.
///
/// On entry:
/// 1. Read the session cookie from the request.
/// 2. If absent OR the cookie value doesn't resolve to a live
///    session row (stale / expired / destroyed), create a fresh
///    **anonymous** session (`user_id = NULL`) and flag the
///    response.
/// 3. Inject the resolved [`SessionToken`] into request extensions
///    so extractors find it.
///
/// On exit:
/// 4. If the session was newly created, set the `Set-Cookie`
///    header on the response.
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
            Ok(Some(_)) => (t, false),
            _ => {
                // Cookie present but session is gone (expired or
                // destroyed). Create a fresh anonymous session so the
                // browser doesn't go without one for the entire request.
                match create_session(None, None).await {
                    Ok(new) => (new, true),
                    Err(_) => return next.run(req).await, // best-effort
                }
            }
        },
        None => match create_session(None, None).await {
            Ok(new) => (new, true),
            Err(_) => return next.run(req).await, // best-effort
        },
    };

    req.extensions_mut().insert(SessionToken(token.clone()));
    if fresh {
        req.extensions_mut().insert(SessionFresh);
    }

    let mut response = next.run(req).await;

    // Set-Cookie on the way out for newly-created sessions. We check
    // both: the original cookie was absent/stale AND no later layer
    // already replaced it (login() sets its own Set-Cookie via the
    // response headers, which would have landed on `response` already
    // — but it ALSO inserts a SessionFresh marker via login_with_layer
    // when called inside the SessionLayer scope. For now the rule is
    // simple: if we minted the token, we set the cookie.
    if fresh {
        let cookie = set_cookie_header(&token, None);
        if let Ok(value) = cookie.parse() {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
    }
    response
}

/// Resolve the current [`AuthUser`] and stash it in the
/// [`umbra::templates::CURRENT_USER`] task-local so every `render`
/// inside this request can pick it up as `{{ user }}` — Django's
/// `request.user` shape.
///
/// Always injects a value:
/// - Authenticated session → serialize the user, augment with
///   `is_authenticated: true`.
/// - Anonymous / no session / stale cookie → the sentinel
///   `{ is_authenticated: false }`.
///
/// Cost: one `current_user` call per request. The cheap path is
/// `cookie_from_headers` + `read_session` + `AuthUser::filter().first()`
/// — three round trips. Opt in via `SessionsPlugin::with_user_in_templates`
/// when the app's request mix is HTML-heavy; leave off for REST-only
/// services so static-asset and health-check requests don't trip the
/// DB read.
pub async fn user_context_layer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let user_value = match current_user(req.headers()).await {
        Ok(Some(u)) => serialize_authenticated(&u),
        _ => anonymous_user_value(),
    };
    umbra::templates::with_current_user(Some(user_value), next.run(req)).await
}

/// Serialize an `AuthUser` into a minijinja value with
/// `is_authenticated: true` merged in. Templates can therefore call
/// `{% if user.is_authenticated %}` uniformly without checking for
/// `user == None` separately.
fn serialize_authenticated(user: &AuthUser) -> umbra::templates::Value {
    let mut json = match serde_json::to_value(user) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    json.insert(
        "is_authenticated".to_string(),
        serde_json::Value::Bool(true),
    );
    umbra::templates::Value::from_serialize(&serde_json::Value::Object(json))
}

/// Anonymous user sentinel — `{ is_authenticated: false }`. Lets
/// templates write `{% if user.is_authenticated %}` without a
/// separate `{% if user %}` guard.
fn anonymous_user_value() -> umbra::templates::Value {
    let mut json = serde_json::Map::new();
    json.insert(
        "is_authenticated".to_string(),
        serde_json::Value::Bool(false),
    );
    umbra::templates::Value::from_serialize(&serde_json::Value::Object(json))
}
