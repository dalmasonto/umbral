//! CORS configuration for [`AppBuilder::cors`].
//!
//! Wraps [`tower_http::cors::CorsLayer`] behind a small,
//! security-defaulted builder so users don't have to learn
//! tower-http's `AllowOrigin` / `AllowMethods` / `AllowHeaders`
//! sum-types to enable a sensible cross-origin policy.
//!
//! ## When to use which constructor
//!
//! - [`CorsConfig::strict`] — empty allowlist; you opt in to every
//!   origin / method / header. The safe default for production
//!   APIs that talk to a known frontend.
//! - [`CorsConfig::permissive`] — mirrors `*` for everything except
//!   credentials. Convenient for dev / public APIs. The CORS spec
//!   forbids combining `*` origin with `allow_credentials=true`, so
//!   this constructor turns credentials off.
//!
//! ## Example
//!
//! ```ignore
//! use umbral::prelude::*;
//! use umbral::cors::CorsConfig;
//! use std::time::Duration;
//!
//! App::builder()
//!     .cors(
//!         CorsConfig::strict()
//!             .allow_origin("https://app.example.com")
//!             .allow_origin("https://admin.example.com")
//!             .allow_credentials(true)
//!             .max_age(Duration::from_secs(3600)),
//!     )
//!     .build()
//!     .await?;
//! ```

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::response::Response;
use tower::{Layer, Service};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, Cors, CorsLayer};

/// CORS policy for an `AppBuilder`. Defaults to *strict*: nothing
/// is allowed until you opt in. Use [`Self::permissive`] for dev.
#[derive(Debug, Clone)]
pub struct CorsConfig {
    origins: OriginPolicy,
    methods: MethodPolicy,
    headers: HeaderPolicy,
    expose_headers: Vec<HeaderName>,
    allow_credentials: bool,
    max_age: Option<Duration>,
}

#[derive(Debug, Clone)]
enum OriginPolicy {
    /// `Access-Control-Allow-Origin: *`. Incompatible with
    /// `allow_credentials = true`; the spec rejects that combination.
    Any,
    /// Echo the request's `Origin` back if it's in this list.
    List(Vec<String>),
}

#[derive(Debug, Clone)]
enum MethodPolicy {
    Any,
    List(Vec<Method>),
}

#[derive(Debug, Clone)]
enum HeaderPolicy {
    Any,
    List(Vec<HeaderName>),
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self::strict()
    }
}

impl CorsConfig {
    /// Empty allowlist — every cross-origin request is denied until
    /// you call `.allow_origin(...)` etc. The recommended starting
    /// point for production APIs.
    pub fn strict() -> Self {
        Self {
            origins: OriginPolicy::List(Vec::new()),
            methods: MethodPolicy::List(vec![
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::HEAD,
                Method::OPTIONS,
            ]),
            headers: HeaderPolicy::List(vec![header::CONTENT_TYPE, header::AUTHORIZATION]),
            expose_headers: Vec::new(),
            allow_credentials: false,
            max_age: Some(Duration::from_secs(3600)),
        }
    }

    /// Mirror everything (origin, methods, headers) — convenient for
    /// dev and fully-public APIs. Credentials are turned off because
    /// the CORS spec forbids combining `*` origin with credentials.
    pub fn permissive() -> Self {
        Self {
            origins: OriginPolicy::Any,
            methods: MethodPolicy::Any,
            headers: HeaderPolicy::Any,
            expose_headers: Vec::new(),
            allow_credentials: false,
            max_age: Some(Duration::from_secs(3600)),
        }
    }

