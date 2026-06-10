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
        let base = crate::branding::current().base_path;
        let location = format!("{base}/login?next={}", urlencoding_simple(&next));
        Redirect::to(&location).into_response()
    };

    let user = match umbra_auth::current_user(headers).await {
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
/// Embeds the shared CSRF token in the form as the `csrf_token`
/// hidden input, resolved by [`ensure_csrf_token`]: with
/// `SecurityPlugin` mounted the middleware minted it *before* this
/// handler ran (first visit included) and the ambient value is used;
/// without the plugin the admin reads the cookie or self-mints, and
/// its own validation in [`login_post`] is what rejects bad tokens.
pub(crate) async fn login_get(
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // If already logged in as staff, redirect straight to /admin/.
    if let Ok(Some(user)) = umbra_auth::current_user(&headers).await {
        if user.is_staff {
            let next = params
                .get("next")
                .map(|n| sanitise_next(n))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("{}/", crate::branding::current().base_path));
            return Redirect::to(&next).into_response();
        }
    }

    let next = params
        .get("next")
        .map(|n| sanitise_next(n))
        .unwrap_or_default();

    // Same CSRF token as everything else in the app — ambient when
    // SecurityPlugin is mounted; self-minted (with the Set-Cookie
    // attach below) only when it isn't.
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
    // Constant-time comparison — short-circuit `==` leaks token-prefix
    // matches over time (same rule the middleware follows).
    let csrf_ok = !submitted_csrf.is_empty()
        && !cookie_csrf.is_empty()
        && umbra_security::tokens_match(submitted_csrf, &cookie_csrf);
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
        format!("{}/", crate::branding::current().base_path)
    } else {
        next.clone()
    };
    let mut response = Redirect::to(&redirect_to).into_response();
    if let Err(e) = umbra_auth::login_with_request(&headers, response.headers_mut(), &user).await {
        tracing::error!(error = %e, "admin: login: session creation failed");
        return (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response();
    }
    response
}

/// Resolve the CSRF token for an admin-rendered form.
///
/// 1. Ambient (`umbra::templates::current_csrf()`): `SecurityPlugin`
///    is mounted — its middleware already minted (and, on first visit,
///    set) the token before this handler ran. The middleware owns the
///    cookie; the admin sets nothing.
/// 2. Cookie fallback, then self-mint: no `SecurityPlugin`. The admin
///    stays self-protecting — `login_post`'s own comparison is the
///    only validator in this mode, so the mint here is what makes the
///    login form work at all.
fn ensure_csrf_token(headers: &HeaderMap) -> (String, Option<String>) {
    if let Some(tok) = umbra::templates::current_csrf() {
        return (tok, None);
    }
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
    let base = crate::branding::current().base_path;
    let mut response = Redirect::to(&format!("{base}/login")).into_response();
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
    let base = crate::branding::current().base_path;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("//") || trimmed.contains("://") {
        return format!("{base}/");
    }
    if !trimmed.starts_with(&*base) {
        return format!("{base}/");
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

#[cfg(test)]
mod csrf_tests {
    use super::*;

    #[tokio::test]
    async fn ensure_csrf_token_prefers_the_ambient_token() {
        let headers = HeaderMap::new();
        let (tok, cookie) =
            umbra::templates::with_current_csrf(Some("ambient-token".to_string()), async {
                ensure_csrf_token(&headers)
            })
            .await;
        assert_eq!(tok, "ambient-token");
        assert!(
            cookie.is_none(),
            "middleware owns the cookie; admin must not set one"
        );
    }

    #[tokio::test]
    async fn ensure_csrf_token_self_mints_without_middleware() {
        let headers = HeaderMap::new();
        let (tok, cookie) = ensure_csrf_token(&headers);
        assert!(!tok.is_empty());
        assert!(
            cookie.is_some(),
            "no middleware, no cookie: admin must self-mint"
        );
    }
}
