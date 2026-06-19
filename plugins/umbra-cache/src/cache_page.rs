//! View-level caching middleware — the Rust equivalent of Django's
//! `@cache_page(seconds)` decorator.
//!
//! Wrap a [`Router`] subtree with [`cache_page`] and every eligible
//! `GET` or `HEAD` response for that subtree is cached for the
//! configured TTL. Subsequent requests for the same URI + query string
//! get the cached response without hitting the handler.
//!
//! ```ignore
//! use umbra_cache::cache_page;
//! use std::time::Duration;
//!
//! let public = Router::new()
//!     .route("/", get(home))
//!     .route("/about", get(about))
//!     .layer(cache_page(Duration::from_secs(60)));
//! ```
//!
//! ## Cache key
//!
//! `cache:page:GET:<host>:/path?query` — method + Host header + full URI
//! including query string.  Fragments are stripped by the browser and never
//! reach the server.  Including the Host header prevents multi-tenant
//! cache-poisoning where tenant A's cached page would otherwise be served to
//! requests arriving on a different Host.
//!
//! ## What gets cached
//!
//! Only `GET` and `HEAD` responses with HTTP status **200** are stored.
//! The following bypass caching:
//! - Any method other than `GET` / `HEAD` (POST, PUT, PATCH, DELETE).
//! - Status code other than 200.
//! - Response carries `Cache-Control: no-store`.
//! - Response carries a `Set-Cookie` header (the body may be personalised).
//! - Request carries an `umbra_session` cookie — personalised / logged-in
//!   requests are neither served from nor written to the page cache, keeping
//!   the cache to the safe anonymous-only subset.
//!
//! ## Ambient cache dependency
//!
//! [`cache_page`] reads the ambient [`super::Cache`] via [`super::ambient()`].
//! If the ambient cache has not been initialised (i.e. [`super::CachePlugin::init`]
//! has not been called), cache misses and stores are silently skipped —
//! the handler always fires normally. This is intentional: a misconfigured
//! cache degrades gracefully rather than returning 500s.
//!
//! ## Deferred
//!
//! - ETag / 304 conditional caching — the current implementation always
//!   serves the full cached body. A future iteration will store and compare
//!   ETags to emit 304 Not Modified, saving bandwidth.
//! - Vary-header awareness (`Vary: Accept-Language`, etc.).
//! - Per-route cache key prefix customisation.

use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, header};
use bytes::Bytes;
use futures_util::future::BoxFuture;
use http_body_util::BodyExt;
use tower::{Layer, Service};

use crate::Cache;

// ── Public constructor ───────────────────────────────────────────────────────

/// Return a [`CachePageLayer`] that caches eligible `GET`/`HEAD` responses
/// for `ttl`.
///
/// Mount it with `Router::layer(cache_page(Duration::from_secs(60)))`.
pub fn cache_page(ttl: Duration) -> CachePageLayer {
    CachePageLayer { ttl, cache: None }
}

// ── Layer ────────────────────────────────────────────────────────────────────

/// [`tower::Layer`] returned by [`cache_page`]. Wraps the inner service
/// with [`CachePageService`].
#[derive(Clone)]
pub struct CachePageLayer {
    ttl: Duration,
    // An explicit cache can be injected for testing; production code
    // reads the ambient handle via `crate::ambient()`.
    cache: Option<Arc<Cache>>,
}

impl CachePageLayer {
    /// Override the cache handle used by this layer. Useful in tests
    /// where the ambient cache isn't initialised.
    pub fn with_cache(mut self, cache: Cache) -> Self {
        self.cache = Some(Arc::new(cache));
        self
    }
}

impl<S> Layer<S> for CachePageLayer {
    type Service = CachePageService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CachePageService {
            inner,
            ttl: self.ttl,
            cache: self.cache.clone(),
        }
    }
}

// ── Service ──────────────────────────────────────────────────────────────────

/// [`tower::Service`] produced by [`CachePageLayer`].
#[derive(Clone)]
pub struct CachePageService<S> {
    inner: S,
    ttl: Duration,
    cache: Option<Arc<Cache>>,
}

