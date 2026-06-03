//! Reference auth endpoints layered on top of `umbra-auth` +
//! `umbra-sessions` + `umbra-rest`. JSON-only, deliberately small,
//! mirrors the Django shape.
//!
//! Lives in the example app — *not* the auth plugin — because the
//! shape of "what does the response look like, do you also issue a
//! bearer on login, do you ship register" is application policy.
//! Treat this file as a copy-pasteable starting point.
//!
//! ## Surface
//!
//! | Method | Path | Body | Returns |
//! |---|---|---|---|
//! | POST | `/api/auth/register` | `{username, email, password}` | the new user (no password_hash) |
//! | POST | `/api/auth/login` | `{username, password}` | `{user, token}` and a Set-Cookie |
//! | POST | `/api/auth/logout` | — | 204 + clear-cookie |
//! | GET  | `/api/auth/me` | — | the current user (session OR bearer) |
//!
//! ## What login returns
//!
//! Both shapes at once: a `Set-Cookie` for the browser AND a fresh
//! bearer token in the JSON body for the CLI / mobile / CI case.
//! The caller picks which it cares about. The token is named
//! `"login"` so it shows up identifiably in admin "your tokens"
//! listings.
//!
//! ## What is intentionally missing
//!
//! - Password reset (couples to a mail crate; punt to a dedicated
//!   plugin).
//! - Throttling / lockout (production hardening; an example app
//!   shouldn't pretend to ship that securely).
//! - Email-verification on register (workflow varies wildly per
//!   app — wrap `create_user` yourself).

use serde::{Deserialize, Serialize};
use umbra::web::{HeaderMap, IntoResponse, Json, Response, StatusCode};
use umbra_auth::{AuthToken, AuthUser, auth_user, parse_bearer_header};

// =========================================================================
// Wire-shape DTOs. AuthUser carries password_hash; we never want that
// in any response, so register / login / me all serialise via UserOut.
// =========================================================================

