//! umbral-security — CSRF protection and a configurable security-header bundle.
//!
//! CSRF protection plus a security-header bundle,
//! widened to the modern header set. Plug it into the app and every non-safe
//! request must carry a matching CSRF token; every response gets the hardening
//! headers you've enabled.
//!
//! ```ignore
//! App::builder()
//!     .plugin(AuthPlugin::new())
//!     .plugin(SecurityPlugin::new())   // secure-but-dev-safe defaults
//!     .build()
//!     .await?;
//! ```
//!
//! ## Configuration is a struct, not a builder chain
//!
//! Construct a [`SecurityConfig`] (every field has a secure, dev-safe default)
//! and flip exactly what you need — no long `.with_x().with_y()` chain:
//!
//! ```ignore
//! SecurityPlugin::with_config(SecurityConfig {
//!     hsts: true,
//!     content_security_policy: Some("default-src 'self'".into()),
//!     server_header: Some("umbral".into()),
//!     request_body_limit: Some(2 * 1024 * 1024),
//!     ..Default::default()
//! })
//! ```
//!
//! `SecurityPlugin::new()` keeps the defaults; `SecurityPlugin::with_hsts(true)`
//! stays as a one-flag convenience.
//!
//! ## CSRF
//!
//! Signed double-submit cookie pattern, fully automatic (see
//! `docs/decisions/2026-06-10-automatic-csrf.md`):
//!
//! 1. **The middleware is the only mint.** On GET / HEAD / OPTIONS it mints a
//!    token *before* the handler runs (first visit covered) and appends the
//!    `umbral_csrf_token` cookie to the response. The cookie is NOT HttpOnly:
//!    the page's JS reads it and copies it into a header on later writes.
//! 2. **Templates get the token for free.** The token is scoped into
//!    `umbral::templates::CURRENT_CSRF` around every non-exempt request, so
//!    any rendered template can write `{{ csrf_input }}` (the full hidden
//!    input) or `{{ csrf_token }}` (raw value, for `X-CSRF-Token` headers /
//!    htmx `hx-headers`). View code never touches CSRF.
//! 3. Every POST / PUT / PATCH / DELETE must include the cookie AND a matching
//!    `X-CSRF-Token` header (JS path) or `csrf_token` / `__csrf` form field
//!    (HTML-form path). A mismatch returns 403. On success the token stays in
//!    scope so a validation-error re-render still carries it into the form.
//!
//! The token is a 32-byte CSPRNG value, hex-encoded. The CSRF cookie gains
//! `Secure` automatically under `Environment::Prod` (or force it with
//! [`SecurityConfig::csrf_cookie_secure`]).
//!
//! ### Signed / session-bound CSRF ([`SecurityConfig::signed_csrf`])
//!
//! Naive double-submit trusts the cookie: an attacker who can plant a cookie on
//! a sibling subdomain can forge a matching token. `signed_csrf` (**default
//! on**) makes the token `<random>.<HMAC-SHA256(secret_key, random[.session])>`
//! — a forged cookie can't carry a valid signature without the app
//! `secret_key`. Set [`SecurityConfig::session_bind_cookie`] to also fold the
//! session cookie's value into the signature so a token minted under one
//! session can't be replayed under another.
//!
//! The flip to default-on is deploy-safe because the middleware **rotates**
//! any cookie token that can't pass signed-mode validation on the next safe
//! request (browsers holding pre-upgrade unsigned cookies converge instead of
//! 403ing), and because no other mint exists: the admin prefers the ambient
//! middleware token and only self-mints when this plugin isn't mounted. With
//! no resolvable `secret_key` (tests, pre-`App::build()` renders) minting and
//! validation degrade to plain double-submit instead of locking writes out.
//! Opt back into plain double-submit with `signed_csrf: false`.
//!
//! ## Headers
//!
//! Enabled by default: `X-Content-Type-Options: nosniff`, `X-Frame-Options:
//! DENY`, `Referrer-Policy: strict-origin-when-cross-origin`, `X-XSS-Protection:
//! 0` (modern guidance disables the legacy auditor), `Cross-Origin-Opener-Policy:
//! same-origin`, and a `Server: umbral` header. Opt-in
//! (default off, each a field on [`SecurityConfig`]): `Strict-Transport-Security`,
//! `Content-Security-Policy`, `Permissions-Policy`, `Cross-Origin-Resource-Policy`,
//! `Cross-Origin-Embedder-Policy`. CSP and HSTS are off by default because a wrong
//! value breaks apps (HSTS bricks `http://` dev; a strict CSP breaks the CDN-using
//! admin).
//!
//! ## Server identity & tower-http knobs
//!
//! [`SecurityConfig::server_header`] sets the `Server` header (prefer a bare
//! product name — a version is an information-disclosure tradeoff);
//! [`SecurityConfig::hide_server_header`] strips whatever the stack added.
//! [`SecurityConfig::request_body_limit`] caps the request body via tower-http's
//! `RequestBodyLimitLayer` (DoS hardening); [`SecurityConfig::redact_sensitive_headers`]
//! (default on) marks `authorization` / `cookie` / `set-cookie` sensitive so
//! they're redacted in tracing output.
//!
//! ## Why this lives in Plugin::wrap_router
//!
//! Layering middleware needs a `tower::Layer` value; the Plugin trait's
//! `wrap_router(Router) -> Router` lets each plugin layer its middleware with
//! the full axum / tower API. The app builder calls it in topological order so
//! security wraps everything declared before it.

