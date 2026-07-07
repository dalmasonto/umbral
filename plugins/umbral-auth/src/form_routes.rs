//! POST-only form-action auth endpoints — the redirect-style counterpart to
//! the JSON surface in [`crate::auth_routes`].
//!
//! The framework ships the **endpoints**; the developer ships their own HTML
//! pages. A developer-written login page might look like:
//!
//! ```html
//! <form method="POST" action="/auth/login?redirect=/dashboard">
//!   <input name="username"> <input name="password" type="password">
//!   <button>Sign in</button>
//! </form>
//! ```
//!
//! Each handler:
//! 1. Reads a form-encoded body.
//! 2. Runs the same auth logic as the JSON handlers (throttle check,
//!    enumeration-safe guards, etc.).
//! 3. Sets a flash message via `umbral_sessions::Messages`.
//! 4. Returns `303 See Other` — to the success target on success, to the
//!    error target (Referer then `?redirect` then `/`) on failure.
//!
//! ## Redirect safety
//!
//! All redirect targets pass through [`safe_path`]: a relative path that
//! starts with `/`, is not protocol-relative (`//`), and contains no
//! backslashes or control characters. Anything else (absolute URL, scheme,
//! `//host`) is rejected to prevent open redirects.
//!
//! - **Success target:** the `?redirect=<path>` query param if safe, else `/`.
//! - **Error target:** the `Referer` header if it is from the same host and
//!   its path is safe, else `?redirect` if safe, else `/`.

use serde::Deserialize;
use umbral::web::{Form, HeaderMap, IntoResponse, Query, Redirect, Response, Router, post};
use umbral_sessions::Messages;

// =========================================================================
// Open-redirect-safe helper
// =========================================================================

/// A redirect target is safe only if it is a same-site relative path:
/// starts with '/', is not protocol-relative ('//'), and has no backslash
/// or control chars (which browsers can normalize into a host). Anything
/// else (absolute URL, scheme, `//host`) is rejected to prevent open
/// redirects. Returns the path if safe, else None.
fn safe_path(raw: &str) -> Option<String> {
    if raw.starts_with('/')
        && !raw.starts_with("//")
        && !raw.contains('\\')
        && !raw.chars().any(|c| c.is_control())
    {
        Some(raw.to_string())
    } else {
        None
    }
}

/// Resolve the success redirect target.
///
/// Uses `?redirect=<path>` if `safe_path` accepts it, otherwise falls back
/// to the application root `/`.
fn success_target(redirect: Option<&str>) -> String {
    redirect
        .and_then(safe_path)
        .unwrap_or_else(|| "/".to_string())
}

/// Resolve the error redirect target.
///
/// Preference order:
/// 1. The `Referer` header, if the host matches the request `Host` and the
///    path is safe (returns the user to the form they were on).
/// 2. The `?redirect` query param if safe.
/// 3. The application root `/`.
///
/// **Security:** we never emit an off-site redirect. The Referer check
/// confirms that the browser's previous page was on the same origin before
/// we trust its path. The `safe_path` guard on `?redirect` prevents an
/// attacker from injecting an absolute URL through the query string.
fn error_target(headers: &HeaderMap, redirect: Option<&str>) -> String {
    // Try the Referer header first: browsers send an absolute URL like
    // `https://mysite.com/login`. We only trust it when the host matches
    // the request's own `Host` header so we stay on-site.
    if let Some(referer_path) = same_site_referer_path(headers) {
        return referer_path;
    }
    // Fall back to the ?redirect param if it's safe.
    if let Some(safe) = redirect.and_then(safe_path) {
        return safe;
    }
    "/".to_string()
}

