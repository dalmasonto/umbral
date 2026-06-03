// The Model derive on the crate-private SessionRow emits `pub` column
// constants whose types (chrono::DateTime, etc.) are reachable through
// the pub consts. SessionRow itself is `pub(crate)` because we do not
// want to leak it from the crate. The combination trips the
// `private_interfaces` lint, which is the wrong call for an internal
// type that only exists to drive the predicate-builder pattern.
#![allow(private_interfaces)]

//! Built-in `/auth` HTTP surface — login, logout, me, register.
//!
//! Mounted by [`crate::AuthPlugin::with_default_routes`] (only
//! available on `AuthPlugin<AuthUser>` because the handlers FK
//! into `AuthUser` directly via `AuthToken`). Apps that want a
//! custom user model bring their own routes.
//!
//! ## Surface
//!
//! | Method | Path | Body | Returns |
//! |---|---|---|---|
//! | POST | `<prefix>/register` | `{username, email, password}` | the new user (no password_hash) |
//! | POST | `<prefix>/login` | `{username, password}` | `{user, token}` and a Set-Cookie |
//! | POST | `<prefix>/logout` | — | 204 + clear-cookie |
//! | GET  | `<prefix>/me` | — | the current user (session OR bearer) |
//!
//! Prefix defaults to `/api/auth`; override via
//! [`crate::AuthPlugin::with_default_routes_at`].
//!
//! ## What login returns
//!
//! Both shapes at once: a `Set-Cookie` for browsers AND a fresh
//! bearer token in the JSON body for CLI / mobile / CI clients.
//! The caller picks which it cares about. The minted token is
//! named `"login"` so it shows up identifiably in admin "your
//! tokens" listings.
//!
//! ## What is deliberately missing
//!
//! - Password reset — couples to a mail crate; lands as its own
//!   plugin when there's a real consumer.
//! - Throttling / lockout — production hardening; wrong layer.
//! - Email verification on register — workflow varies per app.
//! - `/token` (issue / list / revoke) — admin surface, separate.

use crate::token::AuthToken;
use crate::{AuthUser, OptionalIdentity, auth_user};
use serde::{Deserialize, Serialize};
use umbra::orm::Manager;
use umbra::web::{HeaderMap, IntoResponse, Json, Response, Router, StatusCode, post};

// =========================================================================
// Wire-shape DTOs. AuthUser carries password_hash; we never want that
// in any response, so register / login / me all serialise via UserOut.
// =========================================================================

#[derive(Debug, Deserialize)]
struct RegisterIn {
    username: String,
    email: String,
    password: String,
}

#[derive(Debug, Deserialize)]
struct LoginIn {
    username: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct UserOut {
    id: i64,
    username: String,
    email: String,
    is_staff: bool,
    is_superuser: bool,
}

impl From<&AuthUser> for UserOut {
    fn from(u: &AuthUser) -> Self {
        Self {
            id: u.id,
            username: u.username.clone(),
            email: u.email.clone(),
            is_staff: u.is_staff,
            is_superuser: u.is_superuser,
        }
    }
}

#[derive(Debug, Serialize)]
struct LoginOut {
    user: UserOut,
    token: String,
}

#[derive(Debug, Serialize)]
struct ErrorOut {
    error: &'static str,
    detail: String,
}

fn err(status: StatusCode, error: &'static str, detail: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorOut {
            error,
            detail: detail.into(),
        }),
    )
        .into_response()
}

// =========================================================================
// Router construction
// =========================================================================

/// Build the four-route Router under `prefix`. Called from
/// `AuthPlugin::routes()` when `with_default_routes()` is on.
pub(crate) fn build_router(prefix: &str) -> Router {
    Router::new()
        .route(&format!("{prefix}/register"), post(register))
        .route(&format!("{prefix}/login"), post(login))
        .route(&format!("{prefix}/logout"), post(logout))
        .route(&format!("{prefix}/me"), umbra::web::get(me))
}

