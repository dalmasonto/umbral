//! Route registry — a snapshot of every URL path the framework knows
//! about, grouped by plugin.
//!
//! The registry is populated once at `App::build()` time from two
//! sources:
//!
//! 1. The implicit `"app"` plugin's path list, fed from the
//!    [`Routes`] builder passed to [`crate::AppBuilder::routes`].
//!    Each `.get(...) / .post(...)` etc. call records both a handler
//!    *and* a [`RouteSpec`], so the registry is automatically in
//!    sync with the actual axum router for user-binary routes.
//! 2. Each registered plugin's `Plugin::route_paths()` contribution,
//!    walked in topological dependency order.
//!
//! The registry is opt-in for surfacing. Currently the only consumer
//! is the dev-mode default 404 template, which renders the path list
//! so a developer who hits a typoed URL can see what's available
//! without grepping the router tree. The registry is read by
//! `crate::errors::render_not_found` only when `settings.environment
//! == Dev`, so production 404 responses stay minimal.
//!
//! ## What this is *not*
//!
//! The registry is a *declared* list, not a live introspection of
//! axum's route table. axum doesn't expose its internal `RouteTable`,
//! so plugins that contribute routes through `Plugin::routes()`
//! report them via this companion `Plugin::route_paths()` method. The
//! two can drift — if a plugin author adds a `.route("/foo", ...)` to
//! its `routes()` method but forgets to add `"/foo"` to
//! `route_paths()`, the registry won't mention it. The cost of that
//! drift is "404 page is slightly stale," not "framework is broken."
//!
//! For user-binary routes, the [`Routes`] builder eliminates drift
//! at the source: a path can only land in the axum router by going
//! through `Routes::get/post/...`, which also records the spec. The
//! escape hatch `Routes::with_router` *can* merge an external
//! `axum::Router` whose paths the registry doesn't see — by design,
//! since that's where typed-State / middleware / nested routers
//! live and there's no axum API to introspect them.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use axum::Router;
use axum::handler::Handler;

/// One declared route entry: the URL path pattern plus the HTTP
/// methods it accepts. The dev-mode 404 template renders the methods
/// as colored badges next to each path so a developer can tell at a
/// glance which verb the endpoint expects.
///
/// `methods` is `Vec<&'static str>` because every realistic value is
/// a method name literal (`"GET"`, `"POST"`, etc.). When a plugin
/// declares a path without naming methods, `methods` stays empty and
/// the template falls back to an "ANY" badge.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RouteSpec {
    pub path: String,
    pub methods: Vec<&'static str>,
    /// The permission string this route is gated on, when it was registered
    /// through a permission-aware builder (`route_gated` / the umbral-permissions
    /// `require_permission` helper). `None` means the framework has no *recorded*
    /// permission for the route — it may still be gated by a hand-applied
    /// `.layer(permission_required(...))` (opaque to `RouteSpec`) or be
    /// intentionally public. Drives the boot audit of ungated mutating routes
    /// (audit_2 H19) and future OpenAPI security annotations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
}

impl RouteSpec {
    /// Construct a spec with the given path and method names. Use
    /// when you want explicit control; the `From` impls below cover
    /// the ergonomic shorthands.
    pub fn new<P: Into<String>>(path: P, methods: Vec<&'static str>) -> Self {
        Self {
            path: path.into(),
            methods,
            permission: None,
        }
    }
}

impl From<&str> for RouteSpec {
    /// `"/admin/"` → spec with no method declared.
    fn from(path: &str) -> Self {
        Self {
            path: path.to_string(),
            methods: Vec::new(),
            permission: None,
        }
    }
}

impl From<String> for RouteSpec {
    fn from(path: String) -> Self {
        Self {
            path,
            methods: Vec::new(),
            permission: None,
        }
    }
}