#[derive(Debug, Deserialize)]
pub struct RegisterIn {
    pub username: String,
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginIn {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct UserOut {
    pub id: i64,
    pub username: String,
    pub email: String,
    pub is_staff: bool,
    pub is_superuser: bool,
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
pub struct LoginOut {
    pub user: UserOut,
    /// The plaintext bearer token, returned ONCE. Save it client-side
    /// (env var, keychain) — it is not recoverable from the
    /// `auth_token` row.
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorOut {
    pub error: &'static str,
    pub detail: String,
}

fn err(status: StatusCode, error: &'static str, detail: impl Into<String>) -> Response {
    (status, Json(ErrorOut { error, detail: detail.into() })).into_response()
}

// =========================================================================
// Resolve current user from EITHER the session cookie OR a bearer
// token. The route handlers below could lean on the RestPlugin's
// `ChainAuthentication` wiring, but /me is a custom (non-CRUD)
// handler so it walks the two paths explicitly — and it doubles as
// a worked example of "how do I check who the caller is from a
// hand-written handler."
// =========================================================================

pub async fn resolve_current_user(headers: &HeaderMap) -> Option<AuthUser> {
    // Session cookie first — cheaper for browsers (one fewer row
    // touched than the token path, since current_user already
    // performs the user JOIN).
    if let Some(user) = umbra_sessions::current_user(headers).await.ok().flatten() {
        return Some(user);
    }
    // Then bearer token. Two indexed lookups + an is_active guard.
    let plaintext = parse_bearer_header(headers)?;
    let token = AuthToken::lookup(plaintext).await.ok().flatten()?;
    AuthUser::objects()
        .filter(auth_user::ID.eq(token.user_id.id()) & auth_user::IS_ACTIVE.eq(true))
        .first()
        .await
        .ok()
        .flatten()
}

// =========================================================================
// Handlers
// =========================================================================

/// `POST /api/auth/register` — create a new user.
///
/// JSON shape: `{username, email, password}`. Returns the created
/// user on 201; 400 on duplicate username / invalid input; 500 on a
/// database error.
///
/// Doesn't auto-login. Django ships register the same way: account
/// creation and session creation are different policy decisions
/// (some apps email-verify before activating the session, some
/// don't).
pub async fn register(Json(body): Json<RegisterIn>) -> Response {
    // Cheap input guard. The DB UNIQUE on username would catch the
    // empty case via the conflict-error branch below, but a clean
    // 400 here is friendlier than a generic create error.
    if body.username.is_empty() || body.email.is_empty() || body.password.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_input",
            "username, email and password are required",
        );
    }
    // Defensive UNIQUE pre-check. The `auth_user` schema doesn't
    // emit a UNIQUE constraint on `username` at the column level yet
    // (framework gap — `#[umbra(unique)]` is on the roadmap), so a
    // raw `create_user` call would silently produce a duplicate.
    // This is the "do it in the handler until the schema catches up"
    // shape — drop the pre-check once UNIQUE lands.
    match AuthUser::objects()
        .filter(auth_user::USERNAME.eq(&body.username))
        .exists()
        .await
    {
        Ok(true) => {
            return err(
                StatusCode::CONFLICT,
                "username_taken",
                format!("a user named {:?} already exists", body.username),
            );
        }
        Ok(false) => {}
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                format!("{e}"),
            );
        }
    }
    match umbra_auth::create_user(&body.username, &body.email, &body.password).await {
        Ok(user) => (StatusCode::CREATED, Json(UserOut::from(&user))).into_response(),
        Err(e) => {
            // The auth helper doesn't classify conflicts — surface
            // the underlying string so a duplicate username is
            // visible to the caller without leaking internals.
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

/// `POST /api/auth/login` — verify credentials, set a session
/// cookie, mint a fresh bearer token.
///
/// JSON shape: `{username, password}`. Returns
/// `{user, token}` and a `Set-Cookie` header on 200; 401 on bad
/// credentials.
///
/// Issues both shapes in one response so the same endpoint serves
/// browsers (which only care about the cookie) and curl / mobile
/// (which only care about the token). The token is named `"login"`
/// for admin listings.
pub async fn login(headers: HeaderMap, Json(body): Json<LoginIn>) -> Response {
    let user: AuthUser = match umbra_auth::authenticate(&body.username, &body.password).await {
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
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, "token_failed", format!("{e}")),
    };
    let body = LoginOut { user: UserOut::from(&user), token: plaintext.0 };
    let mut response = Json(body).into_response();
    // `login_with_request` performs the session-fixation defense:
    // if the request carried an anonymous session, that row is
    // destroyed before the new authenticated session is created.
    if let Err(e) = umbra_sessions::login_with_request(
        &headers,
        response.headers_mut(),
        &user,
    )
    .await
    {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "session_failed", format!("{e}"));
    }
    response
}

/// `POST /api/auth/logout` — clear the session cookie + destroy
/// the session row. Returns 204.
///
/// Does NOT revoke a bearer token if one was sent — bearer
/// revocation is a separate explicit step (`AuthToken::revoke` from
/// an admin action or a dedicated endpoint). Logout-revokes-bearer
/// would mean a single Set-Cookie request silently kills CLI access
/// from a totally unrelated terminal, which is surprising.
pub async fn logout(headers: HeaderMap) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let _ = umbra_sessions::logout(&headers, response.headers_mut()).await;
    response
}

/// `GET /api/auth/me` — return the current user.
///
/// Accepts either a session cookie or `Authorization: Bearer …`
/// (or both — session takes precedence). 401 if neither resolves
/// to an active user. 200 otherwise.
pub async fn me(headers: HeaderMap) -> Response {
    match resolve_current_user(&headers).await {
        Some(user) => Json(UserOut::from(&user)).into_response(),
        None => err(
            StatusCode::UNAUTHORIZED,
            "not_authenticated",
            "send a session cookie or a Bearer token",
        ),
    }
}