/// Same as [`build_router`] but also returns the route specs the
/// `AuthPlugin::route_paths()` impl forwards to the dev-mode 404
/// page so the developer sees the auth surface in the route
/// listing.
pub(crate) fn declared_routes(prefix: &str) -> Vec<umbra::routes::RouteSpec> {
    vec![
        ("POST", format!("{prefix}/register")).into(),
        ("POST", format!("{prefix}/login")).into(),
        ("POST", format!("{prefix}/logout")).into(),
        ("GET", format!("{prefix}/me")).into(),
    ]
}

// =========================================================================
// Handlers
// =========================================================================

/// `POST {prefix}/register` — create a new user.
///
/// JSON `{username, email, password}` → 201 with the user shape
/// (no password_hash). 400 on missing fields. 409 on duplicate
/// `username` / `email` — the `UNIQUE` constraints on those
/// columns (gap #65) raise a sqlx error containing the keyword
/// "unique", which this branch translates to the 409 status.
async fn register(Json(body): Json<RegisterIn>) -> Response {
    if body.username.is_empty() || body.email.is_empty() || body.password.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_input",
            "username, email and password are required",
        );
    }
    match crate::create_user(&body.username, &body.email, &body.password).await {
        Ok(user) => (StatusCode::CREATED, Json(UserOut::from(&user))).into_response(),
        Err(e) => {
            let msg = format!("{e}");
            let status = if msg.to_lowercase().contains("unique") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            err(status, "create_failed", msg)
        }
    }
}

/// `POST {prefix}/login` — verify credentials, set a session
/// cookie, mint a fresh bearer token.
///
/// Returns `{user, token}` plus a Set-Cookie. The token is named
/// `"login"` for admin listings.
async fn login(headers: HeaderMap, Json(body): Json<LoginIn>) -> Response {
    let user: AuthUser = match crate::authenticate(&body.username, &body.password).await {
        Ok(u) => u,
        Err(_) => {
            return err(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "username or password is incorrect",
            );
        }
    };
    let (_token_row, plaintext) = match AuthToken::create_for(&user, "login").await {
        Ok(t) => t,
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "token_failed",
                format!("{e}"),
            );
        }
    };
    let body = LoginOut {
        user: UserOut::from(&user),
        token: plaintext.0,
    };
    let mut response = Json(body).into_response();
    // umbra-auth can't depend on umbra-sessions (the dep arrow runs
    // the other way), so we re-implement the cookie-set step.
    // Mirrors `umbra_sessions::login_with_request`'s session-
    // fixation defense: destroy any inbound anonymous session
    // before minting the authenticated one.
    if let Err(e) = create_session_cookie(&headers, response.headers_mut(), &user).await {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session_failed",
            format!("{e}"),
        );
    }
    response
}

/// `POST {prefix}/logout` — clear the session cookie + destroy
/// the row. 204. Does NOT revoke bearer tokens.
async fn logout(headers: HeaderMap) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let _ = clear_session_cookie(&headers, response.headers_mut()).await;
    response
}

/// `GET {prefix}/me` — return the current user.
///
/// Resolves via `OptionalIdentity` (session-first, then bearer).
/// 401 if neither yields an active user; 200 with the user shape
/// otherwise.
async fn me(OptionalIdentity(id): OptionalIdentity) -> Response {
    let Some(id) = id else {
        return err(
            StatusCode::UNAUTHORIZED,
            "not_authenticated",
            "send a session cookie or a Bearer token",
        );
    };
    let user: AuthUser = match AuthUser::objects()
        .filter(auth_user::ID.eq(id.user_id) & auth_user::IS_ACTIVE.eq(true))
        .first()
        .await
    {
        Ok(Some(u)) => u,
        Ok(None) => {
            return err(
                StatusCode::UNAUTHORIZED,
                "not_authenticated",
                "user record went away between auth and lookup",
            );
        }
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                format!("{e}"),
            );
        }
    };
    Json(UserOut::from(&user)).into_response()
}

