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
use umbral::web::{HeaderMap, IntoResponse, Json, Response, Router, StatusCode, post};

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

// New DTOs for the verification + password-reset surface (Task 10).
#[derive(Debug, Deserialize)]
struct VerifyEmailIn {
    email: String,
    code: String,
}

#[derive(Debug, Deserialize)]
struct EmailOnlyIn {
    email: String,
}

#[derive(Debug, Deserialize)]
struct ResetIn {
    token: String,
    new_password: String,
}

/// Resolve the client IP best-effort from reverse-proxy headers. ConnectInfo
/// isn't wired in umbral's serve path, so the peer address isn't available; the
/// proxy headers are the reliable source. Takes the first hop of
/// `X-Forwarded-For`, else `X-Real-IP`. When neither resolves (direct
/// connection, no proxy), falls back to a fixed key so the throttle still
/// counts — every un-proxied caller shares one bucket, which is the safe side:
/// it limits, it never opens a hole. Mirrors `umbral_logs`'s `resolve_ip`.
pub(crate) fn client_ip(headers: &HeaderMap) -> String {
    // audit_2 H9: resolve the client IP under the framework's trusted-proxy
    // policy (`settings.trusted_proxy_hops`) instead of blindly trusting the
    // forgeable leftmost `X-Forwarded-For`. When no trusted proxy is configured
    // (the default) or the chain is spoofed, this yields `None` → the shared
    // "unknown" bucket: every un-attributable caller is limited TOGETHER, which
    // never opens a hole, rather than each getting their own bucket via a header
    // they control.
    umbral::settings::client_ip(headers).unwrap_or_else(|| "unknown".to_string())
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

/// Build the request origin's reset URL base from reverse-proxy headers.
///
/// Prefers `X-Forwarded-Proto` (default `"https"`) + the `Host` header to
/// build `{proto}://{host}/auth/reset`. The `/auth/reset` page is owned by
/// the HTML auth surface (Task 14); the JSON password-forgot endpoint points
/// the email there so the user clicks through to the confirmation form.
///
/// Falls back to the relative path `"/auth/reset"` when the `Host` header is
/// absent (e.g. a test client that doesn't set it).
///
/// ## Security: why trusting `Host` is safe here
///
/// Reading the `Host` header to build an absolute URL is normally a
/// *host-header injection* / *password-reset poisoning* risk (CWE-640): an
/// attacker supplies a `Host: evil.com` header, the server echoes it into the
/// reset link, and the victim's click goes to the attacker's server.
///
/// This risk is eliminated upstream, before this function is ever reached.
/// In **production** mode the framework mounts a host-guard layer during
/// `App::build` (Phase 5.95 in `crates/umbral-core/src/app.rs`): any request
/// whose `Host` header is not listed in `settings.allowed_hosts` is rejected
/// with HTTP 400 before any handler runs. By the time execution reaches
/// `password_forgot_h` → `reset_url_base`, the `Host` value has already been
/// validated against the operator-configured allowlist, so embedding it in the
/// reset URL is safe.
///
/// In **non-production** (dev) mode, host validation is intentionally disabled
/// so that `localhost` and `127.0.0.1` work without any extra configuration.
/// The reset URL will reflect whatever `Host` the client sends — acceptable in
/// a local dev environment where the only callers are the developer themselves.
pub(crate) fn reset_url_base(headers: &HeaderMap) -> String {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim());
    let Some(host) = host else {
        return "/auth/reset".to_string();
    };
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .unwrap_or("https");
    format!("{proto}://{host}/auth/reset")
}

#[derive(serde::Deserialize)]
struct ChangePasswordIn {
    current_password: String,
    new_password: String,
}