impl<S> Service<Request<Body>> for CachePageService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        let ttl = self.ttl;
        let explicit_cache = self.cache.clone();

        Box::pin(async move {
            // Only attempt to cache GET and HEAD
            let method = req.method().clone();
            if method != Method::GET && method != Method::HEAD {
                return inner.call(req).await;
            }

            // Bypass for personalised / authenticated requests: if the incoming
            // request carries an `umbra_session` cookie the response is user-
            // specific and must not be served from or stored in the page cache.
            // We match the literal cookie name "umbra_session" (the canonical
            // name from umbra-sessions::COOKIE_NAME) without importing that crate
            // to avoid a plugin-to-plugin dependency.
            if request_has_session_cookie(&req) {
                return inner.call(req).await;
            }

            // Build the cache key from method + Host header + full URI (path + query).
            // Including the Host prevents multi-tenant cache-poisoning where different
            // virtual hosts serving different content share cache entries.
            let host = req
                .headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let uri = req.uri().to_string();
            let cache_key = format!("cache:page:{}:{}:{}", method, host, uri);

            // Resolve the cache to use: explicit (test injection) > ambient
            let cache: Option<&Cache> = if let Some(ref c) = explicit_cache {
                Some(c.as_ref())
            } else {
                crate::ambient()
            };

            // Cache hit — return the stored response bytes
            if let Some(cache) = cache {
                if let Some(stored) = cache.get_bytes_raw(&cache_key).await {
                    if let Ok(resp) = deserialise_cached_response(stored) {
                        return Ok(resp);
                    }
                    // Deserialisation failure → treat as a miss and re-run the handler
                }
            }

            // Cache miss — call through to the handler
            let resp = inner.call(req).await?;

            // Only cache eligible responses
            let status = resp.status();
            if status != StatusCode::OK {
                return Ok(resp);
            }

            let should_skip = response_bypasses_cache(&resp);

            // Collect the body so we can both cache and return it.
            // This buffers the full response in memory which is fine
            // for HTML pages (< a few MB). Skip caching if collection
            // fails but still return the original error to the client.
            let (parts, body) = resp.into_parts();
            let body_bytes = match body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(e) => {
                    // BROKEN-7: the body stream failed partway. Reusing the
                    // success `parts` with an empty body fabricates a 200
                    // whose `Content-Length` no longer matches the (empty)
                    // body — that desyncs keep-alive connections and is
                    // indistinguishable from a real empty page. Log it and
                    // return a clean 502 instead; never cache it.
                    tracing::error!(
                        error = %e,
                        "cache_page: failed to collect upstream response body; returning 502"
                    );
                    let mut resp = Response::new(Body::from("Bad Gateway"));
                    *resp.status_mut() = StatusCode::BAD_GATEWAY;
                    return Ok(resp);
                }
            };

            if !should_skip {
                if let Some(cache) = explicit_cache.as_deref().or_else(|| crate::ambient()) {
                    let serialised = serialise_cached_response(&parts, &body_bytes);
                    cache.set_bytes_raw(&cache_key, serialised, Some(ttl)).await;
                }
            }

            let resp = Response::from_parts(parts, Body::from(body_bytes));
            Ok(resp)
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Return `true` when the request carries an `umbra_session` cookie.
///
/// Session-cookie-bearing requests are for authenticated / personalised pages.
/// Serving those from (or caching them into) the shared page cache would either
/// leak one user's content to another user or serve a stale anonymous page to a
/// logged-in user.  We bypass the cache entirely for these requests.
///
/// The cookie name `umbra_session` matches `umbra_sessions::COOKIE_NAME`.  We
/// match the literal string to avoid a crate dependency from umbra-cache on
/// umbra-sessions.
fn request_has_session_cookie<B>(req: &Request<B>) -> bool {
    // Cookie header value is a semicolon-separated list of "name=value" pairs.
    req.headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|cookie_str| {
            cookie_str
                .split(';')
                .any(|pair| pair.trim().starts_with("umbra_session="))
        })
        .unwrap_or(false)
}

/// Return `true` when the response should not be cached:
/// - `Cache-Control: no-store` is present
/// - `Set-Cookie` header is present
fn response_bypasses_cache<B>(resp: &Response<B>) -> bool {
    let headers = resp.headers();

    // Cache-Control: no-store
    if let Some(cc) = headers.get(header::CACHE_CONTROL) {
        if cc
            .to_str()
            .unwrap_or("")
            .split(',')
            .any(|d| d.trim().eq_ignore_ascii_case("no-store"))
        {
            return true;
        }
    }

    // Any Set-Cookie header means the response is personalised
    if headers.contains_key(header::SET_COOKIE) {
        return true;
    }

    false
}

// ── Wire format for cached responses ─────────────────────────────────────────
//
// Stored bytes layout (length-prefixed, little-endian u32):
//   [4 bytes: header_count N]
//   for each header:
//     [4 bytes: name_len][name bytes][4 bytes: value_len][value bytes]
//   [body bytes]
//
// This is a simple custom format; serde/JSON would add overhead for the
// binary body. Status code is always 200 (the only value we cache) so
// it's not stored.

fn serialise_cached_response(parts: &http::response::Parts, body: &Bytes) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();

    let header_count = parts.headers.len() as u32;
    out.extend_from_slice(&header_count.to_le_bytes());

    for (name, value) in &parts.headers {
        let name_bytes = name.as_str().as_bytes();
        let value_bytes = value.as_bytes();
        out.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&(value_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(value_bytes);
    }

    out.extend_from_slice(body);
    out
}

fn deserialise_cached_response(data: Vec<u8>) -> Result<Response<Body>, ()> {
    if data.len() < 4 {
        return Err(());
    }
    let mut pos = 0;

    let header_count = u32::from_le_bytes(data[pos..pos + 4].try_into().map_err(|_| ())?) as usize;
    pos += 4;

    let mut builder = Response::builder().status(StatusCode::OK);

    for _ in 0..header_count {
        if pos + 4 > data.len() {
            return Err(());
        }
        let name_len = u32::from_le_bytes(data[pos..pos + 4].try_into().map_err(|_| ())?) as usize;
        pos += 4;
        if pos + name_len > data.len() {
            return Err(());
        }
        let name = std::str::from_utf8(&data[pos..pos + name_len]).map_err(|_| ())?;
        pos += name_len;

        if pos + 4 > data.len() {
            return Err(());
        }
        let val_len = u32::from_le_bytes(data[pos..pos + 4].try_into().map_err(|_| ())?) as usize;
        pos += 4;
        if pos + val_len > data.len() {
            return Err(());
        }
        let value = &data[pos..pos + val_len];
        pos += val_len;

        builder = builder.header(name, value);
    }

    let body_bytes = Bytes::copy_from_slice(&data[pos..]);
    builder.body(Body::from(body_bytes)).map_err(|_| ())
}