    /// Append an explicit origin to the allowlist. Repeat for each
    /// origin; switches off `Any` if a previous call set it.
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        let entry = origin.into();
        self.origins = match self.origins {
            OriginPolicy::List(mut v) => {
                v.push(entry);
                OriginPolicy::List(v)
            }
            OriginPolicy::Any => OriginPolicy::List(vec![entry]),
        };
        self
    }

    /// Append several origins at once — the batch form of
    /// [`allow_origin`](Self::allow_origin). Accepts anything iterable
    /// of string-likes, so `vec!["https://a", "https://b"]`, an array,
    /// or a `Vec<String>` all work. Switches off `Any` if it was set.
    pub fn allow_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut list = match self.origins {
            OriginPolicy::List(v) => v,
            OriginPolicy::Any => Vec::new(),
        };
        list.extend(origins.into_iter().map(Into::into));
        self.origins = OriginPolicy::List(list);
        self
    }

    /// Allow every origin (`*`). Mutually exclusive with
    /// `allow_credentials(true)`; the builder panics at
    /// [`into_layer`](Self::into_layer) time if both are set.
    pub fn allow_any_origin(mut self) -> Self {
        self.origins = OriginPolicy::Any;
        self
    }

    /// Replace the allowed methods list. Default: the seven common
    /// HTTP verbs (GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS).
    pub fn allow_methods(mut self, methods: Vec<Method>) -> Self {
        self.methods = MethodPolicy::List(methods);
        self
    }

    /// Allow any request method.
    pub fn allow_any_method(mut self) -> Self {
        self.methods = MethodPolicy::Any;
        self
    }

    /// Replace the allowed request headers list. Default:
    /// `Content-Type` and `Authorization` — enough for JSON APIs
    /// plus Bearer / Basic auth.
    pub fn allow_headers(mut self, headers: Vec<HeaderName>) -> Self {
        self.headers = HeaderPolicy::List(headers);
        self
    }

    /// Allow any request header. Falls back to mirror-request when
    /// `allow_credentials(true)` is set, because the spec forbids
    /// `*` with credentials.
    pub fn allow_any_header(mut self) -> Self {
        self.headers = HeaderPolicy::Any;
        self
    }

    /// Headers JavaScript on the calling origin is allowed to read
    /// off the response (beyond the CORS-safelisted ones). Default
    /// empty; add e.g. `X-Total-Count` if you publish that header
    /// for pagination.
    pub fn expose_headers(mut self, headers: Vec<HeaderName>) -> Self {
        self.expose_headers = headers;
        self
    }

    /// Whether browsers should send cookies and `Authorization` on
    /// cross-origin requests. Off by default; switching it on
    /// forces the allowlist to use explicit origins (the CORS spec
    /// forbids `*` with credentials) and switches `Any` headers
    /// over to mirror-request.
    pub fn allow_credentials(mut self, yes: bool) -> Self {
        self.allow_credentials = yes;
        self
    }

    /// Preflight (`OPTIONS`) cache TTL the browser is asked to
    /// honour. Default 1 hour; `None` omits the `Access-Control-
    /// Max-Age` header so browsers fall back to their own default.
    pub fn max_age(mut self, duration: Duration) -> Self {
        self.max_age = Some(duration);
        self
    }

    /// Disable the `Access-Control-Max-Age` header.
    pub fn no_max_age(mut self) -> Self {
        self.max_age = None;
        self
    }

    /// Materialise into a tower-http [`CorsLayer`]. Panics if the
    /// configuration is internally inconsistent (origin = `Any` +
    /// credentials = true).
    pub(crate) fn into_layer(self) -> CorsLayer {
        let mut layer = CorsLayer::new();

        let origin: AllowOrigin = match self.origins {
            OriginPolicy::Any => {
                if self.allow_credentials {
                    panic!(
                        "CorsConfig: allow_any_origin() is incompatible with \
                         allow_credentials(true). The CORS spec forbids combining \
                         `Access-Control-Allow-Origin: *` with credentials. \
                         Use explicit allow_origin(...) calls instead."
                    );
                }
                AllowOrigin::any()
            }
            OriginPolicy::List(list) => {
                let parsed: Vec<HeaderValue> = list
                    .iter()
                    .map(|s| {
                        HeaderValue::from_str(s).unwrap_or_else(|e| {
                            panic!(
                                "CorsConfig: allow_origin({s:?}) is not a valid \
                                 HTTP header value: {e}"
                            )
                        })
                    })
                    .collect();
                AllowOrigin::list(parsed)
            }
        };
        layer = layer.allow_origin(origin);

        let methods: AllowMethods = match self.methods {
            MethodPolicy::Any => AllowMethods::any(),
            MethodPolicy::List(list) => AllowMethods::list(list),
        };
        layer = layer.allow_methods(methods);

        let headers: AllowHeaders = match self.headers {
            HeaderPolicy::Any if self.allow_credentials => AllowHeaders::mirror_request(),
            HeaderPolicy::Any => AllowHeaders::any(),
            HeaderPolicy::List(list) => AllowHeaders::list(list),
        };
        layer = layer.allow_headers(headers);

        if !self.expose_headers.is_empty() {
            layer = layer.expose_headers(self.expose_headers);
        }

        if self.allow_credentials {
            layer = layer.allow_credentials(true);
        }

        if let Some(age) = self.max_age {
            layer = layer.max_age(age);
        }

        layer
    }
}

/// A [`tower::Layer`] that applies an inner [`CorsLayer`] only to requests whose
/// path starts with `prefix` (e.g. `"/api"`); every other request passes through
/// untouched. The path-scoped counterpart to a global [`CorsConfig::into_layer`]
/// — "CORS on the REST API, not the HTML pages." Built by
/// [`AppBuilder::cors_for`](crate::app::AppBuilder::cors_for).
#[derive(Clone)]
pub(crate) struct ScopedCorsLayer {
    prefix: Arc<str>,
    cors: CorsLayer,
}