/// `POST {prefix}/change-password` — `{current_password, new_password}` for the
/// authenticated caller (session OR bearer). Verifies the current password,
/// enforces the strength policy on the new one, rotates the hash. `204` on
/// success; `401` unauthenticated; `400 invalid_credentials` (wrong current) /
/// `400 weak_password` (policy).
async fn change_password_h(
    OptionalIdentity(id): OptionalIdentity,
    Json(body): Json<ChangePasswordIn>,
) -> Response {
    let Some(id) = id else {
        return err(
            StatusCode::UNAUTHORIZED,
            "not_authenticated",
            "send a session cookie or a Bearer token",
        );
    };
    let Ok(uid) = id.user_id.parse::<i64>() else {
        return err(
            StatusCode::UNAUTHORIZED,
            "not_authenticated",
            "session user id does not match the AuthUser PK shape",
        );
    };
    let user: AuthUser = match AuthUser::objects()
        .filter(auth_user::ID.eq(uid) & auth_user::IS_ACTIVE.eq(true))
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
            tracing::error!(error = %e, "change-password: user lookup failed");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "internal error",
            );
        }
    };
    match crate::change_password(&user, &body.current_password, &body.new_password).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(crate::AuthError::InvalidCredentials) => err(
            StatusCode::BAD_REQUEST,
            "invalid_credentials",
            "current password is incorrect",
        ),
        Err(crate::AuthError::WeakPassword(reasons)) => {
            err(StatusCode::BAD_REQUEST, "weak_password", reasons.join(" "))
        }
        Err(e) => {
            tracing::error!(error = %e, "change-password: rotate failed");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "internal error",
            )
        }
    }
}

/// Build the four-route Router under `prefix`. Called from
/// `AuthPlugin::routes()` when `with_default_routes()` is on.
pub(crate) fn build_router(prefix: &str) -> Router {
    // Register every route at BOTH the bare and trailing-slash form (gaps3
    // #11). REST resources end in `/` and the scaffold turns on
    // `SlashRedirect::Append`, so a client naturally tries `/api/auth/login/`
    // — but Append only redirects a *no-slash* request TO the slash form, so
    // registering only the no-slash form left the slash form 404ing. Binding
    // both makes either path work regardless of the app's redirect policy.
    Router::new()
        .route(&format!("{prefix}/register"), post(register))
        .route(&format!("{prefix}/register/"), post(register))
        .route(&format!("{prefix}/login"), post(login))
        .route(&format!("{prefix}/login/"), post(login))
        .route(&format!("{prefix}/logout"), post(logout))
        .route(&format!("{prefix}/logout/"), post(logout))
        .route(&format!("{prefix}/me"), umbral::web::get(me))
        .route(&format!("{prefix}/me/"), umbral::web::get(me))
        .route(
            &format!("{prefix}/change-password"),
            post(change_password_h),
        )
        .route(
            &format!("{prefix}/change-password/"),
            post(change_password_h),
        )
        .route(&format!("{prefix}/verify-email"), post(verify_email_h))
        .route(&format!("{prefix}/verify-email/"), post(verify_email_h))
        .route(
            &format!("{prefix}/resend-verification"),
            post(resend_verification_h),
        )
        .route(
            &format!("{prefix}/resend-verification/"),
            post(resend_verification_h),
        )
        .route(
            &format!("{prefix}/password-forgot"),
            post(password_forgot_h),
        )
        .route(
            &format!("{prefix}/password-forgot/"),
            post(password_forgot_h),
        )
        .route(&format!("{prefix}/password-reset"), post(password_reset_h))
        .route(&format!("{prefix}/password-reset/"), post(password_reset_h))
}

/// Same as [`build_router`] but also returns the route specs the
/// `AuthPlugin::route_paths()` impl forwards to the dev-mode 404
/// page so the developer sees the auth surface in the route
/// listing.
pub(crate) fn declared_routes(prefix: &str) -> Vec<umbral::routes::RouteSpec> {
    vec![
        ("POST", format!("{prefix}/register")).into(),
        ("POST", format!("{prefix}/login")).into(),
        ("POST", format!("{prefix}/logout")).into(),
        ("GET", format!("{prefix}/me")).into(),
        ("POST", format!("{prefix}/verify-email")).into(),
        ("POST", format!("{prefix}/resend-verification")).into(),
        ("POST", format!("{prefix}/password-forgot")).into(),
        ("POST", format!("{prefix}/password-reset")).into(),
    ]
}