use std::convert::Infallible;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;
use http::header::{AUTHORIZATION, COOKIE, HeaderName, HeaderValue, SERVER, SET_COOKIE};
use http::{Method, StatusCode};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::sensitive_headers::SetSensitiveHeadersLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use umbral::prelude::*;

const CSRF_COOKIE: &str = "umbral_csrf_token";
const CSRF_HEADER: &str = "x-csrf-token";
/// Form field name that carries the CSRF token for HTML `<form>` submissions.
/// Two shapes are accepted — `csrf_token` and `__csrf` — so existing form code
/// on either convention works without migration. The header path stays the
/// canonical one for JS clients.
const CSRF_FORM_FIELDS: &[&str] = &["csrf_token", "__csrf"];
/// Hard cap on the buffered body size when we peek at form data to extract the
/// CSRF field. 1 MiB is well above any realistic urlencoded form.
const MAX_FORM_BODY: usize = 1024 * 1024;

/// Declarative security configuration. Build from [`Default`] (secure,
/// dev-safe) and override the fields you need — see the crate docs for the
/// rationale behind each default.
#[derive(Debug, Clone)]
pub struct SecurityConfig {
    // ---- CSRF ----
    /// Run the CSRF middleware. Default `true`.
    pub csrf: bool,
    /// Force the `Secure` flag on the CSRF cookie. Default `false`; `Secure` is
    /// added automatically under `Environment::Prod` regardless, so this only
    /// matters for forcing it on in a non-prod HTTPS setup.
    pub csrf_cookie_secure: bool,
    /// Sign the CSRF token with the app `secret_key` (HMAC-SHA256). Default
    /// `true` — the middleware is the only mint, so every token carries a
    /// signature; stale unsigned cookies rotate automatically on the next
    /// safe request. Set `false` for plain double-submit.
    pub signed_csrf: bool,
    /// When `signed_csrf` is on, also bind the token to this cookie's value
    /// (typically the session cookie). Default `None`.
    pub session_bind_cookie: Option<String>,
    /// Request-path prefixes exempt from CSRF (CSRF-exempt paths).
    /// A token-authenticated REST API carries no session cookie, so a
    /// bearer-auth `POST /api/...` would otherwise 403; exempt `"/api"` to
    /// keep it working. Matched as a path prefix. Default empty.
    pub csrf_exempt_paths: Vec<String>,

