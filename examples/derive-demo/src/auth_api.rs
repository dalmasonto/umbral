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
use umbra_auth::{AuthToken, AuthUser, OptionalIdentity, auth_user};

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
// /me reads the current user via `OptionalIdentity` from umbra-auth.
// That extractor runs the same session-then-bearer chain the
// RestPlugin uses internally; here we wire it from a custom route so
// the surface stays uniform. Identity carries `user_id` + `is_staff`
// + an `extras` map — enough for /me, and a second SELECT on
// `AuthUser::objects().filter(...)` only happens when the response
// needs the email / username (which it does, see below).
// =========================================================================

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
    // Database UNIQUE on `username` and `email` (gap #65 — set
    // via `#[umbra(unique)]` on the AuthUser model) is the
    // source of truth for "already taken". The conflict surfaces
    // here as a sqlx error whose message contains the word
    // "unique" — the error branch below translates that to 409.
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
///
/// `OptionalIdentity` runs the same chain the RestPlugin uses
/// internally and gives us the `user_id` + `is_staff` flag for
/// free; we then SELECT the full row to populate `email` and
/// `username` for the response. A `CurrentIdentity` extractor
/// would replace the inner Option match with a 401 rejection at
/// extractor time, but we want the JSON-shaped error body, so
/// the manual match stays.
pub async fn me(OptionalIdentity(id): OptionalIdentity) -> Response {
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
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, "lookup_failed", format!("{e}")),
    };
    Json(UserOut::from(&user)).into_response()
}