/// OpenAPI Path Item Objects for the eight auth routes (register, login,
/// logout, me, verify-email, resend-verification, password-forgot,
/// password-reset). The shapes are the bare minimum the spec needs to render
/// in Swagger UI: an `operationId`, a `summary`, a `tags` entry to group
/// them under "auth", and response codes. Request bodies are documented as
/// JSON objects with the right `application/json` content type; the inline
/// schemas describe the field shapes so Swagger UI's "Try it out" pane
/// prefills sensible defaults. Closes BUG-20 from `bugs/tests/testBugs.md`.
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
                        "401": {"description": "Not authenticated.", "content": {"application/json": {"schema": error_response.clone()}}}
                    }
                }
            }),
        ),
        (
            format!("{prefix}/verify-email"),
            json!({
                "post": {
                    "tags": [tag],
                    "operationId": "auth_verify_email",
                    "summary": "Verify an email address with a 6-digit code.",
                    "description": "JSON `{email, code}` → 204 on success. 400 (generic) on any failure (unknown email, no active challenge, wrong code, attempt cap) — no enumeration.",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": json!({
                            "type": "object",
                            "required": ["email", "code"],
                            "properties": {
                                "email": {"type": "string", "format": "email"},
                                "code":  {"type": "string", "example": "483920"}
                            }
                        })}}
                    },
                    "responses": {
                        "204": {"description": "Email verified."},
                        "400": {"description": "Invalid or expired code.", "content": {"application/json": {"schema": error_response.clone()}}}
                    }
                }
            }),
        ),
        (
            format!("{prefix}/resend-verification"),
            json!({
                "post": {
                    "tags": [tag],
                    "operationId": "auth_resend_verification",
                    "summary": "Re-issue an email-verification code.",
                    "description": "JSON `{email}` → always 202. Unknown emails and already-verified users receive the same response as a pending user (no enumeration). The verification mail is sent best-effort.",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": json!({
                            "type": "object",
                            "required": ["email"],
                            "properties": {
                                "email": {"type": "string", "format": "email"}
                            }
                        })}}
                    },
                    "responses": {
                        "202": {"description": "Request accepted (mail sent if the address is known and unverified)."}
                    }
                }
            }),
        ),
        (
            format!("{prefix}/password-forgot"),
            json!({
                "post": {
                    "tags": [tag],
                    "operationId": "auth_password_forgot",
                    "summary": "Issue a password-reset link.",
                    "description": "JSON `{email}` → always 202. Unknown emails receive the same response as known ones (no enumeration). The reset link is sent best-effort.",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": json!({
                            "type": "object",
                            "required": ["email"],
                            "properties": {
                                "email": {"type": "string", "format": "email"}
                            }
                        })}}
                    },
                    "responses": {
                        "202": {"description": "Request accepted (reset link sent if the address matches a known account)."}
                    }
                }
            }),
        ),
        (
            format!("{prefix}/password-reset"),
            json!({
                "post": {
                    "tags": [tag],
                    "operationId": "auth_password_reset",
                    "summary": "Consume a password-reset token.",
                    "description": "JSON `{token, new_password}` → 204 on success. 400 (generic) on any failure (unknown / expired / already-used token, weak password).",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": json!({
                            "type": "object",
                            "required": ["token", "new_password"],
                            "properties": {
                                "token":        {"type": "string", "description": "Opaque reset token from the emailed link."},
                                "new_password": {"type": "string", "format": "password"}
                            }
                        })}}
                    },
                    "responses": {
                        "204": {"description": "Password updated."},
                        "400": {"description": "Invalid, expired, or already-used token; or weak password.", "content": {"application/json": {"schema": error_response}}}
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
async fn register(headers: HeaderMap, Json(body): Json<RegisterIn>) -> Response {
    // Throttle BEFORE any DB work — defends mass automated account creation.
    // Keyed per IP (no username yet at register time). 429 once the IP has
    // burned its budget (default 10 / hour).
    let ip = client_ip(&headers);
    if !crate::register_throttle_check(&ip) {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many registration attempts; try again later",
        );
    }
    if body.username.is_empty() || body.email.is_empty() || body.password.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_input",
            "username, email and password are required",
        );
    }
    // Enforce the password-strength policy HERE, at the registration boundary —
    // this is the untrusted surface (a client submitting a password) and the
    // single point we validate (routes / views, not `create_user`). The
    // low-level `create_user` is intentionally non-validating so seed scripts
    // and the test suite aren't broken by it. `validate_password` reads the
    // ambiently-installed policy, so `AuthPlugin::disable_password_validation`
    // (which installs an empty policy) makes this a no-op automatically — no
    // separate flag to thread through.
    if let Err(reasons) = crate::validate_password(
        &body.password,
        &crate::PasswordContext::new(Some(&body.username), Some(&body.email)),
    ) {
        // A weak password is a client error, not a server error: 400 with the
        // full list of reasons so a form can render each one.
        return err(StatusCode::BAD_REQUEST, "weak_password", reasons.join(" "));
    }
    match crate::create_user(&body.username, &body.email, &body.password).await {
        Ok(user) => {
            // Auto-send a verification code when the gate is active. Best-effort:
            // a mail failure must NOT fail the registration — the user account is
            // already created and the code can be re-issued via /resend-verification.
            if crate::verified_email_required() {
                if let Err(e) = crate::start_email_verification(&user).await {
                    tracing::warn!(
                        user_id = user.id,
                        "umbral-auth: require_verified_email: auto-send on register failed: {e}"
                    );
                }
            }
            (StatusCode::CREATED, Json(UserOut::from(&user))).into_response()
        }
        // audit_2 plugin-auth #4: the argon2 gate shed this registration under
        // load — 503 so the client retries later instead of into the flood.
        Err(crate::AuthError::Overloaded) => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
            "the server is busy; please retry shortly",
        ),
        Err(e) => {
            // Log the raw error server-side; NEVER echo it to the client. The
            // raw `AuthError`/sqlx Display leaks DB driver / schema / column
            // details to an unauthenticated caller (audit plugin-auth #5).
            tracing::error!(error = %e, "umbral-auth: register create_user failed");
            let msg = format!("{e}");
            let status = if msg.to_lowercase().contains("unique") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            err(status, "create_failed", "could not create account")
        }
    }
}