    // ---- Response headers (None / false = header omitted) ----
    /// `X-Content-Type-Options: nosniff`. Default `true`.
    pub content_type_options: bool,
    /// `X-Frame-Options`. Default `Some("DENY")`.
    pub frame_options: Option<String>,
    /// `Referrer-Policy`. Default `Some("strict-origin-when-cross-origin")`.
    pub referrer_policy: Option<String>,
    /// `X-XSS-Protection`. Default `Some("0")` — disables the buggy legacy
    /// filter rather than enabling it (current OWASP guidance).
    pub xss_protection: Option<String>,
    /// Emit `Strict-Transport-Security`. Default `false` (dev-safe). Value is
    /// built from the `hsts_*` fields.
    pub hsts: bool,
    /// HSTS `max-age` in seconds. Default one year.
    pub hsts_max_age: u64,
    /// Add `; includeSubDomains` to HSTS. Default `true`.
    pub hsts_include_subdomains: bool,
    /// Add `; preload` to HSTS. Default `false`.
    pub hsts_preload: bool,
    /// `Content-Security-Policy`. Default `None` — a wrong CSP breaks apps, so
    /// it's opt-in.
    pub content_security_policy: Option<String>,
    /// `Permissions-Policy`. Default `None`.
    pub permissions_policy: Option<String>,
    /// `Cross-Origin-Opener-Policy`. Default `Some("same-origin")`.
    /// Set `None` to omit, e.g. apps relying on cross-origin
    /// popups (some OAuth flows).
    pub cross_origin_opener_policy: Option<String>,
    /// `Cross-Origin-Resource-Policy` (e.g. `"same-origin"`). Default `None`.
    pub cross_origin_resource_policy: Option<String>,
    /// `Cross-Origin-Embedder-Policy` (e.g. `"require-corp"`). Default `None`.
    pub cross_origin_embedder_policy: Option<String>,

    // ---- Server identity ----
    /// Set the `Server` response header. Default `Some("umbral")` — a bare
    /// product name (no version, so no info disclosure), the way many app
    /// servers advertise one. Set `None` to omit. Prefer no version.
    pub server_header: Option<String>,
    /// Strip any `Server` header the stack set. Default `false`. Ignored when
    /// `server_header` is `Some` (the set wins) — to strip, also set
    /// `server_header: None`.
    pub hide_server_header: bool,

    // ---- axum / tower-http knobs ----
    /// Cap request body size in bytes (tower-http `RequestBodyLimitLayer`).
    /// Default `None` (axum's own default applies).
    pub request_body_limit: Option<usize>,
    /// Mark `authorization` / `cookie` / `set-cookie` sensitive so tracing
    /// redacts them. Default `true`.
    pub redact_sensitive_headers: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            csrf: true,
            csrf_cookie_secure: false,
            signed_csrf: true,
            session_bind_cookie: None,
            csrf_exempt_paths: Vec::new(),
            content_type_options: true,
            frame_options: Some("DENY".to_string()),
            referrer_policy: Some("strict-origin-when-cross-origin".to_string()),
            xss_protection: Some("0".to_string()),
            hsts: false,
            hsts_max_age: 31_536_000,
            hsts_include_subdomains: true,
            hsts_preload: false,
            content_security_policy: None,
            permissions_policy: None,
            // On by default (same-origin). Isolates the browsing
            // context group; only affects apps that rely on cross-origin popups.
            cross_origin_opener_policy: Some("same-origin".to_string()),
            cross_origin_resource_policy: None,
            cross_origin_embedder_policy: None,
            // Advertise the framework (no version, no info disclosure). Many app
            // servers emit a `Server` header. Set `None` to omit
            // or pair `None` + `hide_server_header` to strip an upstream one.
            server_header: Some("umbral".to_string()),
            hide_server_header: false,
            request_body_limit: None,
            redact_sensitive_headers: true,
        }
    }
}

impl SecurityConfig {
    fn hsts_value(&self) -> String {
        let mut v = format!("max-age={}", self.hsts_max_age);
        if self.hsts_include_subdomains {
            v.push_str("; includeSubDomains");
        }
        if self.hsts_preload {
            v.push_str("; preload");
        }
        v
    }
}

/// CSRF + security-headers plugin. Configure via [`SecurityConfig`].
#[derive(Debug, Clone, Default)]
pub struct SecurityPlugin {
    config: SecurityConfig,
}

impl SecurityPlugin {
    /// Secure, dev-safe defaults (see [`SecurityConfig`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct from an explicit config — the preferred entry point.
    pub fn with_config(config: SecurityConfig) -> Self {
        Self { config }
    }

    /// Borrow the active config.
    pub fn config(&self) -> &SecurityConfig {
        &self.config
    }