impl ScopedCorsLayer {
    pub(crate) fn new(prefix: impl Into<String>, cors: CorsLayer) -> Self {
        Self {
            prefix: Arc::from(prefix.into()),
            cors,
        }
    }
}

impl<S: Clone> Layer<S> for ScopedCorsLayer {
    type Service = ScopedCors<S>;

    fn layer(&self, inner: S) -> ScopedCors<S> {
        ScopedCors {
            prefix: self.prefix.clone(),
            with_cors: self.cors.layer(inner.clone()),
            without: inner,
        }
    }
}

/// Service produced by [`ScopedCorsLayer`]. Holds both the CORS-wrapped and the
/// bare inner service and dispatches per request path.
#[derive(Clone)]
pub(crate) struct ScopedCors<S> {
    prefix: Arc<str>,
    with_cors: Cors<S>,
    without: S,
}

impl<S> Service<Request> for ScopedCors<S>
where
    S: Service<Request, Response = Response, Error = Infallible> + Clone,
    S::Future: Send + 'static,
    Cors<S>: Service<Request, Response = Response, Error = Infallible>,
    <Cors<S> as Service<Request>>::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        // Either branch may serve the next request, so both must be ready.
        let ready =
            self.with_cors.poll_ready(cx).is_ready() && self.without.poll_ready(cx).is_ready();
        if ready {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn call(&mut self, req: Request) -> Self::Future {
        if req.uri().path().starts_with(&*self.prefix) {
            Box::pin(self.with_cors.call(req))
        } else {
            Box::pin(self.without.call(req))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_starts_empty_and_chains() {
        let cfg = CorsConfig::strict()
            .allow_origin("https://app.example.com")
            .allow_origin("https://admin.example.com")
            .allow_credentials(true);
        // Materialises without panic — would-be inconsistency
        // (Any + credentials) is the only panic case.
        let _ = cfg.into_layer();
    }

    #[test]
    fn permissive_materialises() {
        let _ = CorsConfig::permissive().into_layer();
    }

    #[tokio::test]
    async fn scoped_cors_only_affects_matching_prefix() {
        use axum::Router;
        use axum::routing::get;
        use tower::ServiceExt;

        let app = Router::new()
            .route("/api/ping", get(|| async { "api" }))
            .route("/page", get(|| async { "html" }))
            .layer(ScopedCorsLayer::new(
                "/api",
                CorsConfig::strict()
                    .allow_origin("https://app.example.com")
                    .into_layer(),
            ));

        // A cross-origin request under `/api` gets the CORS allow-origin header.
        let res = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/ping")
                    .header("origin", "https://app.example.com")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.headers()
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("https://app.example.com"),
            "/api should carry CORS headers"
        );

        // The same cross-origin request to a non-`/api` path gets none.
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/page")
                    .header("origin", "https://app.example.com")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            res.headers().get("access-control-allow-origin").is_none(),
            "non-/api routes must not be CORS-scoped"
        );
    }

    #[test]
    fn allow_origins_batches_and_appends() {
        let cfg = CorsConfig::strict()
            .allow_origin("https://a.example.com")
            .allow_origins(vec!["https://b.example.com", "https://c.example.com"]);
        match cfg.origins {
            OriginPolicy::List(v) => assert_eq!(v.len(), 3, "single + batch of two = three"),
            OriginPolicy::Any => panic!("should be a list"),
        }
        // Batch onto a fresh strict config materialises without panic.
        let _ = CorsConfig::strict()
            .allow_origins(["https://x.example.com".to_string()])
            .into_layer();
    }

    #[test]
    #[should_panic(expected = "allow_any_origin() is incompatible with allow_credentials(true)")]
    fn any_origin_plus_credentials_panics() {
        let _ = CorsConfig::permissive()
            .allow_credentials(true)
            .into_layer();
    }

    #[test]
    fn allow_origin_after_any_resets_to_list() {
        let cfg = CorsConfig::permissive()
            .allow_origin("https://app.example.com")
            .allow_credentials(true);
        // Should not panic — allow_origin() flipped Any → List.
        let _ = cfg.into_layer();
    }

    #[test]
    fn any_headers_with_credentials_mirrors_request() {
        // The spec forbids `*` headers with credentials, so the
        // builder substitutes mirror_request automatically. No
        // panic here.
        let cfg = CorsConfig::strict()
            .allow_origin("https://app.example.com")
            .allow_any_header()
            .allow_credentials(true);
        let _ = cfg.into_layer();
    }

    #[test]
    fn methods_default_to_common_seven() {
        let cfg = CorsConfig::strict();
        match cfg.methods {
            MethodPolicy::List(ref list) => {
                assert!(list.contains(&Method::GET));
                assert!(list.contains(&Method::POST));
                assert!(list.contains(&Method::OPTIONS));
            }
            _ => panic!("expected default methods to be a list, got Any"),
        }
    }
}