/// `POST {prefix}/login` — verify credentials, set a session
/// cookie, mint a fresh bearer token.
///
/// Returns `{user, token}` plus a Set-Cookie. The token is named
/// `"login"` for admin listings. The session + cookie are written
/// via [`crate::login_with_request`], which delegates to
/// `umbral_sessions::login_user_id` for the cookie + session table
/// and then bumps `auth_user.last_login`. No duplicate session
/// code lives here.
async fn login(headers: HeaderMap, Json(body): Json<LoginIn>) -> Response {
    // Throttle BEFORE touching the DB — defends credential stuffing / brute
    // force. Keyed per IP + username. The SAME 429 is returned regardless of
    // whether the account exists, so this never leaks account existence. The
    // check ALSO records this attempt; a successful login below forgives the
    // counter so a legit user's earlier typo doesn't lock them out.
    let ip = client_ip(&headers);
    if !crate::login_throttle_check(&ip, &body.username) {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many login attempts; try again later",
        );
    }
    let user: AuthUser = match crate::authenticate(&body.username, &body.password).await {
        Ok(u) => u,
        // audit_2 plugin-auth #4: the argon2 gate shed this request under load.
        // 503 (not 401) so clients back off instead of retrying into the flood.
        Err(crate::AuthError::Overloaded) => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "overloaded",
                "the server is busy; please retry shortly",
            );
        }
        Err(_) => {
            // The failed attempt is already counted by the check above.
            return err(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "username or password is incorrect",
            );
        }
    };
    // Authenticated: forgive the counter so prior typos don't accumulate.
    crate::login_throttle_clear(&ip, &body.username);
    // Gate: if require_verified_email is on and the user hasn't verified yet,
    // block login. The 403 (not 401) distinguishes "good credentials, missing
    // step" from "bad credentials", so clients can surface actionable feedback.
    if crate::verified_email_required() && user.email_verified_at.is_none() {
        return err(
            StatusCode::FORBIDDEN,
            "email_not_verified",
            "verify your email before logging in",
        );
    }
    let (_token_row, plaintext) = match AuthToken::create_for(&user, "login").await {
        Ok(t) => t,
        Err(e) => {
            // Log server-side; return static text (audit plugin-auth #5).
            tracing::error!(error = %e, "umbral-auth: login token mint failed");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "token_failed",
                "could not complete login",
            );
        }
    };
    let body = LoginOut {
        user: UserOut::from(&user),
        token: plaintext.0,
    };
    let mut response = Json(body).into_response();
    if let Err(e) = crate::login_with_request(&headers, response.headers_mut(), &user).await {
        // Log server-side; return static text (audit plugin-auth #5).
        tracing::error!(error = %e, "umbral-auth: login session write failed");
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session_failed",
            "could not complete login",
        );
    }
    response
}