    /// One-flag convenience for `SecurityConfig::hsts`. Equivalent to
    /// `with_config(SecurityConfig { hsts, ..Default::default() })`.
    pub fn with_hsts(mut self, hsts: bool) -> Self {
        self.config.hsts = hsts;
        self
    }
}

impl Plugin for SecurityPlugin {
    fn name(&self) -> &'static str {
        "security"
    }

    fn wrap_router(&self, router: Router) -> Router {
        let cfg = &self.config;
        let mut router = router;

        // CSRF middleware (innermost of our additions).
        if cfg.csrf {
            let state = CsrfState::from_config(cfg);
            router = router.layer(middleware::from_fn_with_state(state, csrf_middleware));
        }

        // Response-header setters. Order among them is irrelevant.
        if cfg.content_type_options {
            router = set_header(
                router,
                "x-content-type-options",
                Some("nosniff".to_string()),
            );
        }
        router = set_header(router, "x-frame-options", cfg.frame_options.clone());
        router = set_header(router, "referrer-policy", cfg.referrer_policy.clone());
        router = set_header(router, "x-xss-protection", cfg.xss_protection.clone());
        if cfg.hsts {
            router = set_header(router, "strict-transport-security", Some(cfg.hsts_value()));
        }
        router = set_header(
            router,
            "content-security-policy",
            cfg.content_security_policy.clone(),
        );
        router = set_header(router, "permissions-policy", cfg.permissions_policy.clone());
        router = set_header(
            router,
            "cross-origin-opener-policy",
            cfg.cross_origin_opener_policy.clone(),
        );
        router = set_header(
            router,
            "cross-origin-resource-policy",
            cfg.cross_origin_resource_policy.clone(),
        );
        router = set_header(
            router,
            "cross-origin-embedder-policy",
            cfg.cross_origin_embedder_policy.clone(),
        );

        // Server identity: an explicit value overrides; otherwise optionally strip.
        if let Some(v) = cfg.server_header.as_deref() {
            if let Ok(hv) = HeaderValue::from_str(v) {
                router = router.layer(SetResponseHeaderLayer::overriding(SERVER, hv));
            }
        } else if cfg.hide_server_header {
            router = router.layer(middleware::from_fn(strip_server_header));
        }

        // tower-http knobs (outermost so they wrap everything above).
        if cfg.redact_sensitive_headers {
            router = router.layer(SetSensitiveHeadersLayer::new([
                AUTHORIZATION,
                COOKIE,
                SET_COOKIE,
            ]));
        }
        if let Some(limit) = cfg.request_body_limit {
            router = router.layer(RequestBodyLimitLayer::new(limit));
        }

        router
    }

    fn on_ready(
        &self,
        _ctx: &umbral::plugin::AppContext,
    ) -> Result<(), umbral::plugin::PluginError> {
        let settings = umbral::settings::get_opt();

        // Boot nudge: HSTS and CSP are opt-in (safe defaults for dev), but
        // a Prod deployment shipping neither is a real exposure — SSL
        // stripping with no HSTS, XSS with no CSP backstop. Warn loudly so
        // the gap is visible at startup rather than discovered in an audit.
        let is_prod = settings
            .map(|s| matches!(s.environment, Environment::Prod))
            .unwrap_or(false);
        if is_prod {
            if !self.config.hsts {
                tracing::warn!(
                    "SecurityPlugin: HSTS is disabled in Environment::Prod — responses ship \
                     no Strict-Transport-Security header, leaving clients open to SSL \
                     stripping. Enable with `.with_hsts(true)`."
                );
            }
            if self.config.content_security_policy.is_none() {
                tracing::warn!(
                    "SecurityPlugin: no Content-Security-Policy set in Environment::Prod — \
                     XSS has no CSP backstop. Set `content_security_policy` in SecurityConfig."
                );
            }
        }

        check_secret_key(settings, &self.config)?;

        Ok(())
    }
}

/// Add a `SetResponseHeaderLayer::if_not_present` for `name` when `value` is a
/// valid header value; otherwise return the router untouched.
fn set_header(router: Router, name: &'static str, value: Option<String>) -> Router {
    match value.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
        Some(hv) => router.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static(name),
            hv,
        )),
        None => router,
    }
}

