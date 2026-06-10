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
use axum::http::header::{CONTENT_TYPE, HOST};
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
        other => disallowed_host_page(other.as_deref()),
    }
}

/// HTML-escape a string for safe interpolation into element text / attributes.
/// The `Host` header is attacker-controlled, so this is mandatory before it
/// reaches the response body.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the 400 page HTML for a rejected (or missing) host. `host` is the
/// already-port-stripped requested host, or `None` when no `Host` header was
/// sent. Split out from the `Response` builder so it's unit-testable without an
/// async runtime.
fn render_disallowed_host(host: Option<&str>) -> String {
    let escaped = host.map(html_escape);
    let host_block = match &escaped {
        Some(h) => format!(
            "<p class=\"row\">Requested host <span class=\"host\">{h}</span> is not allowed.</p>"
        ),
        None => "<p class=\"row\">The request did not include a <code>Host</code> header.</p>"
            .to_string(),
    };
    let example = escaped.as_deref().unwrap_or("example.com");
    PAGE.replace("__HOST_BLOCK__", &host_block)
        .replace("__EXAMPLE__", example)
}

fn disallowed_host_page(host: Option<&str>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        render_disallowed_host(host),
    )
        .into_response()
}

/// The 400 page template. Placeholders `__HOST_BLOCK__` and `__EXAMPLE__` are
/// substituted (both already HTML-escaped) — kept as `replace` targets rather
/// than `format!` args so the CSS braces don't need doubling.
const PAGE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>400 — Disallowed Host</title>
<style>
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; padding: 2rem;
    font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
    background: #0b0c0e; color: #e7e9ec; }
  .card { width: 100%; max-width: 42rem; background: #16181d; border: 1px solid #272a31;
    border-radius: 16px; padding: 2.5rem; box-shadow: 0 10px 40px rgba(0,0,0,.4); }
  h1 { margin: 0 0 .35rem; font-size: 1.5rem; }
  .sub { color: #9aa0a8; margin: 0 0 1.5rem; line-height: 1.5; }
  .row { margin: 0 0 1.25rem; }
  code, pre { font-family: ui-monospace, "SF Mono", Menlo, monospace; }
  .host { background: #23262d; border: 1px solid #343842; border-radius: 8px;
    padding: .15rem .5rem; color: #7dd3fc; }
  pre { background: #0e0f12; border: 1px solid #272a31; border-radius: 10px;
    padding: 1rem; overflow-x: auto; color: #cbd5e1; margin: .5rem 0 0; }
  .hint { color: #9aa0a8; font-size: .875rem; margin-top: 1.5rem; line-height: 1.5; }
  .hint code { color: #cbd5e1; }
</style>
</head>
<body>
  <main class="card">
    <h1>400 — Disallowed Host</h1>
    <p class="sub">This server refused the request because its <code>Host</code> header isn't in the configured <code>allowed_hosts</code> list.</p>
    __HOST_BLOCK__
    <p class="row">To allow it, add the host to <code>UMBRA_ALLOWED_HOSTS</code> (comma-separated) or to <code>allowed_hosts</code> in <code>umbra.toml</code>, then restart:</p>
    <pre>UMBRA_ALLOWED_HOSTS=__EXAMPLE__</pre>
    <p class="hint">Use a leading dot (<code>.example.com</code>) to match a domain and all its subdomains, or <code>*</code> to allow any host. Host validation runs only when <code>environment = "Prod"</code>.</p>
  </main>
</body>
</html>"#;

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

        // Forged host → 400 with the styled HTML page naming the host.
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
        assert_eq!(
            res.headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("Disallowed Host"), "full styled page");
        assert!(body.contains("evil.com"), "names the rejected host");
        assert!(
            body.contains("UMBRA_ALLOWED_HOSTS=evil.com"),
            "shows the fix"
        );
    }

    #[test]
    fn rejected_host_is_html_escaped() {
        // The Host header is attacker-controlled — it must not break out of the
        // page (reflected XSS). A scripty host is escaped, not echoed raw.
        let html = render_disallowed_host(Some(r#"evil.com"><script>alert(1)</script>"#));
        assert!(
            !html.contains("<script>alert(1)</script>"),
            "scripty host must be escaped, not echoed raw"
        );
        assert!(html.contains("&lt;script&gt;"), "escaped form present");
        assert_eq!(html_escape("a\"<>&'b"), "a&quot;&lt;&gt;&amp;&#x27;b");
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
