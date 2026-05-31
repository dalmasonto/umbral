//! Trailing-slash redirect policy — Django's `APPEND_SLASH` port.
//!
//! axum's Router treats `/foo` and `/foo/` as distinct paths; a route
//! registered as one returns 404 for the other. Most real apps want
//! both forms to reach the same handler, and they want it consistent
//! across handlers (so users can't break sharing a URL just because
//! they happened to type or omit a trailing slash).
//!
//! Django's `APPEND_SLASH = True` (the framework default) handles this
//! by intercepting 404 responses, checking whether the path with a
//! trailing slash added would have matched, and 301-redirecting if so.
//! umbra ports that shape — opt-in, default off — under
//! [`SlashRedirect`].
//!
//! ## Usage
//!
//! ```ignore
//! use umbra::prelude::*;
//! use umbra::web::SlashRedirect;
//!
//! let app = App::builder()
//!     .slash_redirect(SlashRedirect::Append)  // Django's APPEND_SLASH=True shape
//!     .router(Router::new().route("/articles", get(handler)))
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
//! `/api/users/`. Django uses 301 for backwards compatibility; umbra
//! picks 308 since it's a greenfield framework.
//!
//! ## Implementation: fallback handler, not tower layer
//!
//! `Router::layer(...)` in axum wraps individual route handlers; it
//! does **not** run on requests that don't match any route. The
//! redirect probe has to fire on 404s for paths that *don't* match —
//! so we install a fallback handler instead. The handler captures a
//! pre-fallback clone of the Router and probes it for the alternate
//! form when a request hits the fallback.
//!
//! ## Performance
//!
//! Routes that match on the first try pay zero overhead. The fallback
//! only fires when nothing matched; in that case it does one extra
//! Router::call to probe the alternate path. The probe goes through a
//! Router clone (cheap, Arc internally).

use std::future::Future;
use std::pin::Pin;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, Uri, header};
use tower::Service;

/// Policy for how the framework handles requests with a trailing slash
/// that don't match a registered route.
///
/// The default is [`Self::Off`] — no redirects, requests reach axum's
/// routing table as-is. Users opt into Django-style behaviour via
/// [`crate::app::AppBuilder::slash_redirect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlashRedirect {
    /// Default. No redirects. `/foo` and `/foo/` are distinct.
    #[default]
    Off,
    /// Django's `APPEND_SLASH = True` shape. On a 404 for `/foo`,
    /// the framework probes `/foo/`; if that would match, returns a
    /// 308 redirect. Most app frameworks default to this.
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

/// Build a closure suitable for `Router::fallback_service` that
/// implements the slash-redirect policy.
///
/// `snapshot` is a clone of the router taken **before** the fallback
/// is installed, so probing it can't recursively re-trigger this
/// fallback. `policy` chooses the redirect direction.
///
/// Returns a function-like service that takes
/// `axum::http::Request<Body>` and returns `axum::http::Response<Body>`.
pub fn slash_redirect_fallback(
    snapshot: Router,
    policy: SlashRedirect,
    not_found_template: Option<String>,
) -> impl Fn(Request<Body>) -> Pin<Box<dyn Future<Output = Response<Body>> + Send>>
+ Clone
+ Send
+ Sync
+ 'static {
    move |req: Request<Body>| {
        let snapshot = snapshot.clone();
        let policy = policy;
        let template = not_found_template.clone();
        Box::pin(async move {
            let original_path = req.uri().path().to_owned();
            let query = req
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default();

            // 404 path. Uses the configured not-found template when
            // present; otherwise plain text.
            let default_404 =
                || crate::errors::render_not_found(template.as_deref(), &original_path);

            // Don't fire the redirect unless the policy says so.
            let Some(alt) = policy.alternate_path(&original_path) else {
                return default_404();
            };
            // Probe the alternate path with a GET request. axum's
            // routes only match specific methods; if a route exists
            // for `/alt` it usually serves GET (and a re-request from
            // the browser after the 308 will use the same method the
            // user attempted). For non-GET probes that come back 405
            // we still redirect — 405 means "route exists, just not
            // for that method" which is enough to know the alternate
            // path is real.
            let alt_uri: Uri = match format!("{alt}{query}").parse() {
                Ok(u) => u,
                Err(_) => return default_404(),
            };
            let probe_req = match Request::builder()
                .method(Method::GET)
                .uri(alt_uri.clone())
                .body(Body::empty())
            {
                Ok(r) => r,
                Err(_) => return default_404(),
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
                    Err(_) => return default_404(),
                };
            // Removed debug eprintln; uncomment for diagnostics.
            // 404 means "no route for this path at all." Anything
            // else (200, 405, 3xx, etc.) means a route exists.
            if probe_resp.status() == StatusCode::NOT_FOUND {
                return default_404();
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