/// Per-request CSRF state captured at `wrap_router` time. The `secret` is read
/// once from settings (absent in tests / before `App::build()` — signing then
/// degrades to plain double-submit rather than panicking).
#[derive(Clone)]
struct CsrfState {
    secure: bool,
    signed: bool,
    secret: Option<String>,
    session_cookie: Option<String>,
    exempt_paths: Vec<String>,
}

impl CsrfState {
    fn from_config(cfg: &SecurityConfig) -> Self {
        let settings = umbral::settings::get_opt();
        let is_prod = settings
            .map(|s| matches!(s.environment, Environment::Prod))
            .unwrap_or(false);
        let secret = if cfg.signed_csrf {
            settings
                .map(|s| s.secret_key.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        };
        Self {
            secure: cfg.csrf_cookie_secure || is_prod,
            signed: cfg.signed_csrf,
            secret,
            session_cookie: cfg.session_bind_cookie.clone(),
            exempt_paths: cfg.csrf_exempt_paths.clone(),
        }
    }

    /// True when `path` falls under a configured CSRF-exempt prefix.
    fn is_exempt(&self, path: &str) -> bool {
        self.exempt_paths.iter().any(|prefix| {
            let prefix = prefix.trim_end_matches('/');
            path == prefix || path.starts_with(&format!("{prefix}/"))
        })
    }

    /// The session value to fold into the signature, or `None` when session
    /// binding isn't configured.
    fn session_bind<'a>(&self, session_value: Option<&'a str>) -> Option<&'a str> {
        if self.session_cookie.is_some() {
            session_value
        } else {
            None
        }
    }

    /// True when `token` may keep serving as this browser's CSRF cookie.
    /// Plain mode accepts any non-empty token. Signed mode (with a
    /// resolvable secret) requires a structurally valid `<raw>.<sig>` —
    /// anything else (typically a cookie minted before `signed_csrf`
    /// was enabled) triggers a rotation re-mint by the caller.
    fn token_acceptable(&self, token: &str, session_value: Option<&str>) -> bool {
        if token.is_empty() {
            return false;
        }
        if !self.signed {
            return true;
        }
        let Some(secret) = self.secret.as_deref() else {
            return true; // signing requested but no secret resolved: degrade
        };
        let Some((raw, sig)) = token.rsplit_once('.') else {
            return false;
        };
        tokens_match(sig, &sign(secret, raw, self.session_bind(session_value)))
    }
}

/// Generate a fresh 32-byte token, hex-encoded. Public so tests and downstream
/// code that mints tokens directly (e.g. server-rendered forms) share the same
/// shape. Raw (unsigned) — the signed wrapper is applied by the middleware.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("getrandom failed");
    hex::encode(bytes)
}

/// HMAC-SHA256 over `raw` (and the session value, when bound), keyed by the app
/// secret, hex-encoded.
///
/// `secret` must never be empty in production. Boot (`on_ready`) already
/// rejects an empty `SECRET_KEY` before this path is reachable in a real
/// deployment; the assert below catches the bug in debug/test builds if
/// that guard is somehow bypassed.
fn sign(secret: &str, raw: &str, session: Option<&str>) -> String {
    debug_assert!(
        !secret.is_empty(),
        "sign() called with an empty secret — CSRF tokens are trivially forgeable; \
         on_ready should have rejected boot already"
    );
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac =
        <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(raw.as_bytes());
    if let Some(s) = session {
        mac.update(b".");
        mac.update(s.as_bytes());
    }
    hex::encode(mac.finalize().into_bytes())
}

/// Mint a token for the response cookie — signed when configured and a secret
/// is available, raw otherwise.
fn mint_token(state: &CsrfState, session_value: Option<&str>) -> String {
    let raw = generate_token();
    if state.signed {
        if let Some(secret) = state.secret.as_deref() {
            let sig = sign(secret, &raw, state.session_bind(session_value));
            return format!("{raw}.{sig}");
        }
    }
    raw
}

