//! Login / logout / CSRF / staff-gate for the admin.
//!
//! The admin gates every non-login route through [`require_staff`].
//! Login is plain HTML-form POST; the form carries a per-session CSRF
//! token stored in the session `data` map, so the CSRF middleware's
//! double-submit-cookie scheme (which needs JS to set a custom header)
//! doesn't apply.
//!
//! `sanitise_next` rejects open-redirect attempts in the `?next=` URL
//! param so a phished link can't bounce off the login form into an
//! attacker-controlled host.

use std::collections::HashMap;

use axum::extract::Query;
use minijinja::context;
use umbra::web::{HeaderMap, IntoResponse, Redirect, Response, StatusCode};

use crate::engine::render;
use crate::util::urlencoding_simple;

// =========================================================================
// CSRF helpers for the login form.
//
// umbra-security's CSRF middleware uses double-submit-cookie with the
// `x-csrf-token` header. HTML forms can't set custom headers, so the
// login page needs its own per-session token stored in the session
// `data` map and submitted as a hidden form field.
// =========================================================================

const ADMIN_CSRF_SESSION_KEY: &str = "_umbra_admin_csrf";

/// Issue a CSRF token for the admin login form. Generates a fresh
/// token, stores it in the session `data` map, and returns the value
/// for embedding in the form. The session token must be the raw token
/// from the request cookie (used by `umbra_sessions::set_data`).
async fn issue_login_csrf(session_token: &str) -> String {
    let token = umbra_security::generate_token();
    let _ = umbra_sessions::set_data(session_token, ADMIN_CSRF_SESSION_KEY, &token).await;
    token
}

/// Verify the login form CSRF token. Returns `true` only when the
/// submitted form value equals what we stored in the session. We do
/// not need a constant-time compare: an attacker who can read the
/// session DB already has the token, so the protection is purely
/// against cross-site forms that can't see the session cookie.
async fn verify_login_csrf(session_token: &str, submitted: &str) -> bool {
    if submitted.is_empty() {
        return false;
    }
    let session = match umbra_sessions::read_session(session_token).await {
        Ok(Some(s)) => s,
        _ => return false,
    };
    match umbra_sessions::get_data::<String>(&session, ADMIN_CSRF_SESSION_KEY) {
        Ok(Some(stored)) => stored == submitted,
        _ => false,
    }
}

// =========================================================================
// Auth gate — session-based.
// =========================================================================

/// Check that the request carries a valid staff session.
///
/// On success: returns the authenticated [`umbra_auth::AuthUser`].
/// On failure: returns a `Response` that redirects to the login page
/// (307 Temporary Redirect with `?next=<requested_path>`). Non-staff
/// users get a 403 instead — the difference being that "not logged in"
/// is a recoverable state and "logged in but not staff" is not.
pub(crate) async fn require_staff(
    headers: &HeaderMap,
    current_path: &str,
) -> Result<umbra_auth::AuthUser, Response> {
    // Encode the `next` parameter: drop double-slash / external URLs.
    let next = sanitise_next(current_path);
    let login_redirect = || {
        let location = format!("/admin/login?next={}", urlencoding_simple(&next));
        Redirect::to(&location).into_response()
    };

    let user = match umbra_sessions::current_user(headers).await {
        Ok(Some(u)) => u,
        _ => return Err(login_redirect()),
    };
    if !user.is_staff {
        return Err((StatusCode::FORBIDDEN, "umbra-admin: not a staff user").into_response());
    }
    Ok(user)
}

// =========================================================================
// Login / Logout handlers.
// =========================================================================

/// `GET /admin/login` — render the login form.
///
/// If the request has no session cookie, a fresh anonymous session is
/// created and a `Set-Cookie` header is added to the response. This
/// ensures there is always a session available to anchor the CSRF token,
/// even when the `SessionsPlugin` auto-layer is disabled (the common
/// case for admin-only deployments that don't want every request to
/// create a session row).
pub(crate) async fn login_get(
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // If already logged in as staff, redirect straight to /admin/.
    if let Ok(Some(user)) = umbra_sessions::current_user(&headers).await {
        if user.is_staff {
            let next = params
                .get("next")
                .map(|n| sanitise_next(n))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "/admin/".to_string());
            return Redirect::to(&next).into_response();
        }
    }

    let next = params
        .get("next")
        .map(|n| sanitise_next(n))
        .unwrap_or_default();

    // Obtain a session token for the CSRF anchor. If the request already
    // has a valid session cookie, reuse it. Otherwise create a fresh
    // anonymous session so we have somewhere to store the CSRF token.
    let existing_token = umbra_sessions::cookie_from_headers(&headers);

    let valid_existing = if let Some(ref tok) = existing_token {
        umbra_sessions::read_session(tok)
            .await
            .ok()
            .flatten()
            .is_some()
    } else {
        false
    };

    let (session_token, new_cookie) = if valid_existing {
        (existing_token.unwrap(), None)
    } else {
        match umbra_sessions::create_session(None, None).await {
            Ok(raw) => {
                let cookie_str = umbra_sessions::set_cookie_header(&raw, None);
                (raw, Some(cookie_str))
            }
            Err(e) => {
                tracing::error!(error = %e, "admin: login_get: failed to create anonymous session");
                // Fallback: render without CSRF protection. The POST
                // will reject the empty token and redirect back here.
                let html = render(
                    "admin/login.html",
                    context!(csrf_token => "", next => next, error => "", prefill_username => ""),
                );
                return match html {
                    Ok(h) => h.into_response(),
                    Err(e2) => e2.into_response(),
                };
            }
        }
    };

    let csrf_token = issue_login_csrf(&session_token).await;

    let html = match render(
        "admin/login.html",
        context!(
            csrf_token       => csrf_token,
            next             => next,
            error            => "",
            prefill_username => "",
        ),
    ) {
        Ok(h) => h,
        Err(e) => return e.into_response(),
    };

    // If we minted a new session, attach it to the response.
    if let Some(cookie_str) = new_cookie {
        let mut resp = html.into_response();
        if let Ok(value) = cookie_str.parse::<axum::http::HeaderValue>() {
            resp.headers_mut()
                .insert(axum::http::header::SET_COOKIE, value);
        }
        resp
    } else {
        html.into_response()
    }
}