/// `POST {prefix}/logout` — clear the session cookie + destroy
/// the row. 204. Does NOT revoke bearer tokens.
///
/// Delegates to [`crate::logout`], the single reusable logout that both
/// built-in surfaces and custom handlers share. On error the route still
/// returns 204 (the client-side cookie is always cleared) and logs the
/// session-layer failure at error level.
async fn logout(headers: HeaderMap) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Err(e) = crate::logout(&headers, response.headers_mut()).await {
        tracing::error!("umbral-auth: logout session error: {e}");
    }
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
            // Log server-side; return static text (audit plugin-auth #5).
            tracing::error!(error = %e, "umbral-auth: /me user lookup failed");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "could not load user",
            );
        }
    };
    Json(UserOut::from(&user)).into_response()
}

// =========================================================================
// Task 10: verify-email, resend-verification, password-forgot, password-reset
// =========================================================================

/// `POST {prefix}/verify-email` — consume a 6-digit email-verification code.
///
/// JSON `{email, code}` → 204 on success; 400 (generic, no enumeration) on
/// any failure (unknown email, no active challenge, wrong code, attempt cap).
/// Throttled per IP+email (default 5 / hour) to stop online code-guessing.
async fn verify_email_h(headers: HeaderMap, Json(b): Json<VerifyEmailIn>) -> Response {
    let ip = client_ip(&headers);
    if !crate::email_action_throttle_check(&ip, &b.email) {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many requests; try again later",
        );
    }
    match crate::verify_email(&b.email, &b.code).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => err(
            StatusCode::BAD_REQUEST,
            "invalid_code",
            "verification failed",
        ),
    }
}

/// `POST {prefix}/resend-verification` — re-issue an email-verification code.
///
/// JSON `{email}` → always 202 (no enumeration: unknown emails or already-
/// verified users get the same response as an unverified user who gets the
/// mail). Fires `start_email_verification` best-effort for unverified users.
/// Throttled per IP+email (default 5 / hour) to stop email-bombing.
async fn resend_verification_h(headers: HeaderMap, Json(b): Json<EmailOnlyIn>) -> Response {
    let ip = client_ip(&headers);
    if !crate::email_action_throttle_check(&ip, &b.email) {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many requests; try again later",
        );
    }
    // Look up an UNVERIFIED user by email. `is_null()` matches SQL `IS NULL`
    // on the nullable `email_verified_at` column. The filter intentionally
    // excludes already-verified users so the mail is only sent when it
    // matters. All error arms are silently swallowed — the response is always
    // 202 regardless (no account enumeration through this endpoint).
    if let Ok(Some(u)) = AuthUser::objects()
        .filter(auth_user::EMAIL.eq(b.email.clone()) & auth_user::EMAIL_VERIFIED_AT.is_null())
        .first()
        .await
    {
        let _ = crate::start_email_verification(&u).await;
    }
    StatusCode::ACCEPTED.into_response()
}

/// `POST {prefix}/password-forgot` — issue a password-reset link.
///
/// JSON `{email}` → always 202 (no enumeration: unknown emails get the same
/// response as known ones). Fires `start_password_reset` best-effort; the
/// reset URL base is built from the request's `Host` /
/// `X-Forwarded-Proto` headers.
/// Throttled per IP+email (default 5 / hour) to stop email-bombing.
async fn password_forgot_h(headers: HeaderMap, Json(b): Json<EmailOnlyIn>) -> Response {
    let ip = client_ip(&headers);
    if !crate::email_action_throttle_check(&ip, &b.email) {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many requests; try again later",
        );
    }
    let base = reset_url_base(&headers);
    let _ = crate::start_password_reset(&b.email, &base).await;
    StatusCode::ACCEPTED.into_response()
}

/// `POST {prefix}/password-reset` — consume a password-reset token.
///
/// JSON `{token, new_password}` → 204 on success; 400 (generic) on any
/// failure (unknown / expired / already-used token, weak password).
async fn password_reset_h(Json(b): Json<ResetIn>) -> Response {
    match crate::reset_password(&b.token, &b.new_password).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => err(
            StatusCode::BAD_REQUEST,
            "reset_failed",
            "could not reset password",
        ),
    }
}