/// Extract the path portion of the `Referer` header only when the Referer's
/// host matches the request `Host` header.
///
/// Browsers send absolute Referers (`https://site.com/login`). This function:
/// 1. Gets both the `Referer` and `Host` request headers.
/// 2. Strips the scheme prefix (`https://` or `http://`).
/// 3. Checks that the remaining string starts with the `Host` value.
/// 4. Extracts the path (everything from the first `/` after the host).
/// 5. Passes the path through `safe_path`.
///
/// Returns `None` if any step fails, ensuring we never forward to a
/// different origin.
fn same_site_referer_path(headers: &HeaderMap) -> Option<String> {
    let referer = headers
        .get(umbral::web::header::REFERER)
        .and_then(|v| v.to_str().ok())?;
    let host = headers.get("host").and_then(|v| v.to_str().ok())?.trim();

    // Strip scheme and check host match.
    let after_scheme = referer
        .strip_prefix("https://")
        .or_else(|| referer.strip_prefix("http://"))?;

    // `after_scheme` is now `host/path` or `host`. Check it starts with our host.
    if !after_scheme.starts_with(host) {
        return None;
    }

    // The character immediately after the host must be `/`, `?`, `#`, or
    // end-of-string — otherwise we have a longer hostname collision
    // (e.g. `site.com.evil.com` would start with `site.com`).
    let after_host = &after_scheme[host.len()..];
    if !after_host.is_empty() && !after_host.starts_with(['/', '?', '#']) {
        return None;
    }

    let path = if after_host.is_empty() {
        "/"
    } else {
        after_host
    };
    safe_path(path)
}

// =========================================================================
// Query param extractor
// =========================================================================

#[derive(Deserialize)]
struct RedirectQ {
    #[serde(default)]
    redirect: Option<String>,
}