/// `POST /admin/login` — verify credentials, create session, redirect.
pub(crate) async fn login_post(headers: HeaderMap, body: String) -> Response {
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(_) => return bad_login_response("Invalid form submission.", "", ""),
    };

    let username = form.get("username").map(|s| s.as_str()).unwrap_or("");
    let password = form.get("password").map(|s| s.as_str()).unwrap_or("");
    let next_raw = form.get("next").map(|s| s.as_str()).unwrap_or("");
    let next = sanitise_next(next_raw);
    let submitted_csrf = form.get("csrf_token").map(|s| s.as_str()).unwrap_or("");

    let session_token = umbra_sessions::cookie_from_headers(&headers);
    let csrf_ok = if let Some(ref tok) = session_token {
        verify_login_csrf(tok, submitted_csrf).await
    } else {
        false
    };
    if !csrf_ok {
        let new_csrf = if let Some(ref tok) = session_token {
            issue_login_csrf(tok).await
        } else {
            String::new()
        };
        return bad_login_response_with_csrf(
            "Your session expired. Please try again.",
            username,
            &next,
            &new_csrf,
        );
    }

    // Authenticate credentials. Same error message regardless of which
    // field is wrong — timing-safe because umbra_auth hashes the
    // password unconditionally when the username matches.
    let user = match umbra_auth::authenticate::<umbra_auth::AuthUser>(username, password).await {
        Ok(u) => u,
        Err(_) => {
            let new_csrf = if let Some(ref tok) = session_token {
                issue_login_csrf(tok).await
            } else {
                String::new()
            };
            return bad_login_response_with_csrf(
                "The username or password you entered is incorrect.",
                username,
                &next,
                &new_csrf,
            );
        }
    };

    if !user.is_staff {
        let new_csrf = if let Some(ref tok) = session_token {
            issue_login_csrf(tok).await
        } else {
            String::new()
        };
        return bad_login_response_with_csrf(
            "This account does not have admin access.",
            username,
            &next,
            &new_csrf,
        );
    }

    let redirect_to = if next.is_empty() {
        "/admin/".to_string()
    } else {
        next.clone()
    };
    let mut response = Redirect::to(&redirect_to).into_response();
    if let Err(e) =
        umbra_sessions::login_with_request(&headers, response.headers_mut(), &user).await
    {
        tracing::error!(error = %e, "admin: login: session creation failed");
        return (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response();
    }
    response
}

/// Render the login template with a generic error banner.
fn bad_login_response(error: &str, prefill_username: &str, next: &str) -> Response {
    bad_login_response_with_csrf(error, prefill_username, next, "")
}

fn bad_login_response_with_csrf(
    error: &str,
    prefill_username: &str,
    next: &str,
    csrf_token: &str,
) -> Response {
    match render(
        "admin/login.html",
        context!(
            csrf_token       => csrf_token,
            next             => next,
            error            => error,
            prefill_username => prefill_username,
        ),
    ) {
        Ok(html) => (StatusCode::UNPROCESSABLE_ENTITY, html).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/logout` — destroy session, redirect to login.
pub(crate) async fn logout_handler(headers: HeaderMap) -> Response {
    let mut response = Redirect::to("/admin/login").into_response();
    let _ = umbra_sessions::logout(&headers, response.headers_mut()).await;
    response
}

// =========================================================================
// Validate the `next` redirect target.
//
// Accept only same-origin relative paths starting with `/admin/` or
// `/admin`. Reject: protocol-relative `//`, absolute `http://`, or
// anything that doesn't start with the admin prefix. The check is the
// guard against an attacker bouncing the login form off an external host.
// =========================================================================

pub(crate) fn sanitise_next(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("//") || trimmed.contains("://") {
        return "/admin/".to_string();
    }
    if !trimmed.starts_with("/admin") {
        return "/admin/".to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitise_next;

    #[test]
    fn rejects_external_urls() {
        assert_eq!(sanitise_next("http://evil.com/"), "/admin/");
        assert_eq!(sanitise_next("https://evil.com/"), "/admin/");
        assert_eq!(sanitise_next("//evil.com/"), "/admin/");
    }

    #[test]
    fn rejects_non_admin_paths() {
        assert_eq!(sanitise_next("/app/dashboard"), "/admin/");
        assert_eq!(sanitise_next("/login"), "/admin/");
    }

    #[test]
    fn accepts_admin_paths() {
        assert_eq!(sanitise_next("/admin/"), "/admin/");
        assert_eq!(sanitise_next("/admin/note/"), "/admin/note/");
        assert_eq!(sanitise_next("/admin"), "/admin");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(sanitise_next(""), "");
        assert_eq!(sanitise_next("   "), "");
    }
}
