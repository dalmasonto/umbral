//! Trailing-slash redirect policy.
//!
//! axum's Router treats `/foo` and `/foo/` as distinct paths; a route
//! registered as one returns 404 for the other. Most real apps want
//! both forms to reach the same handler, and they want it consistent
//! across handlers (so users can't break sharing a URL just because
//! they happened to type or omit a trailing slash).
//!
//! The append-slash policy handles this by intercepting 404 responses,
//! checking whether the path with a trailing slash added would have
//! matched, and redirecting if so. umbral offers that shape, opt-in,
//! default off, under [`SlashRedirect`].
//!
//! ## Usage
//!
//! ```ignore
//! use umbral::prelude::*;
//! use umbral::web::SlashRedirect;
//!
//! let app = App::builder()
//!     .slash_redirect(SlashRedirect::Append)  // append a trailing slash and retry
//!     .routes(Routes::new().get("/articles", handler))
//!     .build()?;
//! ```
//!
//! With `Append`, a request to `/articles/` (trailing slash) that
//! axum returns 404 for gets re-checked: if `/articles` (no trailing
//! slash) would match, the response becomes a 308 redirect to
//! `/articles`. The browser follows; the second request hits the real
//! handler. The same shape works in reverse with `Strip`.
//!
//! ## Why 308, not 301
//!
//! 308 (Permanent Redirect) preserves the HTTP method and body, where
//! 301 historically converted POST → GET. The current consensus is to
//! use 308 / 307 for slash normalisation so a POST to `/api/users` (no
//! slash) doesn't silently become a GET when the canonical URL is
//! `/api/users/`. umbral picks 308 since it's a greenfield framework.
//!
//! ## Implementation: an outer layer over EVERY 404, not a fallback
//!
//! The first implementation was a `Router::fallback` handler, which only
//! runs on route-*misses*. That left a blind spot users hit constantly
//! (gaps4 #50): a 404 produced by a MATCHED route never reaches a
//! fallback, and wildcard routes make that the common case — `/api/docs/`
//! *matches* REST's `/api/{table}/` route (table = "docs"), REST answers
//! 404 "unknown resource" from inside the handler, and the redirect to
//! the real `/api/docs` never fired. The same mechanism produced the
//! playground `/api/playground` trap, and gaps3 #11 hand-mounted both
//! slash forms of every auth route to dodge it — symptoms of the
//! mechanism being at the wrong altitude.
//!
//! The probe is now a middleware layer wrapped around the WHOLE router
//! (including its fallback and nested services): run the request, and if
//! the response — from a matched handler, a wildcard capture, a
//! `nest_service`, or the fallback — is a 404, probe the alternate slash
//! form against a pre-layer snapshot of the router and redirect when it
//! would answer. A deliberate 404 whose alternate form ALSO 404s (a
//! missing row when both slash forms are registered, an out-of-scope row)
//! passes through with its original body untouched.
//!
//! ## Performance
//!
//! Responses that aren't 404 pay a string clone of the path. A 404 pays
//! one extra Router::call to probe the alternate path (through a Router
//! clone — cheap, Arc internally). The probe is a header-less GET: safe
//! to repeat, and a 405 ("route exists, wrong method") counts as proof
//! enough to redirect.

use std::future::Future;
use std::pin::Pin;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, Uri, header};
use axum::middleware::Next;
use tower::Service;

/// Policy for how the framework handles requests with a trailing slash
/// that don't match a registered route.
///
/// The default is [`Self::Off`] — no redirects, requests reach axum's
/// routing table as-is. Users opt into redirect behaviour via
/// [`crate::app::AppBuilder::slash_redirect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlashRedirect {
    /// Default. No redirects. `/foo` and `/foo/` are distinct.
    #[default]
    Off,
    /// Append-slash shape. On a 404 for `/foo`,
    /// the framework probes `/foo/`; if that would match, returns a
    /// 308 redirect. Most web frameworks default to this.
    Append,
    /// REST-API convention. On a 404 for `/foo/`, probes `/foo`;
    /// returns 308 if that would match. Useful for APIs that prefer
    /// the slashless canonical form.
    Strip,
}

impl SlashRedirect {
    /// The path to probe given the original request path. Returns
    /// `None` when this policy wouldn't kick in (e.g. `Off`, or
    /// `Append` on a path that already ends with `/`, or `Strip` on
    /// the root path).
    ///
    /// The contract: if this returns `Some(alt)`, the layer probes
    /// `alt` against the inner router after a 404 and redirects
    /// when the probe succeeds.
    pub fn alternate_path(&self, path: &str) -> Option<String> {
        // audit_2 core-web #6: never build a redirect target from a
        // protocol-relative path. `//evil.com` is parsed by browsers as
        // `scheme://evil.com`, so a `Location: //evil.com/` would be an open
        // redirect. Refuse it here (the single source of the alternate path).
        if path.starts_with("//") {
            return None;
        }
        match self {
            SlashRedirect::Off => None,
            SlashRedirect::Append => {
                if path == "/" || path.ends_with('/') {
                    None
                } else {
                    Some(format!("{path}/"))
                }
            }
            SlashRedirect::Strip => {
                if path == "/" || !path.ends_with('/') {
                    None
                } else {
                    Some(path.trim_end_matches('/').to_string())
                }
            }
        }
    }
}