// =========================================================================
// Cookie / session bridge.
//
// We do NOT call umbra_sessions directly — that crate depends on
// umbra-auth, so the import is forbidden by the dep arrow. The
// session row lives in the `session` table that umbra-sessions
// owns; we read/write through the ORM against a private struct that
// mirrors its schema. The cookie name + hashing scheme match
// umbra-sessions (sha256 of the raw token) so the browser cookie
// minted here is interchangeable with one minted by
// umbra-sessions::login. The session table is migrated by
// SessionsPlugin; if the user didn't add that plugin, these calls
// fail-soft (no row to write, error swallowed).
// =========================================================================

const COOKIE_NAME: &str = "umbra_session";
/// 14 days — matches Django's `SESSION_COOKIE_AGE` default and
/// umbra-sessions' default.
const DEFAULT_TTL_SECONDS: i64 = 14 * 24 * 60 * 60;

// The Model derive emits `pub` column constants that reference
// this struct's type. Marking the struct itself `pub` would leak
// it; `pub(crate)` keeps it crate-private but the consts still
// trip the `private_interfaces` lint. The `#[allow]` is the cleanest
// resolution — the constants are never used outside this crate,
// they just exist to satisfy the predicate-builder pattern (which
// expects `<Field>::<COL>.eq(x)`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "session")]
#[allow(private_interfaces)]
pub(crate) struct SessionRow {
    pub id: String,
    pub user_id: Option<String>,
    pub data: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

fn hash_token(raw: &str) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(raw.as_bytes());
    format!("{:x}", h.finalize())
}

fn cookie_from_headers(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(http::header::COOKIE)?.to_str().ok()?;
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(&format!("{COOKIE_NAME}=")) {
            return Some(value.to_string());
        }
    }
    None
}

async fn destroy_session(token: &str) {
    let stored_id = hash_token(token);
    let _ = Manager::<SessionRow>::default()
        .filter(session_row::ID.eq(stored_id))
        .delete()
        .await;
}

async fn create_session_cookie(
    request_headers: &HeaderMap,
    response_headers: &mut HeaderMap,
    user: &AuthUser,
) -> Result<(), crate::AuthError> {
    // Session-fixation defense: if the request carried an anonymous
    // session, destroy that row before minting a new authenticated
    // one. The new token won't equal the old one because we generate
    // fresh randomness.
    if let Some(old) = cookie_from_headers(request_headers) {
        destroy_session(&old).await;
    }
    let raw_token = uuid::Uuid::new_v4().to_string();
    let stored_id = hash_token(&raw_token);
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::seconds(DEFAULT_TTL_SECONDS);
    Manager::<SessionRow>::default()
        .create(SessionRow {
            id: stored_id,
            user_id: Some(user.id.to_string()),
            data: "{}".to_string(),
            created_at: now,
            expires_at,
        })
        .await?;
    let cookie = format!(
        "{COOKIE_NAME}={raw_token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={DEFAULT_TTL_SECONDS}"
    );
    response_headers.insert(
        http::header::SET_COOKIE,
        cookie.parse().expect("cookie value parses"),
    );
    // Best-effort last_login bump. Failure here doesn't invalidate
    // the login (cookie is already set, session row already exists).
    let mut patch = serde_json::Map::new();
    patch.insert(
        "last_login".to_string(),
        serde_json::to_value(now).unwrap_or(serde_json::Value::Null),
    );
    let _ = AuthUser::objects()
        .filter(auth_user::ID.eq(user.id))
        .update_values(patch)
        .await;
    Ok(())
}

async fn clear_session_cookie(
    request_headers: &HeaderMap,
    response_headers: &mut HeaderMap,
) -> Result<(), crate::AuthError> {
    if let Some(token) = cookie_from_headers(request_headers) {
        destroy_session(&token).await;
    }
    // Max-Age=0 + a far-past expires; the cookie name has to match
    // exactly for the browser to forget the old value.
    let cookie = format!("{COOKIE_NAME}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0");
    response_headers.insert(
        http::header::SET_COOKIE,
        cookie.parse().expect("cookie value parses"),
    );
    Ok(())
}
