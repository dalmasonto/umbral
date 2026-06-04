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
//! use umbra::prelude::*;
//! use umbra::cors::CorsConfig;
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

use std::time::Duration;

use axum::http::{HeaderName, HeaderValue, Method, header};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

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

    #[test]
    #[should_panic(expected = "allow_any_origin() is incompatible with allow_credentials(true)")]
    fn any_origin_plus_credentials_panics() {
        CorsConfig::permissive()
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