/// Build the middleware closure (for `axum::middleware::from_fn`) that
/// implements the slash-redirect policy over EVERY 404 the app produces
/// (gaps4 #50) — matched handlers, wildcard captures, nested services,
/// and the fallback alike.
///
/// `snapshot` is a clone of the router taken **before** this layer is
/// applied, so probing it can't recursively re-enter the layer. `policy`
/// chooses the redirect direction. A 404 whose alternate form doesn't
/// answer passes through with its original body untouched (a REST JSON
/// 404 stays a REST JSON 404).
pub fn slash_redirect_probe(
    snapshot: Router,
    policy: SlashRedirect,
) -> impl Fn(Request<Body>, Next) -> Pin<Box<dyn Future<Output = Response<Body>> + Send>>
+ Clone
+ Send
+ Sync
+ 'static {
    move |req: Request<Body>, next: Next| {
        let snapshot = snapshot.clone();
        let policy = policy;
        Box::pin(async move {
            // Capture the path/query up front — `next.run` consumes the
            // request.
            let original_path = req.uri().path().to_owned();
            let query = req
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default();

            let response = next.run(req).await;
            // Only a 404 is a candidate — and only when the policy
            // produces an alternate form for this path.
            if response.status() != StatusCode::NOT_FOUND {
                return response;
            }
            let Some(alt) = policy.alternate_path(&original_path) else {
                return response;
            };

            // Probe the alternate path with a header-less GET. axum's
            // routes only match specific methods; if a route exists for
            // `alt` it usually serves GET (and the client's re-request
            // after the 308 uses the method it originally attempted).
            // For non-GET probes that come back 405 we still redirect —
            // 405 means "route exists, just not for that method", which
            // is enough to know the alternate path is real. The probe
            // carries no headers, so a protected alternate answers
            // 401/403 — also proof of existence.
            let alt_uri: Uri = match format!("{alt}{query}").parse() {
                Ok(u) => u,
                Err(_) => return response,
            };
            let probe_req = match Request::builder()
                .method(Method::GET)
                .uri(alt_uri)
                .body(Body::empty())
            {
                Ok(r) => r,
                Err(_) => return response,
            };
            // Drive poll_ready before call() per Service contract.
            // The fully-qualified syntax pins which `Service<...>`
            // impl on Router we're targeting — Router has multiple
            // impls (one for HTTP requests, one for IncomingStream
            // accept loops).
            let mut probe_service = snapshot.clone();
            std::future::poll_fn(|cx| {
                <Router as Service<Request<Body>>>::poll_ready(&mut probe_service, cx)
            })
            .await
            .ok();
            let probe_resp =
                match <Router as Service<Request<Body>>>::call(&mut probe_service, probe_req).await
                {
                    Ok(r) => r,
                    Err(_) => return response,
                };
            // 404 means "the alternate doesn't answer either" — keep the
            // ORIGINAL response (body and all). Anything else (200, 405,
            // 401, 3xx, …) means a route exists there.
            if probe_resp.status() == StatusCode::NOT_FOUND {
                return response;
            }
            // Issue a 308 redirect preserving method + body, with
            // the original query string carried across.
            //
            // CRLF injection is prevented by two layers: (a) axum's
            // `Uri::path()` returns the percent-encoded path, so a
            // malicious `%0d%0a` stays as the four-character escape
            // sequence and never becomes raw CR+LF in our `location`
            // string; (b) `HeaderValue` parsing rejects raw control
            // chars, so even if (a) somehow flipped, `value` would
            // fail to parse and the header wouldn't be inserted.
            // Both layers are implicit — if axum ever changes how
            // it decodes paths, this comment is the canary.
            let mut redirect = Response::new(Body::empty());
            *redirect.status_mut() = StatusCode::PERMANENT_REDIRECT;
            let location = format!("{alt}{query}");
            if let Ok(value) = location.parse() {
                redirect.headers_mut().insert(header::LOCATION, value);
            }
            redirect
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternate_path_off_never_returns_anything() {
        assert_eq!(SlashRedirect::Off.alternate_path("/foo"), None);
        assert_eq!(SlashRedirect::Off.alternate_path("/foo/"), None);
        assert_eq!(SlashRedirect::Off.alternate_path("/"), None);
    }

    #[test]
    fn alternate_path_append_adds_trailing_slash() {
        assert_eq!(
            SlashRedirect::Append.alternate_path("/foo"),
            Some("/foo/".to_string())
        );
        assert_eq!(
            SlashRedirect::Append.alternate_path("/api/articles"),
            Some("/api/articles/".to_string())
        );
    }

    #[test]
    fn alternate_path_append_skips_already_slashed() {
        assert_eq!(SlashRedirect::Append.alternate_path("/foo/"), None);
        assert_eq!(SlashRedirect::Append.alternate_path("/"), None);
    }

    /// audit_2 core-web #6 — a `//`-prefixed path is protocol-relative
    /// (`//evil.com` → `https://evil.com`). Building a redirect Location from it
    /// would be an open redirect, so the policy must refuse to produce an
    /// alternate path for one (defense-in-depth; the router normalizes today).
    #[test]
    fn alternate_path_refuses_protocol_relative_paths() {
        assert_eq!(SlashRedirect::Append.alternate_path("//evil.com"), None);
        assert_eq!(SlashRedirect::Append.alternate_path("//evil.com/x"), None);
        assert_eq!(SlashRedirect::Strip.alternate_path("//evil.com/"), None);
        // A legitimate single-slash path is unaffected.
        assert_eq!(
            SlashRedirect::Append.alternate_path("/foo"),
            Some("/foo/".to_string())
        );
    }

    #[test]
    fn alternate_path_strip_removes_trailing_slash() {
        assert_eq!(
            SlashRedirect::Strip.alternate_path("/foo/"),
            Some("/foo".to_string())
        );
        assert_eq!(
            SlashRedirect::Strip.alternate_path("/api/articles/"),
            Some("/api/articles".to_string())
        );
    }

    #[test]
    fn alternate_path_strip_skips_slashless_and_root() {
        assert_eq!(SlashRedirect::Strip.alternate_path("/foo"), None);
        assert_eq!(SlashRedirect::Strip.alternate_path("/"), None);
    }
}