impl From<(&'static str, &str)> for RouteSpec {
    /// `("GET", "/articles")` → spec with one method.
    fn from((method, path): (&'static str, &str)) -> Self {
        Self {
            path: path.to_string(),
            methods: vec![method],
            permission: None,
        }
    }
}

impl From<(&'static str, String)> for RouteSpec {
    fn from((method, path): (&'static str, String)) -> Self {
        Self {
            path,
            methods: vec![method],
            permission: None,
        }
    }
}

impl From<(&[&'static str], &str)> for RouteSpec {
    /// `(&["GET", "POST"], "/api/post/")` → spec with two methods.
    fn from((methods, path): (&[&'static str], &str)) -> Self {
        Self {
            path: path.to_string(),
            methods: methods.to_vec(),
            permission: None,
        }
    }
}

/// Snapshot of declared routes, keyed by plugin name. The implicit
/// `"app"` plugin holds the user's hand-registered paths; built-in
/// and third-party plugins hold their own contributions.
///
/// Iteration order is alphabetical by plugin name (BTreeMap), which
/// gives the 404 template a stable, human-friendly listing without
/// the framework picking an arbitrary plugin to show first.
#[derive(Debug, Clone, Default)]
pub struct RouteRegistry {
    pub by_plugin: BTreeMap<String, Vec<RouteSpec>>,
}

impl RouteRegistry {
    /// Total number of declared paths across every plugin. Used by
    /// the 404 template's pluralisation and by tests asserting that
    /// at least *something* got registered.
    pub fn total(&self) -> usize {
        self.by_plugin.values().map(|v| v.len()).sum()
    }
}

/// Builder for the user binary's hand-registered routes.
///
/// Replaces the `(.router(...) + .route_paths([...]))` double-entry
/// pattern with a single builder that records both as you go:
///
/// ```ignore
/// use umbral::prelude::*;
///
/// App::builder()
///     .routes(
///         Routes::new()
///             .get("/", home)
///             .get("/articles", list_articles_html)
///             .get("/articles/{id}", article_detail)
///             .post("/api/articles", create_article),
///     )
///     .build()?;
/// ```
///
/// Behind the scenes each `.get(...)` / `.post(...)` / etc. call
/// records a [`RouteSpec`] *and* registers the handler with an
/// internal `axum::Router`. `AppBuilder::routes` extracts both —
/// the router becomes the user-binary router, the specs flow into
/// the [`RouteRegistry`] for the dev-mode 404 page.
///
/// ## Why this exists
///
/// axum's `Router` doesn't expose its internal route table, so the
/// framework can't introspect what was registered. The old API
/// asked users to declare paths twice — once via `.route(...)` for
/// the actual handler, once via `.route_paths([...])` for the dev
/// 404 surface. `Routes` tracks both in one call.
///
/// ## Escape hatches
///
/// - **Need axum middleware / nest / fallback / State?** Build a
///   plain `axum::Router` and pass it to [`Routes::with_router`].
///   That router merges into the tracked one; you'll need to
///   declare its paths via `route_paths(...)` if you want them in
///   the dev 404 page.
/// - **Multi-method route on one path?** Use [`Routes::route`]
///   with an explicit method list.
#[must_use = "Routes must be passed to AppBuilder::routes to take effect"]
pub struct Routes {
    inner: Router,
    specs: Vec<RouteSpec>,
}

impl Routes {
    /// Empty builder.
    pub fn new() -> Self {
        Self {
            inner: Router::new(),
            specs: Vec::new(),
        }
    }

    /// Register a `GET` handler. Same handler shape as
    /// `axum::routing::get(...)`.
    pub fn get<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.with_method("GET", path, axum::routing::get(handler))
    }

    /// Register a `POST` handler.
    pub fn post<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.with_method("POST", path, axum::routing::post(handler))
    }

    /// Register a `PUT` handler.
    pub fn put<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.with_method("PUT", path, axum::routing::put(handler))
    }

    /// Register a `PATCH` handler.
    pub fn patch<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.with_method("PATCH", path, axum::routing::patch(handler))
    }

    /// Register a `DELETE` handler.
    pub fn delete<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.with_method("DELETE", path, axum::routing::delete(handler))
    }

    /// Register a `HEAD` handler.
    pub fn head<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.with_method("HEAD", path, axum::routing::head(handler))
    }

    /// Register a `OPTIONS` handler.
    pub fn options<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.with_method("OPTIONS", path, axum::routing::options(handler))
    }

    /// Register a path with a single method using a pre-built
    /// `MethodRouter` — the right shape for per-route middleware.
    ///
    /// The per-method shorthands above accept a bare handler, which
    /// the framework wraps in `axum::routing::<method>(...)` for you.
    /// When you need to layer middleware (`login_required_html`,
    /// rate-limiting, per-route timeouts, etc.) you need the
    /// `MethodRouter` form so you can chain `.layer(...)`:
    ///
    /// ```ignore
    /// use axum::routing::get;
    ///
    /// Routes::new()
    ///     .get("/", home)                                 // bare handler
    ///     .layered("GET", "/dashboard", get(dashboard)    // layered
    ///         .layer(login_required_html("/login")))
    /// ```
    ///
    /// The layer attaches to *this route only* — exactly what
    /// `axum::routing::MethodRouter::layer` already does. The plain
    /// `axum::Router::layer` would have applied to every route on
    /// the Router instance, which is the gotcha the old scaffold
    /// fell into.
    ///
    /// Sugar for [`Self::route`] with a single-element method slice.
    pub fn layered(
        self,
        method: &'static str,
        path: &str,
        handler: axum::routing::MethodRouter<()>,
    ) -> Self {
        self.route(&[method], path, handler)
    }

    /// Register one or more methods on a path. Use this when several
    /// HTTP verbs share a handler-router (`axum::routing::get(h1).post(h2)`)
    /// — the per-method shorthands above each declare exactly one
    /// method, so a chained `MethodRouter` needs this explicit form
    /// to land its full method list in the registry.
    ///
    /// ```ignore
    /// use axum::routing::{get, post};
    ///
    /// Routes::new().route(
    ///     &["GET", "POST"],
    ///     "/api/comments",
    ///     get(list_comments).post(create_comment),
    /// )
    /// ```
    pub fn route(
        mut self,
        methods: &[&'static str],
        path: &str,
        handler: axum::routing::MethodRouter<()>,
    ) -> Self {
        self.specs.push(RouteSpec {
            path: path.to_string(),
            methods: methods.to_vec(),
            permission: None,
        });
        self.inner = self.inner.route(path, handler);
        self
    }

    /// Register a route AND record the permission it's gated on (audit_2 H19).
    ///
    /// Identical to [`Self::route`] except the recorded [`RouteSpec`] carries
    /// `permission`, so the boot audit of ungated mutating routes can see the
    /// route as gated. This method does NOT apply an enforcement layer — the
    /// caller passes a `handler` that already carries it. The ergonomic pairing
    /// (apply the `permission_required` layer AND record the string in one call)
    /// is the umbral-permissions `Routes::require_permission(...)` helper; core
    /// stays free of any dependency on the permissions plugin.
    pub fn route_gated(
        mut self,
        methods: &[&'static str],
        path: &str,
        handler: axum::routing::MethodRouter<()>,
        permission: impl Into<String>,
    ) -> Self {
        self.specs.push(RouteSpec {
            path: path.to_string(),
            methods: methods.to_vec(),
            permission: Some(permission.into()),
        });
        self.inner = self.inner.route(path, handler);
        self
    }

    /// Merge a pre-built `axum::Router` into the tracked routes.
    ///
    /// Use when you need axum features the per-method shorthands
    /// don't expose: `nest`, `fallback`, middleware layers, typed
    /// State, etc. The merged router contributes its handlers but
    /// *not* its paths — paths inside the external router aren't
    /// visible to the framework, so they won't appear in the dev
    /// 404 page unless you declare them via
    /// [`AppBuilder::route_paths`](crate::AppBuilder::route_paths).
    pub fn with_router(mut self, router: Router) -> Self {
        self.inner = self.inner.merge(router);
        self
    }

    /// Consume into the inner axum Router plus the tracked specs.
    /// `AppBuilder::routes` is the canonical consumer.
    pub fn into_parts(self) -> (Router, Vec<RouteSpec>) {
        (self.inner, self.specs)
    }

    /// Shared body for the per-method shorthands.
    fn with_method(
        mut self,
        method: &'static str,
        path: &str,
        handler: axum::routing::MethodRouter<()>,
    ) -> Self {
        self.specs.push(RouteSpec {
            path: path.to_string(),
            methods: vec![method],
            permission: None,
        });
        self.inner = self.inner.route(path, handler);
        self
    }
}

impl Default for Routes {
    fn default() -> Self {
        Self::new()
    }
}

static REGISTRY: OnceLock<RouteRegistry> = OnceLock::new();

/// Publish the registry. Called from `App::build()` after every
/// plugin's `route_paths()` has been collected. Safe to call exactly
/// once; subsequent calls are no-ops.
pub fn init(registry: RouteRegistry) {
    let _ = REGISTRY.set(registry);
}

/// Read the registry. Returns `None` if `init` hasn't been called
/// (production binaries that bypass `App::build()`, tests that
/// short-circuit the build flow). Callers should treat `None` as
/// "no routes to surface" rather than as an error.
pub fn get() -> Option<&'static RouteRegistry> {
    REGISTRY.get()
}