/// Validate a submitted token against the cookie token. Always requires the
/// double-submit equality; additionally verifies the HMAC signature when
/// `signed` is on and a secret is available.
fn csrf_valid(
    state: &CsrfState,
    cookie_token: &str,
    submitted: &str,
    session_value: Option<&str>,
) -> bool {
    if !tokens_match(cookie_token, submitted) {
        return false;
    }
    if !state.signed {
        return true;
    }
    let Some(secret) = state.secret.as_deref() else {
        // Signing requested but no secret resolved (e.g. before App::build()):
        // fall back to plain double-submit rather than locking writes out.
        return true;
    };
    let Some((raw, sig)) = cookie_token.rsplit_once('.') else {
        // Signed mode requires a signature; an unsigned token can't be trusted.
        return false;
    };
    let expected = sign(secret, raw, state.session_bind(session_value));
    tokens_match(sig, &expected)
}

/// Pull the value of a named cookie out of a `Cookie` header. v0 shape: linear
/// scan, no quoting.
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

async fn csrf_middleware(
    State(state): State<CsrfState>,
    req: Request,
    next: Next,
) -> Result<Response, Infallible> {
    let method = req.method().clone();

    // Exempt paths (e.g. a token-authenticated `/api`) bypass CSRF entirely —
    // they carry no session cookie, so the double-submit check doesn't apply.
    if state.is_exempt(req.uri().path()) {
        return Ok(next.run(req).await);
    }

    let cookie_header = req
        .headers()
        .get(COOKIE)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    let cookie_token = cookie_header
        .as_deref()
        .and_then(|h| cookie_value(h, CSRF_COOKIE).map(str::to_string));
    let session_value = state.session_cookie.as_deref().and_then(|name| {
        cookie_header
            .as_deref()
            .and_then(|h| cookie_value(h, name).map(str::to_string))
    });

    if is_safe_method(&method) {
        // The middleware is the only mint (docs/decisions/
        // 2026-06-10-automatic-csrf.md): mint BEFORE the handler runs so
        // first-visit renders already have a token in scope, and rotate a
        // cookie token that can't pass signed-mode validation so flipping
        // `signed_csrf` on doesn't 403 browsers holding old cookies.
        let (token, minted) = match cookie_token {
            Some(t) if state.token_acceptable(&t, session_value.as_deref()) => (t, false),
            _ => (mint_token(&state, session_value.as_deref()), true),
        };
        let mut response =
            umbral::templates::with_current_csrf(Some(token.clone()), next.run(req)).await;
        if minted {
            let mut cookie = format!("{CSRF_COOKIE}={token}; Path=/; SameSite=Lax");
            if state.secure {
                cookie.push_str("; Secure");
            }
            if let Ok(v) = HeaderValue::from_str(&cookie) {
                // `append`, not `insert` — `insert` would wipe any cookie
                // the handler set on this response (e.g. the session).
                response.headers_mut().append(SET_COOKIE, v);
            }
        }
        return Ok(response);
    }

    // Write methods: cookie and (header OR form field) must validate.
    // On success the token is scoped around the handler so a
    // validation-error re-render still carries it into the form.
    let header_token = req
        .headers()
        .get(CSRF_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);

    if let Some(c) = cookie_token.as_ref() {
        if let Some(h) = header_token.as_ref() {
            if csrf_valid(&state, c, h, session_value.as_deref()) {
                let token = c.clone();
                return Ok(umbral::templates::with_current_csrf(Some(token), next.run(req)).await);
            }
        }
        // Form-field path: peek the urlencoded body, then rebuild the request.
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
            if let Some(s) = form_field_token(&bytes) {
                if csrf_valid(&state, &cookie_owned, &s, session_value.as_deref()) {
                    let req = Request::from_parts(parts, Body::from(bytes));
                    return Ok(umbral::templates::with_current_csrf(
                        Some(cookie_owned),
                        next.run(req),
                    )
                    .await);
                }
            }
        }
    }

    Ok(forbidden())
}

/// Strip the `Server` response header (used when `hide_server_header` is set
/// and no explicit value was given).
async fn strip_server_header(req: Request, next: Next) -> Result<Response, Infallible> {
    let mut response = next.run(req).await;
    response.headers_mut().remove(SERVER);
    Ok(response)
}

fn forbidden() -> Response {
    let body = Body::from("CSRF verification failed");
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .body(body)
        .expect("static response")
}

