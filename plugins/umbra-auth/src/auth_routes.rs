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

/// OpenAPI Path Item Objects for the four routes. The shapes are
/// the bare minimum the spec needs to render in Swagger UI: an
/// `operationId`, a `summary`, a `tags` entry to group them under
/// "auth", and response codes. Request bodies are documented as
/// JSON objects with the right `application/json` content type;
/// the inline schemas describe the field shapes so Swagger UI's
/// "Try it out" pane prefills sensible defaults. Closes BUG-20
/// from `bugs/tests/testBugs.md`.
pub(crate) fn openapi_paths(prefix: &str) -> Vec<(String, serde_json::Value)> {
    use serde_json::json;
    let tag = "auth";
    let register_body = json!({
        "type": "object",
        "required": ["username", "email", "password"],
        "properties": {
            "username": {"type": "string", "example": "alice"},
            "email":    {"type": "string", "format": "email", "example": "alice@example.com"},
            "password": {"type": "string", "format": "password"},
        }
    });
    let login_body = json!({
        "type": "object",
        "required": ["username", "password"],
        "properties": {
            "username": {"type": "string", "example": "alice"},
            "password": {"type": "string", "format": "password"},
        }
    });
    let user_response = json!({
        "type": "object",
        "properties": {
            "id":           {"type": "integer", "format": "int64"},
            "username":     {"type": "string"},
            "email":        {"type": "string", "format": "email"},
            "is_staff":     {"type": "boolean"},
            "is_superuser": {"type": "boolean"},
        }
    });
    let login_response = json!({
        "type": "object",
        "properties": {
            "user":  user_response.clone(),
            "token": {"type": "string", "description": "Opaque bearer token. Shown ONCE."},
        }
    });
    let error_response = json!({
        "type": "object",
        "properties": {
            "error":  {"type": "string"},
            "detail": {"type": "string"},
        }
    });

    vec![
        (
            format!("{prefix}/register"),
            json!({
                "post": {
                    "tags": [tag],
                    "operationId": "auth_register",
                    "summary": "Create a new user.",
                    "description": "Returns the user shape (no password_hash). 409 on duplicate username/email; 400 on missing fields.",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": register_body}}
                    },
                    "responses": {
                        "201": {"description": "User created.", "content": {"application/json": {"schema": user_response.clone()}}},
                        "400": {"description": "Invalid input.", "content": {"application/json": {"schema": error_response.clone()}}},
                        "409": {"description": "Username or email already exists.", "content": {"application/json": {"schema": error_response.clone()}}}
                    }
                }
            }),
        ),
        (
            format!("{prefix}/login"),
            json!({
                "post": {
                    "tags": [tag],
                    "operationId": "auth_login",
                    "summary": "Verify credentials, mint a bearer token, set a session cookie.",
                    "description": "Returns `{user, token}` and a `Set-Cookie` header. Browsers can ignore `token`; CLI / mobile can ignore the cookie.",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": login_body}}
                    },
                    "responses": {
                        "200": {"description": "Logged in.", "content": {"application/json": {"schema": login_response}}},
                        "401": {"description": "Invalid credentials.", "content": {"application/json": {"schema": error_response.clone()}}}
                    }
                }
            }),
        ),
        (
            format!("{prefix}/logout"),
            json!({
                "post": {
                    "tags": [tag],
                    "operationId": "auth_logout",
                    "summary": "Clear the session cookie + destroy the session row.",
                    "description": "Does NOT revoke bearer tokens — those stay valid until explicitly revoked.",
                    "responses": {
                        "204": {"description": "Session cleared."}
                    }
                }
            }),
        ),
        (
            format!("{prefix}/me"),
            json!({
                "get": {
                    "tags": [tag],
                    "operationId": "auth_me",
                    "summary": "Return the current user.",
                    "description": "Resolves via session cookie first, then bearer token. 401 if neither yields an active user.",
                    "responses": {
                        "200": {"description": "Authenticated user.", "content": {"application/json": {"schema": user_response}}},
                        "401": {"description": "Not authenticated.", "content": {"application/json": {"schema": error_response}}}
                    }
                }
            }),
        ),
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
/// `"login"` for admin listings. The session + cookie are written
/// via [`crate::login_with_request`], which delegates to
/// `umbra_sessions::login_user_id` for the cookie + session table
/// and then bumps `auth_user.last_login`. No duplicate session
/// code lives here.
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
    if let Err(e) = crate::login_with_request(&headers, response.headers_mut(), &user).await {
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
    let _ = umbra_sessions::logout(&headers, response.headers_mut()).await;
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
    // Identity::user_id is stringified to keep custom-PK user
    // models working; the default `AuthUser` keys by i64, so parse
    // back here. A non-numeric id means the caller wired a custom
    // user model behind /me — they should mount their own route.
    let Ok(auth_user_id) = id.user_id.parse::<i64>() else {
        return err(
            StatusCode::UNAUTHORIZED,
            "not_authenticated",
            "session user id does not match the AuthUser PK shape",
        );
    };
    let user: AuthUser = match AuthUser::objects()
        .filter(auth_user::ID.eq(auth_user_id) & auth_user::IS_ACTIVE.eq(true))
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