// =========================================================================
// OpenAPI path registry (BUG-20).
//
// `Plugin::openapi_paths()` lets a plugin contribute fully-formed
// OpenAPI Path Item Objects keyed by URL. App::build collects every
// plugin's contribution into a flat Vec and publishes it here; the
// umbral-openapi crate reads from this at spec-build time.
//
// The shape mirrors `RouteRegistry`: a OnceLock with the same `init`
// / `get` pattern, lifecycle bound to `App::build()`. Returning
// `None` is the "build wasn't called" case; consumers treat that
// the same as "no plugin contributed routes."
// =========================================================================

static OPENAPI_REGISTRY: OnceLock<Vec<(String, serde_json::Value)>> = OnceLock::new();

/// Publish the OpenAPI registry. Called from `App::build()` after
/// every plugin's `openapi_paths()` has been collected.
pub fn init_openapi(entries: Vec<(String, serde_json::Value)>) {
    let _ = OPENAPI_REGISTRY.set(entries);
}

/// Read the OpenAPI registry. `None` for pre-build callers.
pub fn registered_openapi_paths() -> Option<&'static [(String, serde_json::Value)]> {
    OPENAPI_REGISTRY.get().map(|v| v.as_slice())
}

// The URL the OpenAPI JSON spec is served at. Populated by
// `umbral-openapi`'s `Plugin::routes()` so cross-plugin consumers
// (notably `umbral-playground`, which has to fetch the spec from
// the SPA) can discover the configured mount without taking a
// cross-plugin dependency on `umbral-openapi`. The default of
// `/openapi/openapi.json` becomes wrong the moment the user calls
// `OpenApiPlugin::default().at("/api/docs")`; this registry is
// how the playground's SPA learns about that remap.
static OPENAPI_SPEC_URL: OnceLock<String> = OnceLock::new();