/// Scan a urlencoded form body for any of the accepted CSRF field names.
fn form_field_token(body: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(body).ok()?;
    for part in s.split('&') {
        let mut iter = part.splitn(2, '=');
        let key = iter.next()?;
        let val = iter.next().unwrap_or("");
        if CSRF_FORM_FIELDS.contains(&key) {
            // Tokens are hex (signed tokens add a `.` + hex sig — still no
            // urlencoded-special chars), so `+`→space is the only decode
            // needed for the common case.
            return Some(val.replace('+', " "));
        }
    }
    None
}

/// Read the current CSRF token from the request's cookie header. Public so
/// handlers that render HTML forms can embed it as a hidden `csrf_token` input.
pub fn current_csrf_token(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| cookie_value(h, CSRF_COOKIE).map(str::to_string))
}

/// Constant-time string equality. Short-circuit `==` on `String` is a timing
/// side-channel; `ct_eq` closes it. Per OWASP's "Use Constant-Time String
/// Comparison" rule for security tokens. Public so other token consumers
/// (e.g. the admin's SecurityPlugin-less login fallback) compare the same way.
pub fn tokens_match(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Validate that `secret_key` is non-empty when signed CSRF is enabled.
///
/// Called from [`SecurityPlugin::on_ready`]. Extracted as a free function so
/// integration tests can exercise it with an explicit [`umbral::Settings`]
/// without needing a live `App::build()` to populate the ambient
/// `SETTINGS` OnceLock (which is `pub(crate)` and unreachable from plugin
/// tests).
///
/// Behaviour when `settings` is `None` (i.e. `get_opt()` returned nothing,
/// common in tests that bypass `App::build()`): treated as non-prod, no
/// error.
fn check_secret_key(
    settings: Option<&umbral::Settings>,
    config: &SecurityConfig,
) -> Result<(), umbral::plugin::PluginError> {
    // Only relevant when signed CSRF is active; plain double-submit doesn't
    // use the secret at all.
    if !config.csrf || !config.signed_csrf {
        return Ok(());
    }

    let Some(s) = settings else {
        // No settings available — running outside App::build() (e.g. tests).
        // Can't determine environment or secret; skip.
        return Ok(());
    };

    if s.secret_key.trim().is_empty() {
        match s.environment {
            Environment::Dev | Environment::Test => {
                tracing::warn!(
                    "SecurityPlugin: SECRET_KEY is empty — CSRF tokens are signed with an \
                     empty HMAC key and are trivially forgeable. Set `secret_key` in \
                     umbral.toml or the UMBRAL_SECRET_KEY environment variable before \
                     deploying."
                );
            }
            Environment::Prod => {
                return Err(
                    "SecurityPlugin: SECRET_KEY must not be empty in production. \
                     An empty key makes CSRF tokens trivially forgeable. \
                     Set `secret_key` in umbral.toml or via UMBRAL_SECRET_KEY."
                        .into(),
                );
            }
        }
    }

    Ok(())
}

/// Test-only constructors. `#[doc(hidden)]` — NOT a stable API; integration
/// tests need a CSRF-wrapped router without `App::build()`-resolved settings.
#[doc(hidden)]
pub mod test_support {
    use super::*;

    /// Wrap `router` with the CSRF middleware using an explicit state,
    /// bypassing settings resolution.
    pub fn wrap_with_csrf(
        router: axum::Router,
        signed: bool,
        secret: Option<String>,
    ) -> axum::Router {
        let state = CsrfState {
            secure: false,
            signed,
            secret,
            session_cookie: None,
            exempt_paths: Vec::new(),
        };
        router.layer(middleware::from_fn_with_state(state, csrf_middleware))
    }

    /// Exercise [`check_secret_key`] directly with an explicit [`umbral::Settings`],
    /// bypassing the ambient `SETTINGS` OnceLock (which is `pub(crate)` and
    /// unreachable from plugin tests).
    pub fn validate_secret_key(
        settings: &umbral::Settings,
        config: &SecurityConfig,
    ) -> Result<(), umbral::plugin::PluginError> {
        check_secret_key(Some(settings), config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_state(secret: &str, session_cookie: Option<&str>) -> CsrfState {
        CsrfState {
            secure: false,
            signed: true,
            secret: Some(secret.to_string()),
            session_cookie: session_cookie.map(str::to_string),
            exempt_paths: Vec::new(),
        }
    }

    #[test]
    fn signing_is_deterministic_and_key_dependent() {
        assert_eq!(sign("k", "abc", None), sign("k", "abc", None));
        assert_ne!(sign("k1", "abc", None), sign("k2", "abc", None));
        assert_ne!(sign("k", "abc", None), sign("k", "abc", Some("sess")));
    }

    #[test]
    fn signed_token_round_trips_and_rejects_forgery() {
        let st = signed_state("app-secret", None);
        let token = mint_token(&st, None);
        // Minted token is `<raw>.<sig>` and validates as a double-submit pair.
        assert!(token.contains('.'));
        assert!(csrf_valid(&st, &token, &token, None));
        // An unsigned token (attacker-planted, no valid signature) is rejected
        // even though it double-submits against itself.
        let forged = generate_token();
        assert!(!csrf_valid(&st, &forged, &forged, None));
        // A token signed under a different key is rejected.
        let other = signed_state("different-secret", None);
        let other_token = mint_token(&other, None);
        assert!(!csrf_valid(&st, &other_token, &other_token, None));
    }

    #[test]
    fn session_binding_ties_token_to_session_value() {
        let st = signed_state("app-secret", Some("umbral_session"));
        let token = mint_token(&st, Some("sess-A"));
        assert!(csrf_valid(&st, &token, &token, Some("sess-A")));
        // Same token under a different session value no longer validates.
        assert!(!csrf_valid(&st, &token, &token, Some("sess-B")));
    }

    #[test]
    fn unsigned_mode_is_plain_double_submit() {
        let st = CsrfState {
            secure: false,
            signed: false,
            secret: None,
            session_cookie: None,
            exempt_paths: Vec::new(),
        };
        let tok = generate_token();
        assert!(csrf_valid(&st, &tok, &tok, None));
        assert!(!csrf_valid(&st, &tok, "different", None));
    }

    #[test]
    fn exempt_path_matching_is_prefix_based() {
        let st = CsrfState {
            secure: false,
            signed: false,
            secret: None,
            session_cookie: None,
            exempt_paths: vec!["/api".to_string()],
        };
        assert!(st.is_exempt("/api"));
        assert!(st.is_exempt("/api/customer/1"));
        assert!(!st.is_exempt("/admin"));
        assert!(!st.is_exempt("/contact"));
    }

    /// `/api` exempt must NOT bleed into `/api-internal`, `/apixyz`, etc.
    /// The boundary check requires the prefix to be followed by `/` (sub-path)
    /// or be an exact match — a bare `starts_with("/api")` would incorrectly
    /// exempt those sibling routes.
    #[test]
    fn csrf_exempt_boundary_stops_at_path_segment() {
        let st = CsrfState {
            secure: false,
            signed: false,
            secret: None,
            session_cookie: None,
            exempt_paths: vec!["/api".to_string()],
        };
        // Exact match and sub-paths ARE exempt.
        assert!(st.is_exempt("/api"), "/api exact must be exempt");
        assert!(
            st.is_exempt("/api/users"),
            "/api/users sub-path must be exempt"
        );
        assert!(
            st.is_exempt("/api/v2/resource"),
            "/api/v2/resource must be exempt"
        );
        // Paths that merely START WITH the string but aren't segment-separated
        // must NOT be exempt — that would be a CSRF-bypass on unintended routes.
        assert!(
            !st.is_exempt("/api-internal"),
            "/api-internal must NOT be exempt when /api is configured"
        );
        assert!(
            !st.is_exempt("/apixyz"),
            "/apixyz must NOT be exempt when /api is configured"
        );
        assert!(
            !st.is_exempt("/api2"),
            "/api2 must NOT be exempt when /api is configured"
        );
    }

    #[test]
    fn hsts_value_reflects_flags() {
        let cfg = SecurityConfig {
            hsts_max_age: 100,
            hsts_include_subdomains: true,
            hsts_preload: true,
            ..Default::default()
        };
        assert_eq!(cfg.hsts_value(), "max-age=100; includeSubDomains; preload");
        let bare = SecurityConfig {
            hsts_max_age: 100,
            hsts_include_subdomains: false,
            hsts_preload: false,
            ..Default::default()
        };
        assert_eq!(bare.hsts_value(), "max-age=100");
    }
}
