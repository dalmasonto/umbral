//! Host-header validation — the request-time half of `settings.allowed_hosts`.
//!
//! Django's `ALLOWED_HOSTS`: a request whose `Host` header isn't on the
//! allowlist is rejected with a 400 *before* any handler runs. This defends
//! against Host-header injection — cache poisoning and poisoned absolute-URL /
//! password-reset links that trust an attacker-supplied `Host`.
//!
//! Enforced only in [`Environment::Prod`](crate::settings::Environment::Prod);
//! development passes through so a `localhost` / LAN-IP / tunnel host doesn't
//! 400 mid-iteration. The allowlist comes from `settings.allowed_hosts`
//! (default `["localhost", "127.0.0.1"]`, set via `UMBRA_ALLOWED_HOSTS` or
//! `umbra.toml`). Patterns mirror Django:
//!
//! - `"example.com"` — exact match.
//! - `".example.com"` — the domain itself and any subdomain.
//! - `"*"` — allow any host (the explicit escape hatch / "disable").
//!
//! Wired automatically by `App::build()`; no plugin required. The boot-time
//! `check.rs` warning (still-default `allowed_hosts` in prod) is the companion
//! that tells an operator to set this before they hit the 400.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::HOST;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// Host allowlist captured at `App::build()` time, consulted per request.
#[derive(Clone)]
pub(crate) struct HostPolicy {
    /// Lowercased patterns from `settings.allowed_hosts`.
    allowed: Arc<[String]>,
    /// Only enforce in `Environment::Prod`; dev passes through.
    enforce: bool,
}

impl HostPolicy {
    pub(crate) fn new(allowed: &[String], enforce: bool) -> Self {
        Self {
            allowed: allowed.iter().map(|h| h.to_lowercase()).collect(),
            enforce,
        }
    }

    fn is_allowed(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        self.allowed.iter().any(|pat| match_host(pat, &host))
    }
}

/// Match a single `allowed_hosts` pattern against a (already-lowercased) host.
fn match_host(pattern: &str, host: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(domain) = pattern.strip_prefix('.') {
        // ".example.com" matches "example.com" and any subdomain of it.
        return host == domain || host.ends_with(pattern);
    }
    host == pattern
}

/// Strip the optional port (and IPv6 brackets) from a `Host` header value:
/// `example.com:8080` → `example.com`, `[::1]:8080` → `::1`.
fn hostname_of(raw: &str) -> &str {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    raw.split(':').next().unwrap_or(raw)
}

/// Middleware: 400 a request whose `Host` isn't in the allowlist (Prod only).
pub(crate) async fn host_guard(
    State(policy): State<HostPolicy>,
    req: Request,
    next: Next,
) -> Response {
    if !policy.enforce {
        return next.run(req).await;
    }
    let host = req
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| hostname_of(h).to_string());
    match host {
        Some(h) if policy.is_allowed(&h) => next.run(req).await,
        _ => (
            StatusCode::BAD_REQUEST,
            "Bad Request: the Host header is not in allowed_hosts.",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(hosts: &[&str]) -> HostPolicy {
        let owned: Vec<String> = hosts.iter().map(|s| s.to_string()).collect();
        HostPolicy::new(&owned, true)
    }

    #[test]
    fn exact_match() {
        let p = policy(&["example.com", "localhost"]);
        assert!(p.is_allowed("example.com"));
        assert!(p.is_allowed("localhost"));
        assert!(!p.is_allowed("evil.com"));
    }

    #[test]
    fn case_insensitive() {
        assert!(policy(&["Example.COM"]).is_allowed("example.com"));
    }

    #[test]
    fn subdomain_wildcard() {
        let p = policy(&[".example.com"]);
        assert!(p.is_allowed("example.com"), "bare domain matches");
        assert!(p.is_allowed("api.example.com"), "subdomain matches");
        assert!(p.is_allowed("a.b.example.com"), "nested subdomain matches");
        assert!(
            !p.is_allowed("notexample.com"),
            "suffix-but-not-subdomain rejected"
        );
        assert!(!p.is_allowed("example.com.evil.com"));
    }

    #[test]
    fn star_allows_any() {
        assert!(policy(&["*"]).is_allowed("anything.at.all"));
    }

    #[test]
    fn port_and_ipv6_are_stripped() {
        assert_eq!(hostname_of("example.com:8080"), "example.com");
        assert_eq!(hostname_of("[::1]:8080"), "::1");
        assert_eq!(hostname_of("127.0.0.1"), "127.0.0.1");
    }

    #[tokio::test]
    async fn middleware_400s_bad_host_and_allows_good_in_prod() {
        use axum::Router;
        use axum::routing::get;
        use tower::ServiceExt;

        let app = Router::new().route("/", get(|| async { "ok" })).layer(
            axum::middleware::from_fn_with_state(policy(&["example.com"]), host_guard),
        );

        // Allowed host (port stripped) → handler runs.
        let res = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .header("host", "example.com:8080")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Forged host → 400 before the handler.
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .header("host", "evil.com")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn dev_passes_through_any_host() {
        use axum::Router;
        use axum::routing::get;
        use tower::ServiceExt;

        // enforce = false (the dev posture): no host is rejected.
        let dev = HostPolicy::new(&["example.com".to_string()], false);
        let app = Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(dev, host_guard));

        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .header("host", "anything.com")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