/// Publish the OpenAPI spec URL. Called from
/// `OpenApiPlugin::routes()` with the configured mount point.
pub fn init_openapi_spec_url(url: String) {
    let _ = OPENAPI_SPEC_URL.set(url);
}

/// Read the OpenAPI spec URL. `None` when OpenApiPlugin isn't
/// installed (the OnceLock was never populated). Consumers
/// typically fall back to `/openapi/openapi.json` for backwards
/// compat when this returns `None`.
pub fn registered_openapi_spec_url() -> Option<&'static str> {
    OPENAPI_SPEC_URL.get().map(|s| s.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn dummy_get() -> &'static str {
        "ok"
    }
    async fn dummy_post() -> &'static str {
        "ok"
    }

    #[test]
    fn routes_builder_records_one_spec_per_get_with_method_and_path() {
        let (_router, specs) = Routes::new()
            .get("/", dummy_get)
            .get("/articles", dummy_get)
            .post("/api/articles", dummy_post)
            .into_parts();

        assert_eq!(specs.len(), 3, "one spec per builder call: {specs:?}");
        assert_eq!(specs[0].path, "/");
        assert_eq!(specs[0].methods, vec!["GET"]);
        assert_eq!(specs[1].path, "/articles");
        assert_eq!(specs[1].methods, vec!["GET"]);
        assert_eq!(specs[2].path, "/api/articles");
        assert_eq!(specs[2].methods, vec!["POST"]);
    }

    #[test]
    fn routes_builder_supports_multi_method_via_route() {
        use axum::routing::get;
        let (_router, specs) = Routes::new()
            .route(
                &["GET", "POST"],
                "/api/comments",
                get(dummy_get).post(dummy_post),
            )
            .into_parts();

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].path, "/api/comments");
        assert_eq!(specs[0].methods, vec!["GET", "POST"]);
    }

    #[test]
    fn routes_with_router_merges_axum_router_silently() {
        use axum::Router;
        use axum::routing::get;
        let external = Router::new().route("/external", get(dummy_get));
        let (_router, specs) = Routes::new()
            .get("/tracked", dummy_get)
            .with_router(external)
            .into_parts();

        // Only the tracked route is in specs; the merged axum router
        // contributes its handler without surfacing its path in the
        // registry. That's the documented contract.
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].path, "/tracked");
    }

    #[test]
    fn total_sums_per_plugin_paths_and_handles_empty_groups() {
        let mut reg = RouteRegistry::default();
        reg.by_plugin
            .insert("app".to_string(), vec!["/".into(), "/articles".into()]);
        reg.by_plugin.insert(
            "admin".to_string(),
            vec![
                "/admin/".into(),
                "/admin/login".into(),
                "/admin/logout".into(),
            ],
        );
        reg.by_plugin.insert("sessions".to_string(), Vec::new());

        assert_eq!(reg.total(), 5);
    }
}
