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

    // Write methods: cookie and header must both be present and equal.
    let header_token = req
        .headers()
        .get(CSRF_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);

    match (cookie_token, header_token) {
        (Some(c), Some(h)) if tokens_match(&c, &h) => Ok(next.run(req).await),
        _ => {
            let body = Body::from("CSRF verification failed");
            Ok(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(body)
                .expect("static response"))
        }
    }
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
