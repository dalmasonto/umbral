//! umbra-security — CSRF protection and security headers.
//!
//! Django's `CsrfViewMiddleware` plus the `SecurityMiddleware`
//! header bundle, the small slice that matters for v0. Plug it
//! into the app and every non-safe request must carry a matching
//! CSRF token; every response gets a standard set of hardening
//! headers.
//!
//! ```ignore
//! App::builder()
//!     .plugin(AuthPlugin::new())
//!     .plugin(SecurityPlugin::new())   // wraps the auth-augmented router
//!     .build()
//!     .await?;
//! ```
//!
//! ## CSRF
//!
//! Double-submit cookie pattern:
//!
//! 1. Every GET / HEAD / OPTIONS without the `umbra_csrf_token`
//!    cookie gets one set on the response. The cookie is NOT
//!    HttpOnly: the page's JS reads it and copies it into a header
//!    on later writes.
//! 2. Every POST / PUT / PATCH / DELETE must include the cookie
//!    AND a `X-CSRF-Token` header whose value matches it. A
//!    mismatch returns 403.
//!
//! The token is a 32-byte cryptographically-random value, hex-
//! encoded. Per-session rotation is deferred until umbra-sessions
//! grows the hook for it.
//!
//! ## Headers
//!
//! The default header set:
//!
//! - `X-Content-Type-Options: nosniff`
//! - `X-Frame-Options: DENY`
//! - `Referrer-Policy: strict-origin-when-cross-origin`
//!
//! `Strict-Transport-Security` is opt-in via
//! [`SecurityPlugin::with_hsts`]: it can break local development on
//! `http://` if accidentally enabled in dev.
//!
//! ## Why this lives in Plugin::wrap_router
//!
//! Layering middleware needs a `tower::Layer` value, which is
//! generic and erasing it into a `Box<dyn ...>` clashes with axum's
//! handler trait. The Plugin trait's `wrap_router(Router) -> Router`
//! method sidesteps that: each plugin layers its own middleware on
//! the router with the full axum / tower API and returns the
//! wrapped router. The app builder calls it in topological order so
//! security wraps everything declared before it.

use std::convert::Infallible;

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use http::header::{COOKIE, HeaderName, HeaderValue, SET_COOKIE};
use http::{Method, StatusCode};
use tower_http::set_header::SetResponseHeaderLayer;
use umbra::prelude::*;

const CSRF_COOKIE: &str = "umbra_csrf_token";
const CSRF_HEADER: &str = "x-csrf-token";
/// Form field name that carries the CSRF token for HTML `<form>`
/// submissions. Two shapes are accepted — `csrf_token` and `__csrf`
/// — so existing form code on either convention works without
/// migration. The header path stays the canonical one for JS clients.
const CSRF_FORM_FIELDS: &[&str] = &["csrf_token", "__csrf"];
/// Hard cap on the buffered body size when we peek at form data to
/// extract the CSRF field. 1 MiB is well above any realistic
/// urlencoded form (the login page is < 1 KiB) and well below
/// anything that would justify a memory pressure concern.
const MAX_FORM_BODY: usize = 1024 * 1024;

/// CSRF + security-headers plugin. Configure via [`Self::with_hsts`]
/// (off by default).
#[derive(Debug, Default, Clone)]
pub struct SecurityPlugin {
    hsts: bool,
}

impl SecurityPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable `Strict-Transport-Security: max-age=31536000; includeSubDomains`.
    /// Leave off for local dev; turn on once the production deploy
    /// is HTTPS-only.
    pub fn with_hsts(mut self, hsts: bool) -> Self {
        self.hsts = hsts;
        self
    }
}

impl Plugin for SecurityPlugin {
    fn name(&self) -> &'static str {
        "security"
    }

    fn wrap_router(&self, router: Router) -> Router {
        let mut router = router.layer(middleware::from_fn(csrf_middleware));

        router = router.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ));
        router = router.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ));
        router = router.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ));
        if self.hsts {
            router = router.layer(SetResponseHeaderLayer::if_not_present(
                HeaderName::from_static("strict-transport-security"),
                HeaderValue::from_static("max-age=31536000; includeSubDomains"),
            ));
        }
        router
    }
}

/// Generate a fresh 32-byte token, hex-encoded. Public so tests
/// and downstream code that mints tokens directly (e.g. server-
/// rendered forms) can share the same shape.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("getrandom failed");
    hex::encode(bytes)
}

