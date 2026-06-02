//! Login / logout / staff-gate for the admin.
//!
//! The admin gates every non-login route through [`require_staff`].
//! Login is plain HTML-form POST; the form carries the shared CSRF
//! token from `umbra-security` — same cookie everything else in the
//! app uses, so a Django-style single token works across admin and
//! end-user routes.
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
/// Reads the shared `umbra_csrf_token` cookie via
/// [`umbra_security::current_csrf_token`] and embeds it in the form
/// as the `csrf_token` hidden input. If no cookie is set yet (first
/// request to the admin), the response carries no token and the
/// middleware mints one on the *next* GET — the user will see the
/// rendered page either way; the POST simply uses whatever cookie
/// the browser then carries back. If `SecurityPlugin` is not
/// installed, the form still posts and the admin's own validation
/// fall-back rejects empty tokens.
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

    // Same CSRF token as everything else in the app. If the cookie
    // doesn't exist yet, mint one and attach it to the response so
    // the POST can succeed on the very next click.
    let (csrf_token, new_cookie) = ensure_csrf_token(&headers);

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
///
/// CSRF is checked by comparing the submitted form field against the
/// shared `umbra_csrf_token` cookie. If `SecurityPlugin` is installed,
/// it has already done this same check before the handler runs (the
/// middleware accepts a `csrf_token` form field on POSTs); the
/// redundant check here protects the case where someone runs the
/// admin without the security middleware.
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

    let cookie_csrf = umbra_security::current_csrf_token(&headers).unwrap_or_default();
    let csrf_ok =
        !submitted_csrf.is_empty() && !cookie_csrf.is_empty() && submitted_csrf == cookie_csrf;
    if !csrf_ok {
        return bad_login_response_with_csrf(
            "Your session expired. Please try again.",
            username,
            &next,
            &cookie_csrf,
        );
    }

    // Authenticate credentials. Same error message regardless of which
    // field is wrong — timing-safe because umbra_auth hashes the
    // password unconditionally when the username matches.
    let user = match umbra_auth::authenticate::<umbra_auth::AuthUser>(username, password).await {
        Ok(u) => u,
        Err(_) => {
            return bad_login_response_with_csrf(
                "The username or password you entered is incorrect.",
                username,
                &next,
                &cookie_csrf,
            );
        }
    };

    if !user.is_staff {
        return bad_login_response_with_csrf(
            "This account does not have admin access.",
            username,
            &next,
            &cookie_csrf,
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

/// Read the CSRF token off the incoming cookie, or mint a fresh one
/// and return a Set-Cookie string the caller should attach to the
/// response so the next request carries it. Mirrors what the
/// `SecurityPlugin` middleware does on safe-method requests — handlers
/// rendering forms before the middleware has minted a token need the
/// same behaviour.
fn ensure_csrf_token(headers: &HeaderMap) -> (String, Option<String>) {
    if let Some(tok) = umbra_security::current_csrf_token(headers) {
        return (tok, None);
    }
    let tok = umbra_security::generate_token();
    let cookie = format!("umbra_csrf_token={tok}; Path=/; SameSite=Lax");
    (tok, Some(cookie))
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