// =========================================================================
// Form structs
// =========================================================================

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct SignupForm {
    username: String,
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct VerifyEmailForm {
    email: String,
    code: String,
}

#[derive(Deserialize)]
struct ResendForm {
    email: String,
}

#[derive(Deserialize)]
struct ForgotForm {
    email: String,
}

#[derive(Deserialize)]
struct ResetForm {
    token: String,
    new_password: String,
}

// =========================================================================
// Handlers
// =========================================================================

/// `POST {prefix}/login`
///
/// Form fields: `username`, `password`. Optional `?redirect=<path>`.
///
/// - Throttle-checks per IP+username before any DB work.
/// - Authenticates via [`crate::authenticate`].
/// - If `require_verified_email()` is on and `email_verified_at` is NULL,
///   treats as failure (no enumeration — same flash text as wrong password).
/// - On success: calls `login_with_request` to set the session cookie,
///   clears the throttle counter, flashes a success message, and 303s to
///   the success target.
/// - On failure: flashes an error message and 303s to the error target.
async fn do_login(
    Query(q): Query<RedirectQ>,
    headers: HeaderMap,
    msgs: Messages,
    Form(f): Form<LoginForm>,
) -> Response {
    let ip = crate::auth_routes::client_ip(&headers);

    if !crate::login_throttle_check(&ip, &f.username) {
        msgs.error("Too many attempts; please try again later.")
            .await;
        return Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response();
    }

    let user: crate::AuthUser = match crate::authenticate(&f.username, &f.password).await {
        Ok(u) => u,
        Err(_) => {
            msgs.error("Invalid username or password.").await;
            return Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response();
        }
    };

    if crate::verified_email_required() && user.email_verified_at.is_none() {
        msgs.error("Please verify your email address before signing in.")
            .await;
        return Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response();
    }

    crate::login_throttle_clear(&ip, &f.username);

    let mut resp = Redirect::to(&success_target(q.redirect.as_deref())).into_response();
    if let Err(e) = crate::login_with_request(&headers, resp.headers_mut(), &user).await {
        tracing::error!("umbral-auth form: login_with_request failed: {e}");
        msgs.error("Session error; please try again.").await;
        return Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response();
    }
    msgs.success("You have been signed in.").await;
    resp
}

/// `POST {prefix}/logout`
///
/// No form fields. Clears the session cookie and 303s to the success target.
async fn do_logout(Query(q): Query<RedirectQ>, headers: HeaderMap, msgs: Messages) -> Response {
    let mut resp = Redirect::to(&success_target(q.redirect.as_deref())).into_response();
    if let Err(e) = crate::logout(&headers, resp.headers_mut()).await {
        tracing::error!("umbral-auth form: logout session error: {e}");
    }
    msgs.success("You have been signed out.").await;
    resp
}

/// `POST {prefix}/signup`
///
/// Form fields: `username`, `email`, `password`. Optional `?redirect=<path>`.
///
/// - Register-throttle-checks per IP before any DB work.
/// - Validates the password via the ambient policy.
/// - Creates the user via [`crate::create_user`].
/// - If `require_verified_email()` is on, fires `start_email_verification`
///   best-effort (a mail failure does NOT fail registration).
/// - 303 to success or error target with a flash message.
async fn do_signup(
    Query(q): Query<RedirectQ>,
    headers: HeaderMap,
    msgs: Messages,
    Form(f): Form<SignupForm>,
) -> Response {
    let ip = crate::auth_routes::client_ip(&headers);

    if !crate::register_throttle_check(&ip) {
        msgs.error("Too many registration attempts; please try again later.")
            .await;
        return Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response();
    }

    if f.username.is_empty() || f.email.is_empty() || f.password.is_empty() {
        msgs.error("Username, email and password are required.")
            .await;
        return Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response();
    }

    if let Err(reasons) = crate::validate_password(
        &f.password,
        &crate::PasswordContext::new(Some(&f.username), Some(&f.email)),
    ) {
        msgs.error(reasons.join(" ")).await;
        return Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response();
    }

    match crate::create_user(&f.username, &f.email, &f.password).await {
        Ok(user) => {
            if crate::verified_email_required() {
                if let Err(e) = crate::start_email_verification(&user).await {
                    tracing::warn!(
                        user_id = user.id,
                        "umbral-auth form: auto-send verification on signup failed: {e}"
                    );
                }
            }
            msgs.success("Account created! You can now sign in.").await;
            Redirect::to(&success_target(q.redirect.as_deref())).into_response()
        }
        Err(e) => {
            let msg = format!("{e}");
            if msg.to_lowercase().contains("unique") {
                msgs.error("That username or email is already registered.")
                    .await;
            } else {
                msgs.error("Could not create account; please try again.")
                    .await;
            }
            Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response()
        }
    }
}

/// `POST {prefix}/verify-email`
///
/// Form fields: `email`, `code`. Optional `?redirect=<path>`.
///
/// Consumes the 6-digit verification code. 303 with flash on success or failure.
/// Throttled per IP+email (default 5 / hour) to stop online code-guessing.
async fn do_verify_email(
    Query(q): Query<RedirectQ>,
    headers: HeaderMap,
    msgs: Messages,
    Form(f): Form<VerifyEmailForm>,
) -> Response {
    let ip = crate::auth_routes::client_ip(&headers);
    if !crate::email_action_throttle_check(&ip, &f.email) {
        msgs.error("Too many requests; try again later.").await;
        return Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response();
    }
    match crate::verify_email(&f.email, &f.code).await {
        Ok(()) => {
            msgs.success("Email verified! You can now sign in.").await;
            Redirect::to(&success_target(q.redirect.as_deref())).into_response()
        }
        Err(_) => {
            msgs.error("Verification failed. The code may be expired or incorrect.")
                .await;
            Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response()
        }
    }
}

/// `POST {prefix}/resend`
///
/// Form fields: `email`.
///
/// Re-issues a verification code. ALWAYS generic flash + 303 (no enumeration).
/// Unknown addresses, already-verified users, and mail errors all produce
/// the same response as a successful resend.
/// Throttled per IP+email (default 5 / hour) to stop email-bombing.
async fn do_resend(headers: HeaderMap, msgs: Messages, Form(f): Form<ResendForm>) -> Response {
    let ip = crate::auth_routes::client_ip(&headers);
    if !crate::email_action_throttle_check(&ip, &f.email) {
        msgs.error("Too many requests; try again later.").await;
        return Redirect::to("/").into_response();
    }
    // Look up an UNVERIFIED user; fire best-effort. All errors are silently
    // swallowed — the response is always the same (no account enumeration).
    if let Ok(Some(u)) = crate::AuthUser::objects()
        .filter(
            crate::auth_user::EMAIL.eq(crate::normalize_email(&f.email))
                & crate::auth_user::EMAIL_VERIFIED_AT.is_null(),
        )
        .first()
        .await
    {
        let _ = crate::start_email_verification(&u).await;
    }
    msgs.info("If that address is registered and unverified, a new code has been sent.")
        .await;
    Redirect::to("/").into_response()
}

/// `POST {prefix}/password-forgot`
///
/// Form fields: `email`.
///
/// Issues a password-reset link. ALWAYS generic flash + 303 (no enumeration).
/// Throttled per IP+email (default 5 / hour) to stop email-bombing.
async fn do_forgot(headers: HeaderMap, msgs: Messages, Form(f): Form<ForgotForm>) -> Response {
    let ip = crate::auth_routes::client_ip(&headers);
    if !crate::email_action_throttle_check(&ip, &f.email) {
        msgs.error("Too many requests; try again later.").await;
        return Redirect::to("/").into_response();
    }
    let base = crate::auth_routes::reset_url_base(&headers);
    let _ = crate::start_password_reset(&f.email, &base).await;
    msgs.info("If that address is registered, a reset link has been sent.")
        .await;
    Redirect::to("/").into_response()
}

/// `POST {prefix}/password-reset`
///
/// Form fields: `token`, `new_password`. Optional `?redirect=<path>`.
///
/// Consumes a password-reset token. 303 with flash on success or failure.
async fn do_reset(
    Query(q): Query<RedirectQ>,
    headers: HeaderMap,
    msgs: Messages,
    Form(f): Form<ResetForm>,
) -> Response {
    match crate::reset_password(&f.token, &f.new_password).await {
        Ok(()) => {
            msgs.success("Password updated. You can now sign in with your new password.")
                .await;
            Redirect::to(&success_target(q.redirect.as_deref())).into_response()
        }
        Err(_) => {
            msgs.error("Could not reset password. The link may have expired.")
                .await;
            Redirect::to(&error_target(&headers, q.redirect.as_deref())).into_response()
        }
    }
}

// =========================================================================
// Router construction
// =========================================================================

/// Build the 7-route POST-only router under `prefix`. Called from
/// `AuthPlugin::routes()` when `with_form_routes[_at]` is set.
pub(crate) fn build_router(prefix: &str) -> Router {
    Router::new()
        .route(&format!("{prefix}/login"), post(do_login))
        .route(&format!("{prefix}/logout"), post(do_logout))
        .route(&format!("{prefix}/signup"), post(do_signup))
        .route(&format!("{prefix}/verify-email"), post(do_verify_email))
        .route(&format!("{prefix}/resend"), post(do_resend))
        .route(&format!("{prefix}/password-forgot"), post(do_forgot))
        .route(&format!("{prefix}/password-reset"), post(do_reset))
}

/// Route specs for `AuthPlugin::route_paths()` — surfaced in the dev-mode
/// 404 page so the developer sees the form surface in the route listing.
pub(crate) fn declared_routes(prefix: &str) -> Vec<umbral::routes::RouteSpec> {
    vec![
        ("POST", format!("{prefix}/login")).into(),
        ("POST", format!("{prefix}/logout")).into(),
        ("POST", format!("{prefix}/signup")).into(),
        ("POST", format!("{prefix}/verify-email")).into(),
        ("POST", format!("{prefix}/resend")).into(),
        ("POST", format!("{prefix}/password-forgot")).into(),
        ("POST", format!("{prefix}/password-reset")).into(),
    ]
}