/// Pull the value of a named cookie out of a `Cookie` header. v0
/// shape: linear scan, no quoting. The full RFC 6265 grammar lives
/// in tower-cookies, which we deliberately don't depend on yet.
fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    for part in header.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            if k == name {
                return Some(v);
            }
        }
    }
    None
}

fn is_safe_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

async fn csrf_middleware(req: Request, next: Next) -> Result<Response, Infallible> {
    let method = req.method().clone();
    let cookie_token = req
        .headers()
        .get(COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| cookie_value(h, CSRF_COOKIE).map(str::to_string));

    if is_safe_method(&method) {
        // Pass through, but mint and attach a token if the client
        // doesn't have one yet so the next write request can succeed.
        let mut response = next.run(req).await;
        if cookie_token.is_none() {
            let token = generate_token();
            let cookie = format!("{CSRF_COOKIE}={token}; Path=/; SameSite=Lax");
            if let Ok(v) = HeaderValue::from_str(&cookie) {
                response.headers_mut().insert(SET_COOKIE, v);
            }
        }
        return Ok(response);
    }

    // Write methods: cookie and (header OR form field) must match.
    //
    // Header is the canonical path for JS clients that can set custom
    // headers. HTML forms can't set those, so we also peek into a
    // urlencoded body for `csrf_token` (or `__csrf`). The peek
    // buffers the entire body up to MAX_FORM_BODY then rebuilds the
    // request so the downstream handler still sees a complete body.
    let header_token = req
        .headers()
        .get(CSRF_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);

    if let Some(c) = cookie_token.as_ref() {
        if let Some(h) = header_token.as_ref() {
            if tokens_match(c, h) {
                return Ok(next.run(req).await);
            }
        }
        // Try the form-field path.
        let content_type = req
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if content_type.starts_with("application/x-www-form-urlencoded") {
            let cookie_owned = c.clone();
            let (parts, body) = req.into_parts();
            let bytes = match axum::body::to_bytes(body, MAX_FORM_BODY).await {
                Ok(b) => b,
                Err(_) => return Ok(forbidden()),
            };
            let submitted = form_field_token(&bytes);
            if let Some(s) = submitted {
                if tokens_match(&cookie_owned, &s) {
                    let req = Request::from_parts(parts, Body::from(bytes));
                    return Ok(next.run(req).await);
                }
            }
        }
    }

    Ok(forbidden())
}

fn forbidden() -> Response {
    let body = Body::from("CSRF verification failed");
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .body(body)
        .expect("static response")
}

/// Scan a urlencoded form body for any of the accepted CSRF field
/// names. Plain string scan — we avoid pulling in a full
/// query-string parser for one field.
fn form_field_token(body: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(body).ok()?;
    for part in s.split('&') {
        let mut iter = part.splitn(2, '=');
        let key = iter.next()?;
        let val = iter.next().unwrap_or("");
        if CSRF_FORM_FIELDS.contains(&key) {
            // urlencoded forms percent-encode + use `+` for spaces.
            // The token is hex though, so neither character appears
            // and a no-op decode is fine for the common case. Fall
            // back to identity if `urlencoding` isn't available.
            let decoded = val.replace('+', " ");
            // No real percent-decoding library used here on purpose;
            // tokens are hex (no special chars) so the raw value
            // matches the cookie. Future-proof when token format
            // changes by routing through a proper decoder.
            return Some(decoded);
        }
    }
    None
}

/// Read the current CSRF token from the request's cookie header.
/// Public so handlers that render HTML forms can embed it as a
/// hidden `csrf_token` input — the form POST is then validated by
/// the same middleware path JS clients hit with the header.
pub fn current_csrf_token(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| cookie_value(h, CSRF_COOKIE).map(str::to_string))
}

/// Constant-time string equality. Short-circuit `==` on `String` is
/// a timing side-channel — successive equal-prefix bytes take longer
/// to fail than mismatched-first-byte comparisons, leaking
/// prefix-match progress to an attacker who can measure latency.
///
/// CSRF tokens aren't credentials so the exploitability bar is high
/// (you'd need a chosen-token timing oracle, which is rare in real
/// deployments), but constant-time comparison costs nothing and
/// closes the gap unconditionally. Per OWASP's "Use Constant-Time
/// String Comparison" rule applied to all security tokens.
fn tokens_match(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    // Length mismatch fails in constant time too — `ct_eq` on
    // different-length slices short-circuits to a 0 mask before
    // doing the byte loop, so the timing leak is bounded to "the
    // lengths differ" which is information an attacker already has
    // (they pick the header value).
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
