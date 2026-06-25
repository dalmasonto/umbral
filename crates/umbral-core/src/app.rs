use axum::Router;
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::db::{self, DbPool};
use crate::migrate::ModelMeta;
use crate::orm::Model;
use crate::plugin::Plugin;
use crate::settings::Settings;

/// A per-request resolver that builds the request-scoped
/// [`crate::db::RouteContext`] from the incoming request. Installed via
/// [`AppBuilder::route_context`] and driven by [`route_context_scope_layer`].
type RouteContextResolver =
    std::sync::Arc<dyn Fn(&crate::web::Request) -> crate::db::RouteContext + Send + Sync>;

/// A built and ready-to-serve umbral application.
///
/// Created via `App::builder().build()`. Owns the merged router that
/// carries every registered plugin's routes plus the user-binary
/// routes passed to `AppBuilder::routes()`.
pub struct App {
    router: Router,
    plugins: Vec<Box<dyn Plugin>>,
}

impl App {
    /// Create a new [`AppBuilder`].
    pub fn builder() -> AppBuilder {
        // Load `.env` into the *process* environment so plain
        // `std::env::var(...)` code sees it — most importantly a plugin's
        // `from_env()` credential loader (e.g. the OAuth providers reading
        // `UMBRAL_OAUTH_*`). This runs before the `.plugin(...)` arguments
        // are evaluated, so those loaders find the values.
        //
        // We read `.env` the *same* CWD-relative way figment's settings
        // loader does (`from_filename_iter(".env")`) rather than
        // `dotenvy::dotenv()`, whose parent-directory search resolves the
        // file differently and missed it in practice. Each key is set only
        // when it isn't already present, so real environment vars keep
        // precedence. No-op when there's no `.env`.
        if let Ok(iter) = dotenvy::from_filename_iter(".env") {
            for (key, value) in iter.flatten() {
                if std::env::var_os(&key).is_none() {
                    // SAFETY: runs at startup (App::builder), before the
                    // server spawns request handlers that read the
                    // environment — the same operation `dotenvy::dotenv()`
                    // performs internally.
                    unsafe { std::env::set_var(&key, &value) };
                }
            }
        }
        AppBuilder::default()
    }

    /// Bind the axum listener and serve requests.
    ///
    /// This call blocks until the server stops. At M0 there is no graceful
    /// shutdown hook; that lands with the signal-handling work in a later
    /// milestone.
    pub async fn serve(self, addr: impl Into<SocketAddr>) -> Result<(), std::io::Error> {
        let listener = tokio::net::TcpListener::bind(addr.into()).await?;

        tracing::info!("umbral serving on {}", listener.local_addr()?);

        // Serve via `into_make_service()` rather than passing the router
        // directly. `axum::serve(listener, router)` drives the `Router` as
        // its own connection-maker, whose per-connection `call` runs
        // `self.clone().with_state(())` — and `with_state` finalizes EVERY
        // route eagerly, an O(route-count) cost paid once per new TCP
        // connection. With keep-alive that's amortized over all requests on
        // the connection; WITHOUT keep-alive (one connection per request) it
        // is paid on every request, capping throughput at ~1/with_state-cost
        // regardless of the handler. For an app with hundreds of routes (a
        // full admin + REST surface) that throttled no-keep-alive throughput
        // by ~4x or worse. `IntoMakeService` instead hands each connection a
        // cheap `Router::clone()` (an `Arc` bump) and lets routing finalize
        // lazily per request — measurably faster on fresh connections and no
        // slower with keep-alive. No `ConnectInfo` regression: the direct
        // path didn't provide it either (that needs
        // `into_make_service_with_connect_info`).
        axum::serve(listener, self.router.into_make_service()).await
    }

    /// Consume the [`App`] and return its merged axum router.
    ///
    /// Useful when the caller wants to drive the router themselves: an
    /// integration test that sends synthetic requests via
    /// `tower::ServiceExt::oneshot`, an embedding scenario that nests
    /// umbral under another axum tree, or any other path that doesn't
    /// want `serve()`'s opinionated listener.
    pub fn into_router(self) -> Router {
        self.router
    }

    /// Borrow the registered plugins in topological dependency order.
    ///
    /// Used by [`crate::cli::dispatch`] to walk every plugin's
    /// `commands()` contribution at CLI dispatch time. Borrowed (not
    /// moved) so the App stays usable after a dispatch call returns.
    pub fn plugins(&self) -> &[Box<dyn Plugin>] {
        &self.plugins
    }
}

/// The fluent entry point for constructing an [`App`].
///
/// Collects settings, database pools, and routes, then locks everything
/// into place at [`build`](AppBuilder::build).
pub struct AppBuilder {
    settings: Option<Settings>,
    databases: HashMap<String, DbPool>,
    router: Option<Router>,
    /// Companion path list for `router` — surfaces the user's hand-
    /// registered routes in the dev-mode 404 page. The builder can't
    /// peek inside an axum `Router`, so the caller declares its paths
    /// here. Empty by default; production deployments don't need to
    /// fill it.
    route_paths: Vec<crate::routes::RouteSpec>,
    models: Vec<ModelMeta>,
    plugins: Vec<Box<dyn Plugin>>,
    templates_dir: Option<std::path::PathBuf>,
    slash_redirect: crate::slash::SlashRedirect,
    not_found_template: Option<String>,
    server_error_template: Option<String>,
    /// Custom template per status code for general error pages (429, 403, …),
    /// styled like the 404/500 pages. See [`Self::error_template`].
    error_templates: HashMap<axum::http::StatusCode, String>,
    /// Optional hook called before the 500 template is rendered.
    server_error_hook: Option<crate::errors::ServerErrorHook>,
    /// When `true` (the default), the embedded default 404/500 templates
    /// are used as fallbacks when the user hasn't supplied their own.
    default_error_pages: bool,
    /// Path-scoped cross-origin policies (prefix → config), applied via
    /// [`AppBuilder::cors_for`]. Each is layered only onto requests whose
    /// path starts with the prefix (e.g. `"/api"`).
    cors_scoped: Vec<(String, crate::cors::CorsConfig)>,
    /// Optional cross-origin policy. `None` means no `CorsLayer`
    /// is installed at all and browsers apply the same-origin
    /// default. Configure via [`AppBuilder::cors`].
    cors: Option<crate::cors::CorsConfig>,
    /// When `Some(true)`, every ORM write terminal that supports
    /// `.atomic()` / `.non_atomic()` runs inside a transaction by
    /// default. Per-call `.non_atomic()` overrides. `None` keeps the
    /// pre-flag behaviour (no auto-wrapping). See
    /// [`AppBuilder::atomic_transactions`].
    atomic_transactions: Option<bool>,
    /// When `true`, a `tower-http` gzip/brotli compression layer wraps the
    /// router. Off by default — a reverse proxy usually owns compression,
    /// and double-compressing behind one is wasteful. Enable via
    /// [`AppBuilder::compression`].
    compress: bool,
    /// App-level framework middleware (feature #68), prepended to the
    /// plugins' contributions in the final stack. Added via
    /// [`AppBuilder::middleware`].
    middleware: Vec<std::sync::Arc<dyn crate::middleware::Middleware>>,
    /// Optional custom [`crate::db::DatabaseRouter`]. `None` uses
    /// `DefaultRouter` (today's static per-model routing). Installed
    /// during `build()` via [`crate::db::router::install_router`].
    db_router: Option<std::sync::Arc<dyn crate::db::DatabaseRouter>>,
    /// Optional per-request resolver that builds the request-scoped
    /// [`crate::db::RouteContext`]. When set, `build()` installs a layer that
    /// runs the resolver on each request and scopes the ENTIRE downstream
    /// future (handler plus every `.await`, including ORM calls) inside
    /// [`crate::db::route_context::scope`], so the ambient
    /// `umbral::db::route_context()` accessor — and thus the `DatabaseRouter`
    /// — sees the context this resolver set. Added via
    /// [`AppBuilder::route_context`].
    route_context_resolver: Option<RouteContextResolver>,
}

impl Default for AppBuilder {
    fn default() -> Self {
        Self {
            settings: None,
            databases: HashMap::new(),
            router: None,
            route_paths: Vec::new(),
            models: Vec::new(),
            plugins: Vec::new(),
            templates_dir: None,
            slash_redirect: crate::slash::SlashRedirect::default(),
            not_found_template: None,
            server_error_template: None,
            error_templates: HashMap::new(),
            server_error_hook: None,
            default_error_pages: true,
            cors: None,
            cors_scoped: Vec::new(),
            atomic_transactions: None,
            compress: false,
            middleware: Vec::new(),
            db_router: None,
            route_context_resolver: None,
        }
    }
}

impl AppBuilder {
    /// Set the application settings.
    pub fn settings(mut self, settings: Settings) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Register a database pool under the given alias.
    ///
    /// The `"default"` pool is the one returned by `umbral::db::pool()`
    /// and is required: `build()` fails with `BuildError::
    /// DefaultPoolMissing` if it isn't registered. The caller opens
    /// the pool via `umbral::db::connect(&url).await` and passes it
    /// here.
    ///
    /// Accepts anything that converts into a [`DbPool`]: a typed
    /// [`sqlx::SqlitePool`], a typed [`sqlx::PgPool`], or an already-
    /// built `DbPool`. The [`From`] impls on `DbPool` make plain
    /// SqlitePool callers (every test, every plugin example) work
    /// unchanged.
    pub fn database(mut self, alias: &str, pool: impl Into<DbPool>) -> Self {
        self.databases.insert(alias.to_owned(), pool.into());
        self
    }

    /// Install a custom [`crate::db::DatabaseRouter`]. Omit to use
    /// `DefaultRouter` (today's static per-model routing).
    pub fn router<R: crate::db::DatabaseRouter + 'static>(mut self, router: R) -> Self {
        self.db_router = Some(std::sync::Arc::new(router));
        self
    }

    /// Install a per-request [`crate::db::RouteContext`] resolver.
    ///
    /// The resolver runs once per request, builds a `RouteContext` (typically
    /// reading a tenant header or subdomain), and `build()` wraps the entire
    /// downstream future in [`crate::db::route_context::scope`]. Because the
    /// scope spans the whole handler — including every `.await` and every ORM
    /// call — the ambient `umbral::db::route_context()` accessor inside the
    /// handler, and the active [`crate::db::DatabaseRouter`], see exactly the
    /// context this resolver returned. A request the resolver maps to a
    /// default `RouteContext` runs with no tenant (no silent inheritance from
    /// a prior request).
    ///
    /// ```ignore
    /// use umbral::prelude::*;
    /// use umbral::db::{RouteContext, TenantKey};
    ///
    /// App::builder()
    ///     .route_context(|req| match req.headers().get("x-tenant") {
    ///         Some(v) => RouteContext::new()
    ///             .with_tenant(TenantKey::new(v.to_str().unwrap_or_default())),
    ///         None => RouteContext::new(),
    ///     })
    ///     .build()?;
    /// ```
    pub fn route_context<F>(mut self, resolver: F) -> Self
    where
        F: Fn(&crate::web::Request) -> crate::db::RouteContext + Send + Sync + 'static,
    {
        self.route_context_resolver = Some(std::sync::Arc::new(resolver));
        self
    }

    /// Register a model with the app's migration engine.
    ///
    /// Called once per model the user wants the M5 `makemigrations` /
    /// `migrate` commands to track. Captures the model's `NAME` /
    /// `TABLE` / `FIELDS` constants into an owned `ModelMeta` so the
    /// migration code can iterate without naming concrete `T` at the
    /// call site. M7's Plugin contract will replace this with
    /// `Plugin::models()` discovered through the plugin registry.
    pub fn model<T: Model>(mut self) -> Self {
        self.models.push(ModelMeta::for_::<T>());
        self
    }

    /// Register a plugin (M7).
    ///
    /// Plugins contribute models, routes, system_checks, and an
    /// `on_ready` hook. `App::build()` topologically sorts the
    /// registered set by `Plugin::dependencies()` and walks every
    /// plugin's contributions. The plugin name `"app"` is reserved
    /// for the implicit plugin that owns models registered via
    /// `.model::<T>()`; a plugin claiming that name causes
    /// `BuildError::ReservedPluginName`.
    pub fn plugin<P: Plugin>(mut self, plugin: P) -> Self {
        self.plugins.push(Box::new(plugin));
        self
    }

    /// Attach a [`Routes`](crate::routes::Routes) bundle of
    /// hand-registered routes.
    ///
    /// Each `.get(...) / .post(...) / .put(...) / .patch(...) /
    /// .delete(...) / .head(...) / .options(...)` call on `Routes`
    /// records the path *and* registers the handler, so the framework
    /// surfaces declared routes in the dev-mode 404 page without a
    /// parallel declaration list.
    ///
    /// Multi-method routes go through [`Routes::route`] (explicit
    /// method list + `axum::routing::MethodRouter`). Routes that need
    /// axum features the per-method shorthands don't expose (typed
    /// `State`, middleware layers, `nest`, fallback handlers, etc.)
    /// go through [`Routes::with_router`] — that escape hatch merges
    /// an external `axum::Router` and its paths stay opaque to the
    /// framework (won't appear in the dev 404 page).
    ///
    /// Calling this more than once merges the router and concatenates
    /// the specs.
    ///
    /// ```ignore
    /// use umbral::prelude::*;
    ///
    /// App::builder()
    ///     .routes(
    ///         Routes::new()
    ///             .get("/", home)
    ///             .get("/articles", list_articles_html)
    ///             .post("/api/articles", create_article),
    ///     )
    ///     .build()?;
    /// ```
    pub fn routes(mut self, routes: crate::routes::Routes) -> Self {
        let (router, specs) = routes.into_parts();
        self.router = Some(match self.router.take() {
            Some(prior) => prior.merge(router),
            None => router,
        });
        self.route_paths.extend(specs);
        self
    }

    /// Set the project-level templates directory.
    ///
    /// Defaults to `./templates` (relative to the binary's cwd) when
    /// the builder method isn't called. If the resolved path doesn't
    /// exist, the engine still publishes — calls to
    /// `umbral::templates::render` then return `TemplateError::Missing`
    /// with a clear diagnostic, which matches the "absence isn't an
    /// error unless something tries to render" rule from the spec.
    ///
    /// This directory is searched first (highest priority). Plugin
    /// directories contributed via `Plugin::templates_dirs()` are
    /// appended in topological order and searched afterwards. To
    /// override a plugin's template, drop a same-named file here.
    pub fn templates_dir<P: Into<std::path::PathBuf>>(mut self, path: P) -> Self {
        self.templates_dir = Some(path.into());
        self
    }

    /// Set the trailing-slash redirect policy. Django's `APPEND_SLASH`
    /// port — see [`crate::slash::SlashRedirect`].
    ///
    /// Default is `Off` (axum's strict matching). Most apps want
    /// `Append` (Django default: `/foo` 404 → 308 → `/foo/`) so that
    /// the same URL works with or without the trailing slash.
    ///
    /// ```ignore
    /// use umbral::prelude::*;
    /// use umbral::web::SlashRedirect;
    ///
    /// App::builder()
    ///     .slash_redirect(SlashRedirect::Append)
    ///     .build()?;
    /// ```
    pub fn slash_redirect(mut self, policy: crate::slash::SlashRedirect) -> Self {
        self.slash_redirect = policy;
        self
    }

    /// Set the template rendered on a 404. Mirrors Django's
    /// `404.html` convention.
    ///
    /// The template gets `{ path }` in scope — the request path that
    /// missed — so you can render `The page {{ path }} doesn't
    /// exist.` without wiring extractors. When unset, 404s return
    /// plain-text "Not Found". When set but the template fails to
    /// render (missing file, parse error), the framework falls back
    /// to the plain-text response and logs the render error.
    ///
    /// Composes with [`Self::slash_redirect`] — if a slash-redirect
    /// probe finds the alternate, it 308s before the not-found
    /// template fires.
    pub fn not_found_template(mut self, name: impl Into<String>) -> Self {
        self.not_found_template = Some(name.into());
        self
    }

    /// Set the template rendered on a panicking handler. Mirrors
    /// Django's `500.html` convention.
    ///
    /// Installs a `tower-http` `CatchPanic` layer around the router.
    /// A panic in any handler is caught, logged via `tracing::error`,
    /// and replaced with a 500 response carrying the rendered
    /// template. When unset, panics use tower-http's default
    /// behaviour (log + empty 500 body).
    ///
    /// In dev mode (`settings.environment == Dev`), the template receives
    /// `dev_mode`, `error_display`, `error_chain`, and `request_path`
    /// context variables. In prod those variables are empty.
    ///
    /// See [`Self::on_server_error`] for a hook that fires before the
    /// template renders.
    pub fn server_error_template(mut self, name: impl Into<String>) -> Self {
        self.server_error_template = Some(name.into());
        self
    }

    /// Register a custom template for error responses with `status` (e.g.
    /// `429`, `403`, `410`). When a handler returns `Err((status, message))`
    /// (or any non-HTML error response with this status), the template is
    /// rendered in its place — styled like the 404/500 pages — preserving the
    /// status code. The template receives `{ status, status_text, message,
    /// request_path, dev_mode }`. Repeatable for multiple codes.
    ///
    /// 404 and 500 have dedicated methods ([`Self::not_found_template`] /
    /// [`Self::server_error_template`]); use this for everything else.
    ///
    /// ```ignore
    /// App::builder()
    ///     .error_template(StatusCode::TOO_MANY_REQUESTS, "errors/429.html")
    ///     .error_template(StatusCode::FORBIDDEN, "errors/403.html")
    /// ```
    pub fn error_template(
        mut self,
        status: axum::http::StatusCode,
        name: impl Into<String>,
    ) -> Self {
        self.error_templates.insert(status, name.into());
        self
    }

    /// Register a hook that fires on every internal server error (500).
    ///
    /// The closure receives:
    /// - `error_display: &str` — the `Display` form of the error or the
    ///   stringified panic payload.
    /// - `request_path: &str` — the URI path of the failing request (empty
    ///   for panic-path errors where path isn't yet available).
    ///
    /// The hook runs synchronously before the 500 template is rendered. It
    /// cannot change the response — use it to log to an external service
    /// (Sentry, Datadog, a file, etc.).
    ///
    /// ```ignore
    /// App::builder()
    ///     .on_server_error(|err, path| {
    ///         tracing::error!(err, path, "500 error");
    ///     })
    ///     .build()?
    /// ```
    pub fn on_server_error<F>(mut self, hook: F) -> Self
    where
        F: Fn(&str, &str) + Send + Sync + 'static,
    {
        self.server_error_hook = Some(std::sync::Arc::new(hook));
        self
    }

    /// Disable the built-in default 404/500 templates.
    ///
    /// By default, when the user hasn't called `.not_found_template(...)` or
    /// `.server_error_template(...)`, umbral renders its own embedded Tailwind
    /// error pages. Call this method to revert to axum's built-in behaviour:
    /// a plain-text "Not Found" on 404 and an empty 500 body on panic.
    ///
    /// ```ignore
    /// App::builder()
    ///     .disable_default_error_pages()
    ///     .build()?
    /// ```
    pub fn disable_default_error_pages(mut self) -> Self {
        self.default_error_pages = false;
        self
    }

    /// Install a CORS policy as the outermost middleware.
    ///
    /// The framework doesn't install a `CorsLayer` by default —
    /// same-origin requests need no policy, and CORS is too
    /// security-sensitive to enable implicitly. Pass a
    /// [`crate::cors::CorsConfig`] (start from
    /// [`CorsConfig::strict`](crate::cors::CorsConfig::strict) for
    /// production or [`CorsConfig::permissive`](crate::cors::CorsConfig::permissive)
    /// for dev).
    ///
    /// ```ignore
    /// use umbral::prelude::*;
    /// use umbral::cors::CorsConfig;
    ///
    /// App::builder()
    ///     .cors(CorsConfig::strict()
    ///         .allow_origin("https://app.example.com")
    ///         .allow_credentials(true))
    ///     .build()
    ///     .await?
    /// ```
    ///
    /// The layer is applied LAST in the middleware chain so it
    /// becomes the outermost wrapper — preflight `OPTIONS` is
    /// answered before any plugin / handler sees the request, and
    /// the response headers are added on the way back out
    /// regardless of which downstream layer produced the body.
    pub fn cors(mut self, config: crate::cors::CorsConfig) -> Self {
        self.cors = Some(config);
        self
    }

    /// Apply a CORS policy scoped to requests whose path starts with `prefix`
    /// (e.g. `"/api"`), leaving every other route's responses untouched. The
    /// path-scoped counterpart to [`cors`](Self::cors) — the shape you want for
    /// "CORS on the REST API, not the HTML pages." Call repeatedly for several
    /// prefixes. Scoped policies are applied after (outside) the global one.
    ///
    /// ```ignore
    /// use umbral::cors::CorsConfig;
    ///
    /// App::builder()
    ///     .cors_for("/api", CorsConfig::strict()
    ///         .allow_origins(vec!["https://app.example.com"])
    ///         .allow_credentials(true))
    ///     .build()
    ///     .await?
    /// ```
    pub fn cors_for(mut self, prefix: impl Into<String>, config: crate::cors::CorsConfig) -> Self {
        self.cors_scoped.push((prefix.into(), config));
        self
    }

    /// Default every ORM write to run inside its own transaction.
    ///
    /// When `enabled = true`, terminals that opt into the contract
    /// (`Manager::create`, `Manager::bulk_create`,
    /// `Manager::get_or_create`, `QuerySet::update_values`,
    /// `QuerySet::delete`) wrap their work in a BEGIN / COMMIT pair
    /// unless the caller explicitly opts out with `.non_atomic()`.
    ///
    /// This is the safe-by-default posture: a framework that claims
    /// "secure by default" should also be "transaction-safe by
    /// default." Opting out matters mostly for high-throughput seed
    /// scripts that already wrap an outer transaction themselves.
    ///
    /// Without this flag the framework's behaviour is unchanged —
    /// writes run with whatever transaction the caller arranges. The
    /// per-call `.atomic()` / `.non_atomic()` overrides still work.
    pub fn atomic_transactions(mut self, enabled: bool) -> Self {
        self.atomic_transactions = Some(enabled);
        self
    }

    /// Compress responses with gzip / brotli (a `tower-http`
    /// `CompressionLayer`). The algorithm is chosen from the request's
    /// `Accept-Encoding`; already-encoded or non-compressible content types
    /// are skipped automatically.
    ///
    /// Off by default: in most deployments the reverse proxy (nginx, a CDN)
    /// already compresses, and doing it twice is wasted CPU. Enable this
    /// when you serve directly (a single binary with no proxy in front).
    pub fn compression(mut self) -> Self {
        self.compress = true;
        self
    }

    /// Register a framework-level [`Middleware`](crate::middleware::Middleware)
    /// (feature #68) with `before_request` / `after_response` hooks.
    ///
    /// App-level middleware is added to the stack *before* any plugin's
    /// contribution, so its `before_request` runs first and its
    /// `after_response` runs last (it's the outermost layer of the onion).
    /// Call this multiple times to register several, in order.
    ///
    /// Use this for the common "look at every request / response" case.
    /// For a real tower `Layer` (timeouts, body limits) reach for the
    /// router directly via a plugin's `wrap_router`.
    pub fn middleware<M: crate::middleware::Middleware>(mut self, mw: M) -> Self {
        self.middleware.push(std::sync::Arc::new(mw));
        self
    }

    /// Finalize the application.
    ///
    /// Phases (see spec 01 §Mechanics and invariants and spec 02
    /// §Dependency ordering):
    ///
    /// 1. **Collect.** Gather settings, databases, and router from
    ///    builder-local state. Settings must be set explicitly via
    ///    `.settings(...)`; the "default" database pool must be
    ///    registered via `.database("default", pool)`. The caller
    ///    opens the pool first (with `umbral::db::connect(...).await`)
    ///    and hands it to the builder. This matches the canonical
    ///    pattern in spec 01-app-and-settings.md.
    /// 2. **Validate plugins.** Reject the reserved `"app"` name,
    ///    reject duplicate `Plugin::name()`s, verify every entry in a
    ///    `dependencies()` list points at a registered plugin, and
    ///    compute a stable topological order. Cycles surface as
    ///    `BuildError::PluginCycle`.
    /// 3. **Detect backend.** `backend::detect(&settings.database_url)`
    ///    picks one of the shipped `DatabaseBackend` impls (M4
    ///    abstraction). An unknown URL scheme (mysql / oracle / etc.)
    ///    fails here, before any system check runs.
    /// 4. **Publish ambient state.** Write settings, pools, and the
    ///    active backend into their `OnceLock`s. The model registry
    ///    carries one entry per plugin (the implicit `"app"` plus every
    ///    registered plugin's `Plugin::models()`).
    /// 5. **System check.** Run framework-built-in checks plus every
    ///    plugin's `system_checks()` (concatenated in topological order)
    ///    against the just-published context. Errors block boot;
    ///    warnings log and continue.
    /// 6. **Build router.** Start from the hand-written router (or a
    ///    fallback handler), then merge every plugin's `routes()` in
    ///    topological order. axum's `Router::merge` panics on
    ///    duplicate routes with a clear message.
    /// 7. **Fire `on_ready`.** Call each plugin's `on_ready(&AppContext)`
    ///    in topological order. A failure here surfaces as
    ///    `BuildError::PluginOnReady`.
    ///
    /// `build()` is intentionally sync. Earlier iterations auto-opened
    /// the default pool from `settings.database_url` by spinning up a
    /// throwaway tokio runtime to drive `db::connect`. That panicked
    /// when called from inside any caller that was already in a tokio
    /// runtime ("Cannot start a runtime from within a runtime"), which
    /// is every realistic case. Requiring an explicit `.database(...)`
    /// is both spec-correct and avoids the trap.
    pub fn build(mut self) -> Result<App, BuildError> {
        // Phase 1 — collect
        let settings = self.settings.take().ok_or(BuildError::SettingsMissing)?;

        if !self.databases.contains_key("default") {
            return Err(BuildError::DefaultPoolMissing);
        }

        // Phase 1.5 — validate plugins and compute a stable topological
        // order. Reserved-name and duplicate-name checks reject the
        // build before any ambient state gets published; the toposort
        // surfaces both missing deps and cycles as `BuildError`. The
        // sorted vec is reused in phases 3 / 4 / 5 / 6 so every plugin
        // walk reads from one canonical order, then handed to `App` so
        // post-build callers (notably `umbral::cli::dispatch`) can walk
        // the same list.
        let sorted_plugins = sort_plugins(std::mem::take(&mut self.plugins))?;

        // Phase 2 — detect backend from the configured URL.
        let backend =
            crate::backend::detect(&settings.database_url).map_err(BuildError::BackendDetect)?;

        // Phase 2.1 — cross-check the registered default pool's
        // backend against the URL-derived one. A mismatch (e.g. the
        // URL says `sqlite://` but the caller passed in a `PgPool`)
        // surfaces here with a clear name pair rather than as a
        // confusing query-time error.
        let default_pool = self
            .databases
            .get("default")
            .expect("contains_key check above");
        if default_pool.backend_name() != backend.name() {
            return Err(BuildError::DatabaseBackendMismatch {
                url_backend: backend.name(),
                pool_backend: default_pool.backend_name(),
            });
        }

        // Phase 2.5 — validate every plugin's `database()` alias
        // against the registered pool set BEFORE phase 3 moves
        // `self.databases` into the ambient registry. Lets a typo
        // surface at boot with a clear diagnostic instead of as a
        // runtime "no pool registered" panic from `db::pool_for`.
        // Also collect the per-model alias map for `init_model_aliases`
        // below. Two layers: plugin-level (`Plugin::database()`) and
        // per-model (`#[umbral(database = "alias")]` → `Model::DATABASE`,
        // surfaced via `ModelMeta::database`). Per-model wins when both
        // are set — useful for a plugin that owns one model on the
        // primary DB and another on an analytics/archive DB. Same alias
        // validation: a typo surfaces at boot, not at runtime.
        let mut model_aliases: HashMap<String, String> = HashMap::new();
        for plugin in &sorted_plugins {
            // Plugin-level default for every model this plugin contributes.
            if let Some(alias) = plugin.database() {
                if !self.databases.contains_key(alias) {
                    return Err(BuildError::PluginDatabaseAlias {
                        plugin: plugin.name(),
                        alias,
                    });
                }
                for model in plugin.models() {
                    model_aliases.insert(model.name, alias.to_string());
                }
            }
            // Per-model overrides — walked AFTER the plugin pass so they
            // can supersede the plugin's choice.
            for model in plugin.models() {
                if let Some(alias) = &model.database {
                    if !self.databases.contains_key(alias) {
                        return Err(BuildError::PluginDatabaseAlias {
                            plugin: plugin.name(),
                            alias: Box::leak(alias.clone().into_boxed_str()),
                        });
                    }
                    model_aliases.insert(model.name.clone(), alias.clone());
                }
            }
        }
        // Same per-model walk for the implicit `"app"` plugin's
        // user-registered models, which don't have a `Plugin::database()`
        // wrapper to inherit from.
        for model in &self.models {
            if let Some(alias) = &model.database {
                if !self.databases.contains_key(alias) {
                    return Err(BuildError::PluginDatabaseAlias {
                        plugin: crate::migrate::APP_PLUGIN_NAME,
                        alias: Box::leak(alias.clone().into_boxed_str()),
                    });
                }
                model_aliases.insert(model.name.clone(), alias.clone());
            }
        }

        // Phase 2.5b — cross-database foreign-key guard (gaps2 #22).
        //
        // A foreign key whose target model lives on a DIFFERENT database
        // can't be a real DB constraint — `REFERENCES` can't span pools.
        // We resolve each model's effective alias (plugin default, then
        // per-model override, else "default") into a table→alias map,
        // then check every FK column: if the column's target table
        // routes to a different alias than the model AND the field has
        // not opted out via `#[umbral(db_constraint = false)]`, the build
        // fails loudly here rather than emitting an invalid `FOREIGN KEY`
        // line at migration time.
        //
        // Build the table→alias map with the same precedence as
        // `model_aliases` above: plugin default first, per-model override
        // wins, the implicit "app" models last. Any table not mentioned
        // routes to "default".
        let mut table_alias: HashMap<String, String> = HashMap::new();
        for plugin in &sorted_plugins {
            let plugin_default = plugin.database();
            for model in plugin.models() {
                let alias = model
                    .database
                    .clone()
                    .or_else(|| plugin_default.map(|s| s.to_string()))
                    .unwrap_or_else(|| "default".to_string());
                table_alias.insert(model.table.clone(), alias);
            }
        }
        for model in &self.models {
            let alias = model
                .database
                .clone()
                .unwrap_or_else(|| "default".to_string());
            table_alias.insert(model.table.clone(), alias);
        }
        // Helper to resolve a table's alias, defaulting to "default".
        let alias_of = |table: &str| -> String {
            table_alias
                .get(table)
                .cloned()
                .unwrap_or_else(|| "default".to_string())
        };
        // Walk every model's FK fields and check each FK relation. The
        // default (no custom router) path keeps today's build-time local
        // alias equality (`alias_of(a) == alias_of(b)`): the trait's
        // DEFAULT `allow_relation` reads the GLOBAL `model_alias`, which is
        // still unpublished at this Phase 2.5b point, so routing the
        // default case through the trait would compare "default" == "default"
        // for everything and silently disable the #22 guard. A CUSTOM router
        // is asked directly via `allow_relation`.
        //
        // Materialize the models into a Vec so we can both build a
        // table→meta lookup AND iterate them.
        let all_models: Vec<ModelMeta> = sorted_plugins
            .iter()
            .flat_map(|p| p.models())
            .chain(self.models.iter().cloned())
            .collect();
        let meta_by_table: HashMap<&str, &ModelMeta> =
            all_models.iter().map(|m| (m.table.as_str(), m)).collect();
        // Clone the candidate router — install still happens at Phase 3, so
        // we must NOT take/consume `self.db_router` here.
        let candidate_router = self.db_router.clone();
        for model in &all_models {
            for field in &model.fields {
                let Some(target_table) = field.fk_target.as_deref() else {
                    continue;
                };
                if !field.db_constraint {
                    continue;
                }
                let allowed = match &candidate_router {
                    Some(r) => match meta_by_table.get(target_table) {
                        Some(target_meta) => r.allow_relation(model, target_meta),
                        // Target isn't a registered model (shouldn't happen
                        // for a real FK); don't false-reject — fall back to
                        // the local alias check.
                        None => alias_of(&model.table) == alias_of(target_table),
                    },
                    // No custom router: today's build-time local alias
                    // equality (#22).
                    None => alias_of(&model.table) == alias_of(target_table),
                };
                if !allowed {
                    let model_db = alias_of(&model.table);
                    let target_db = alias_of(target_table);
                    return Err(BuildError::CrossDatabaseForeignKey {
                        model: Box::leak(model.name.clone().into_boxed_str()),
                        field: Box::leak(field.name.clone().into_boxed_str()),
                        model_db: Box::leak(model_db.into_boxed_str()),
                        target_db: Box::leak(target_db.into_boxed_str()),
                    });
                }
            }
        }

        // Phase 2.6 — publish the default-error-pages flag before the
        // templates engine starts so `errors::default_pages_enabled()` is
        // correct the moment any 404/500 helper is called.
        crate::errors::init_default_pages(self.default_error_pages);

        // Phase 3 — publish ambient state. The model registry now carries
        // one entry per registered plugin (the implicit `"app"` plugin
        // for `.model::<T>()` registrations, plus every `.plugin(...)`
        // contribution). Plugins that contribute zero models still get a
        // map entry; the flattening in `migrate::init_plugins` collapses
        // them to nothing in the registry but the per-plugin model walk
        // stays deterministic.
        crate::settings::init(&settings);
        db::init(self.databases);
        if let Some(router) = self.db_router {
            crate::db::router::install_router(router);
        }
        crate::backend::init(backend);
        if let Some(enabled) = self.atomic_transactions {
            db::init_atomic_default(enabled);
        }

        let mut per_plugin: HashMap<String, Vec<ModelMeta>> = HashMap::new();
        per_plugin.insert(
            crate::migrate::APP_PLUGIN_NAME.to_string(),
            std::mem::take(&mut self.models),
        );
        for plugin in &sorted_plugins {
            per_plugin.insert(plugin.name().to_string(), plugin.models());
        }
        crate::migrate::init_plugins(per_plugin);

        // Publish the topological plugin order so the migration engine
        // walks plugins in dependency order. The "app" plugin is the
        // implicit owner of `.model::<T>()` registrations; it has no
        // dependencies and lands first.
        let mut order: Vec<String> = vec![crate::migrate::APP_PLUGIN_NAME.to_string()];
        for plugin in &sorted_plugins {
            order.push(plugin.name().to_string());
        }
        crate::migrate::init_plugin_order(order);

        // Collect every plugin's advertised API endpoints into a global
        // so a discovery surface (umbral-rest's API root) can list them
        // without depending on the contributing plugins' crates. In
        // registration order; plugins that advertise nothing contribute
        // nothing.
        let mut api_endpoints = Vec::new();
        for plugin in &sorted_plugins {
            api_endpoints.extend(plugin.api_endpoints());
        }
        crate::migrate::init_api_endpoints(api_endpoints);

        // Publish the per-plugin model alias map collected in phase
        // 2.5. Done after `migrate::init_plugins` so the migration
        // registry is alive when QuerySet's resolve_pool starts
        // looking up by `Model::NAME`.
        crate::migrate::init_model_aliases(model_aliases);

        // Snapshot the declared route paths into the registry so the
        // dev-mode 404 page can surface them. The implicit `"app"`
        // plugin holds whatever `.route_paths([...])` declared on the
        // builder; each registered plugin contributes its own list.
        // Empty entries are kept so the listing distinguishes "plugin
        // present, no routes" from "plugin absent".
        let mut route_registry = crate::routes::RouteRegistry::default();
        route_registry.by_plugin.insert(
            crate::migrate::APP_PLUGIN_NAME.to_string(),
            std::mem::take(&mut self.route_paths),
        );
        for plugin in &sorted_plugins {
            route_registry
                .by_plugin
                .insert(plugin.name().to_string(), plugin.route_paths());
        }
        crate::routes::init(route_registry);

        // BUG-20: publish every plugin's OpenAPI path contribution
        // so umbral-openapi can merge them into the emitted spec.
        // Flat (path, value) list — multiple plugins contributing
        // the same path produce duplicate entries; umbral-openapi's
        // merge step picks the first.
        let mut openapi_entries: Vec<(String, serde_json::Value)> = Vec::new();
        for plugin in &sorted_plugins {
            openapi_entries.extend(plugin.openapi_paths());
        }
        crate::routes::init_openapi(openapi_entries);

        // Templates engine — published before phase 4 so a future
        // plugin system_check that wants to inspect the loaded
        // templates can.
        //
        // Search order (first-match-wins, matches Django's APP_DIRS semantics):
        //   1. App-level dir: set via `.templates_dir(...)` or `./templates`.
        //   2. Plugin dirs: each plugin's `templates_dirs()` contributions,
        //      in topological dependency order.
        //
        // The engine warns (via tracing) when two directories ship a
        // template with the same name — the first-registered copy wins.
        let app_templates_dir = self
            .templates_dir
            .take()
            .unwrap_or_else(|| std::path::PathBuf::from("templates"));
        let mut all_template_dirs: Vec<std::path::PathBuf> = vec![app_templates_dir];
        for plugin in &sorted_plugins {
            all_template_dirs.extend(plugin.templates_dirs());
        }
        // features.md #67 — collect every plugin's custom tags/filters in
        // topological order so a dependency's registrar runs before its
        // dependent's (and a later plugin can override an earlier one).
        let mut template_registrars: Vec<crate::templates::TemplateRegistrar> = Vec::new();
        for plugin in &sorted_plugins {
            template_registrars.extend(plugin.template_registrars());
        }
        // `init_with` returns the list of collision names (templates present
        // in more than one directory). We log each one via tracing here so
        // the `App::build()` phase is the single point that handles warnings;
        // `templates::init` itself also emits tracing::warn! for each, but
        // returning the list lets callers (tests) assert without a subscriber.
        let _collisions = crate::templates::init_with(&all_template_dirs, template_registrars)
            .map_err(BuildError::TemplatesInit)?;

        // Phase 4 — system check. Build the context against ambient
        // state, run the framework checks plus every plugin's
        // contribution in topological order, partition into errors vs
        // warnings, log the warnings, fail the build on any errors.
        // Whether any registered plugin declares a Storage backend. Read
        // by the `field.storage_backend` check; computed from the
        // capability flag (not the ambient `storage_opt()`) because
        // backends register in `on_ready`, which runs *after* this phase.
        let provides_storage = sorted_plugins.iter().any(|p| p.provides_storage());
        let plugin_names: Vec<&str> = sorted_plugins.iter().map(|p| p.name()).collect();
        let ctx = crate::check::CheckContext {
            backend,
            settings: crate::settings::get(),
            provides_storage,
            registered_plugin_names: &plugin_names,
        };
        let mut checks = crate::check::framework_checks();
        for plugin in &sorted_plugins {
            checks.extend(plugin.system_checks());
        }
        let findings = crate::check::run_all(&ctx, &checks);
        let mut errors = Vec::new();
        for finding in findings {
            match finding.severity {
                crate::check::Severity::Error => errors.push(finding),
                crate::check::Severity::Warning => {
                    tracing::warn!(
                        check = finding.check_id,
                        "umbral system check warning: {}",
                        finding.message
                    );
                }
            }
        }
        if !errors.is_empty() {
            return Err(BuildError::SystemCheckFailed { findings: errors });
        }

        // Phase 5 — build the merged router. Start from the hand-written
        // router (or a fallback handler if none was registered), then
        // merge every plugin's routes in topological order. axum's
        // `Router::merge` composes path tables; conflicts panic with a
        // clear message.
        let mut router = self.router.unwrap_or_else(|| {
            Router::new().fallback(|| async { "umbral is running, but no routes are registered." })
        });
        for plugin in &sorted_plugins {
            router = router.merge(plugin.routes());
            // Phase 5.4 — mount the plugin's `include_bytes!`-embedded
            // assets. Each StaticFile becomes a GET route serving the
            // body with the supplied content-type + cache-control.
            for file in plugin.static_files() {
                router = router.route(
                    file.url_path,
                    axum::routing::get(move || async move {
                        use axum::response::IntoResponse;
                        let cc = file.cache_control.unwrap_or("public, max-age=86400");
                        axum::http::Response::builder()
                            .status(axum::http::StatusCode::OK)
                            .header(axum::http::header::CONTENT_TYPE, file.content_type)
                            .header(axum::http::header::CACHE_CONTROL, cc)
                            .body(axum::body::Body::from(file.body))
                            .unwrap_or_else(|_| {
                                axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
                            })
                    }),
                );
            }
        }

        // Phase 5.45 — mount the unified static pipeline handler. Walk
        // every plugin's `static_dirs()` into a namespace -> source_dir
        // registry (a duplicate namespace fails the build loudly), then
        // nest ONE handler at the configured `static_url` base. It
        // resolves `/static/<ns>/<rest>` live-from-source in dev and
        // from `static_root` in prod (see `crate::static_files`).
        //
        // This coexists with the `StaticFile` embedded routes mounted in
        // Phase 5.4 above — embedded assets stay the zero-config default;
        // the filesystem handler is additive.
        //
        // A CDN-style `static_url` (an absolute http(s):// origin) can't
        // be nested as a local route prefix; in that mode assets are
        // served off the CDN and the local handler is intentionally not
        // mounted — the `static()` template helper still emits the
        // absolute URLs.
        let settings = crate::settings::get();
        let static_base = settings.static_url.trim_end_matches('/');
        let is_cdn_url = settings.static_url.starts_with("http://")
            || settings.static_url.starts_with("https://")
            || settings.static_url.starts_with("//");

        // App/site-level static dirs served at the bare `static_url` root.
        // A `StoragePlugin`'s static side mounted AT `static_url` contributes its
        // directory here (and skips nesting its own catch-all), so the
        // framework owns `static_url` as ONE mount — a second
        // `/static/{*rest}` nest is exactly the conflict this avoids.
        let root_dirs = crate::static_files::StaticContribution::collect_root_dirs(&sorted_plugins);

        // Publish the static contributions ambiently for `collectstatic`
        // (the `StoragePlugin` CLI command). Published UNCONDITIONALLY —
        // before the serving-mode gate below — because `collectstatic`
        // copies assets to disk regardless of serving mode (a CDN-mode
        // app still needs the disk tree built for upload). Mirrors the
        // `settings` ambient OnceLock: read-only config set once at build.
        crate::static_files::publish_static(crate::static_files::PublishedStatic {
            contributions: crate::static_files::StaticContribution::collect(&sorted_plugins),
            root_dirs: root_dirs.clone(),
        });

        // Load the hashed-asset manifest (`<static_root>/staticfiles.json`)
        // if `collectstatic --hashed` has produced one. With a manifest
        // present, `resolve_static_url` / the `static()` template global
        // emit content-hashed URLs so prod assets carry far-future cache
        // headers. Absent (no `--hashed` run), this is a no-op and URLs
        // stay plain. Loaded unconditionally — the URL resolution applies
        // whether or not this app serves the bytes itself.
        crate::static_files::load_manifest(&settings.static_root);

        if !is_cdn_url && !static_base.is_empty() {
            let registry = crate::static_files::StaticRegistry::from_plugins(&sorted_plugins)
                .map_err(|c| BuildError::DuplicateStaticNamespace {
                    namespace: c.namespace,
                    first_plugin: c.first_plugin,
                    second_plugin: c.second_plugin,
                })?;
            // Nothing to serve and no app static dirs — don't claim the
            // `static_url` path at all, so a consumer that wants to mount
            // their own router there can.
            if !registry.is_empty() || !root_dirs.is_empty() {
                let state = crate::static_files::StaticHandlerState {
                    registry,
                    static_root: std::path::PathBuf::from(&settings.static_root),
                    root_dirs,
                    dev: matches!(settings.environment, crate::settings::Environment::Dev),
                };
                let static_router = Router::new()
                    .fallback(crate::static_files::static_handler)
                    .with_state(state);
                router = router.nest_service(static_base, static_router);
            }
        }

        // Phase 5.5 — apply each plugin's middleware in topological
        // order. Later plugins wrap earlier ones, so a security
        // plugin declared after the auth plugin sees the auth-
        // augmented router and can add its own layer on top. This
        // is the M7 deferral being lifted now that umbral-security
        // needs it.
        for plugin in &sorted_plugins {
            router = plugin.wrap_router(router);
        }

        // Phase 5.6 — install the 404 fallback. Four cases:
        //
        // 1. slash_redirect = Off, not_found_template = None, default pages off:
        //    no-op. axum's built-in empty 404 is what users see.
        // 2. slash_redirect = Off, not_found_template = None, default pages ON:
        //    install the not-found fallback; render_not_found will use the
        //    embedded default_404 template.
        // 3. slash_redirect = Off, not_found_template = Some(name):
        //    install the not-found fallback directly. Renders the
        //    template on every miss.
        // 4. slash_redirect != Off:
        //    install the slash-redirect fallback. It handles its own
        //    404 path internally — when no alternate matches, it
        //    renders the configured not-found template (or the default
        //    if enabled, or plain text if both are absent).
        //
        // The slash-redirect fallback ALWAYS captures a router
        // snapshot taken BEFORE the fallback is installed, so the
        // alternate-path probe can't recursively re-hit the fallback.
        let need_not_found_fallback = self.not_found_template.is_some() || self.default_error_pages;
        match (self.slash_redirect, need_not_found_fallback) {
            (crate::slash::SlashRedirect::Off, false) => {
                // axum's default 404 — nothing to do.
            }
            (crate::slash::SlashRedirect::Off, true) => {
                let fallback = crate::errors::not_found_fallback(self.not_found_template.clone());
                router = router.fallback(fallback);
            }
            (policy, _) => {
                let snapshot = router.clone();
                let fallback = crate::slash::slash_redirect_fallback(
                    snapshot,
                    policy,
                    self.not_found_template.clone(),
                );
                router = router.fallback(fallback);
            }
        }

        // Phase 5.65 — framework middleware stack (feature #68). App-level
        // middleware first, then every plugin's contribution in topological
        // order, collected into one stack and installed as a single layer.
        // Placed AFTER the 404 fallback so middleware sees misses too, and
        // BEFORE the panic / compression / CORS / host layers so those stay
        // the outermost wrappers (security and content-encoding run before
        // user middleware ever touches the request).
        let mut middleware_stack = crate::middleware::MiddlewareStack::new();
        middleware_stack.extend(std::mem::take(&mut self.middleware));
        for plugin in &sorted_plugins {
            middleware_stack.extend(plugin.middleware());
        }
        router = middleware_stack.apply(router);

        // Phase 5.66 — request-scoped routing context (DatabaseRouter
        // foundation). When a resolver is registered, wrap the whole
        // downstream future in `route_context::scope`. Installed OUTSIDE the
        // middleware stack above so the task-local is established before any
        // middleware or handler runs — every `.await` in the request,
        // including ORM calls that read `route_context::current()`, then sees
        // the resolved context. A `from_fn` layer is the only mechanism that
        // can wrap `next.run(req)` in a scope; the `Middleware` contract's
        // `before_request(req) -> req` cannot.
        if let Some(resolver) = self.route_context_resolver.take() {
            router = router.layer(axum::middleware::from_fn_with_state(
                resolver,
                route_context_scope_layer,
            ));
        }

        // Phase 5.7 — wrap with the panic-catch layer. Comes AFTER the
        // fallback wiring so a panicking fallback handler is also caught
        // (the panic-catch layer wraps the entire router).
        //
        // Always installed when: a user-supplied server_error_template is
        // set, OR default pages are enabled (the embedded default_500 fires
        // in that case), OR an on_server_error hook is registered.
        let need_panic_layer = self.server_error_template.is_some()
            || self.default_error_pages
            || self.server_error_hook.is_some();
        if need_panic_layer {
            let handler = crate::errors::server_error_panic_handler(
                self.server_error_template.clone(),
                self.server_error_hook.clone(),
            );
            router = router.layer(tower_http::catch_panic::CatchPanicLayer::custom(handler));

            // Phase 5.8 — wrap with the response-rendering middleware so
            // any 500 produced by a handler (not just a panic) gets
            // re-rendered through the configured 500 template. The
            // middleware checks Content-Type: HTML responses (from the
            // panic handler above, or from a handler that rendered its
            // own template) pass through; plain-text 500s get re-rendered.
            // Also fires `on_server_error` for handler-Err paths.
            let render_state = crate::errors::Render500State {
                template: self.server_error_template.clone(),
                hook: self.server_error_hook.clone(),
            };
            router = router.layer(axum::middleware::from_fn_with_state(
                render_state,
                crate::errors::render_500_middleware,
            ));
        }

        // General custom error pages: style any registered status code
        // (429/403/410/…) the way the 500 path does, for handler-Err
        // responses — rendering each through its template while preserving the
        // status. Already-HTML and unregistered statuses pass through; this is
        // independent of the 500 layer above (different status codes).
        if !self.error_templates.is_empty() {
            let state = crate::errors::RenderErrorState {
                templates: std::sync::Arc::new(std::mem::take(&mut self.error_templates)),
            };
            router = router.layer(axum::middleware::from_fn_with_state(
                state,
                crate::errors::render_error_middleware,
            ));
        }

        // Optional response compression (gzip / brotli), opt-in via
        // `AppBuilder::compression`. tower-http chooses the algorithm from
        // `Accept-Encoding` and skips already-encoded / non-compressible
        // bodies. Applied here so it wraps handler responses; CORS + host
        // checks layer outside it.
        if self.compress {
            router = router.layer(tower_http::compression::CompressionLayer::new());
        }

        // Phase 5.9 — CORS, applied last so it's the outermost
        // wrapper. Preflight `OPTIONS` is answered before any
        // plugin/handler sees the request; response headers are
        // added on the way back out regardless of which downstream
        // layer produced the body.
        if let Some(cors) = self.cors.take() {
            router = router.layer(cors.into_layer());
        }
        // Path-scoped CORS (e.g. `/api`) — layered after the global one so each
        // only touches responses for requests under its prefix.
        for (prefix, config) in std::mem::take(&mut self.cors_scoped) {
            router = router.layer(crate::cors::ScopedCorsLayer::new(
                prefix,
                config.into_layer(),
            ));
        }

        // Phase 5.95 — Host-header validation (Django ALLOWED_HOSTS). Applied
        // outermost so a forged `Host` is rejected with a 400 before any
        // handler, plugin, or CORS logic runs. Enforced only in
        // `Environment::Prod`; dev passes through. Allowlist is
        // `settings.allowed_hosts` (`"*"` disables; `.example.com` = subdomain).
        let host_policy = crate::hosts::HostPolicy::new(
            &settings.allowed_hosts,
            matches!(settings.environment, crate::settings::Environment::Prod),
        );
        router = router.layer(axum::middleware::from_fn_with_state(
            host_policy,
            crate::hosts::host_guard,
        ));

        // Phase 5.99 — request tracing span. Applied outermost so every request
        // (including host-guard rejections) runs inside a span. The span
        // carries `http.method`, `http.route`/`uri`, and the response
        // `http.status_code`; this is what an OpenTelemetry layer (installed by
        // an app via `umbral_logs::observability::init`) exports as one span per
        // request. Without an OTel layer attached it's a cheap `tracing` span
        // that the fmt subscriber can surface under `RUST_LOG=tower_http=debug`.
        // W3C `traceparent` propagation (extracting an upstream trace context
        // from the inbound header) is a noted follow-up; this layer creates the
        // local request span.
        router = router.layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(
                |request: &axum::http::Request<axum::body::Body>| {
                    tracing::info_span!(
                        "http.request",
                        http.method = %request.method(),
                        http.route = %request.uri().path(),
                        http.status_code = tracing::field::Empty,
                    )
                },
            ),
        );

        // Phase 6 — fire each plugin's `on_ready` in topological order.
        // Runs after the system check passes and after the router is
        // built, so a plugin can rely on ambient state being live and on
        // any earlier dependency's `on_ready` having already run.
        let ctx = crate::plugin::AppContext {
            pool: crate::db::pool_dispatched().clone(),
            settings: crate::settings::get().clone(),
        };
        for plugin in &sorted_plugins {
            plugin
                .on_ready(&ctx)
                .map_err(|source| BuildError::PluginOnReady {
                    plugin: plugin.name(),
                    source,
                })?;
        }

        Ok(App {
            router,
            plugins: sorted_plugins,
        })
    }
}

/// The axum middleware fn installed by [`AppBuilder::route_context`]: run the
/// resolver against the incoming request to build a [`crate::db::RouteContext`],
/// then drive the ENTIRE downstream future inside
/// [`crate::db::route_context::scope`]. Scoping `next.run(req)` (rather than
/// just a prefix of it) is what keeps the task-local alive across every
/// `.await` the handler performs, so ambient ORM calls route per the resolved
/// context.
async fn route_context_scope_layer(
    axum::extract::State(resolver): axum::extract::State<RouteContextResolver>,
    req: crate::web::Request,
    next: axum::middleware::Next,
) -> crate::web::Response {
    let ctx = resolver(&req);
    crate::db::route_context::scope(ctx, next.run(req)).await
}

/// Validate the registered plugins and return them in a stable
/// topological order keyed by `Plugin::dependencies()`. Standard Kahn's
/// algorithm with a name-sorted ready queue so ties resolve
/// deterministically.
///
/// Rejects:
///
/// - A plugin claiming the reserved `"app"` name.
/// - Two plugins reporting the same `name()`.
/// - A `dependencies()` entry that doesn't name a registered plugin.
/// - A dependency cycle (the remaining-unsorted set surfaces as
///   `BuildError::PluginCycle`).
fn sort_plugins(plugins: Vec<Box<dyn Plugin>>) -> Result<Vec<Box<dyn Plugin>>, BuildError> {
    use std::collections::{BTreeMap, BTreeSet};

    // Reserved + duplicate-name checks. The implicit `"app"` plugin is
    // not counted toward duplicates; only the user's plugin list is.
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    for plugin in &plugins {
        let name = plugin.name();
        if name == crate::migrate::APP_PLUGIN_NAME {
            return Err(BuildError::ReservedPluginName);
        }
        if !seen.insert(name) {
            return Err(BuildError::DuplicatePluginName { name });
        }
    }

    // Index plugins by name for the dependency lookups + the
    // sort-by-name traversal below. We pull the boxes out of the
    // input vec by index later, so the index table stays alongside.
    let by_name: BTreeMap<&'static str, usize> = plugins
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name(), i))
        .collect();

    // Dependency-exists check. Done before the toposort so a missing
    // dep surfaces with the asking plugin's name attached, not as a
    // cycle false-positive.
    for plugin in &plugins {
        for dep in plugin.dependencies() {
            if !by_name.contains_key(dep) {
                return Err(BuildError::DependencyNotFound {
                    plugin: plugin.name(),
                    missing: dep,
                });
            }
        }
    }

    // Kahn's algorithm against the index table. `remaining_deps[name]`
    // is the set of names this plugin still waits on; once it empties,
    // the plugin joins the ready queue. The queue is a sorted set so
    // ties resolve by name.
    let mut remaining_deps: BTreeMap<&'static str, BTreeSet<&'static str>> = plugins
        .iter()
        .map(|p| (p.name(), p.dependencies().iter().copied().collect()))
        .collect();

    let mut ready: BTreeSet<&'static str> = remaining_deps
        .iter()
        .filter_map(|(name, deps)| if deps.is_empty() { Some(*name) } else { None })
        .collect();

    let mut order: Vec<&'static str> = Vec::with_capacity(plugins.len());
    while let Some(name) = ready.iter().next().copied() {
        ready.remove(&name);
        remaining_deps.remove(&name);
        order.push(name);
        for (other_name, deps) in remaining_deps.iter_mut() {
            if deps.remove(&name) && deps.is_empty() {
                ready.insert(*other_name);
            }
        }
    }

    if !remaining_deps.is_empty() {
        let names: Vec<&'static str> = remaining_deps.keys().copied().collect();
        return Err(BuildError::PluginCycle { names });
    }

    // Reorder the owned boxes into topological order. We pull each
    // plugin out of an `Option` slot so the move is statically
    // tracked; every slot is taken exactly once because the toposort
    // produced one entry per plugin.
    let mut slots: Vec<Option<Box<dyn Plugin>>> = plugins.into_iter().map(Some).collect();
    let mut sorted: Vec<Box<dyn Plugin>> = Vec::with_capacity(order.len());
    for name in order {
        let idx = by_name[&name];
        sorted.push(
            slots[idx]
                .take()
                .expect("toposort produced one entry per plugin"),
        );
    }
    Ok(sorted)
}

/// Errors that can occur during `AppBuilder::build()`.
#[derive(Debug)]
pub enum BuildError {
    /// `.settings(Settings)` wasn't called on the builder.
    SettingsMissing,
    /// `.database("default", pool)` wasn't called on the builder.
    DefaultPoolMissing,
    /// The URL scheme in `settings.database_url` doesn't match any
    /// shipped backend.
    BackendDetect(crate::backend::BackendDetectError),
    /// One or more system checks failed with `Severity::Error`. The
    /// full list of findings is in the variant.
    SystemCheckFailed {
        findings: Vec<crate::check::SystemCheckFinding>,
    },
    /// A plugin's `dependencies()` lists a plugin that was never
    /// registered with `.plugin(...)`. Carries the unmet name plus
    /// the plugin that asked for it.
    DependencyNotFound {
        plugin: &'static str,
        missing: &'static str,
    },
    /// The dependency graph has a cycle. Carries the plugin names that
    /// form it (in any cyclic order; the diagnostic is "these N plugins
    /// reference each other").
    PluginCycle { names: Vec<&'static str> },
    /// Two registered plugins share a `name()`. Plugin names are keys
    /// in the migration tracking table and the dependency graph; a
    /// collision would break both.
    DuplicatePluginName { name: &'static str },
    /// A plugin claimed the reserved `"app"` name (used by the
    /// implicit plugin that owns `.model::<T>()` registrations).
    ReservedPluginName,
    /// A plugin's `on_ready` returned an error. Carries the plugin's
    /// name plus the underlying error.
    PluginOnReady {
        plugin: &'static str,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The templates engine failed to initialise. Carries the
    /// underlying `TemplateError` (an IO error reading a template
    /// file, or a syntax error in one of the loaded templates).
    TemplatesInit(crate::templates::TemplateError),
    /// A plugin's `database()` returned an alias that isn't in the
    /// registered pool set. Surfaces a typo at boot with a clear
    /// "register the pool first" diagnostic instead of letting
    /// `db::pool_for` panic at first query.
    PluginDatabaseAlias {
        plugin: &'static str,
        alias: &'static str,
    },
    /// The URL-derived backend (from `settings.database_url`) doesn't
    /// match the runtime type of the default pool passed to
    /// `.database("default", ...)`. Catches the case where the URL
    /// says `postgres://` but a `SqlitePool` was registered, or vice
    /// versa.
    DatabaseBackendMismatch {
        url_backend: &'static str,
        pool_backend: &'static str,
    },
    /// A foreign key targets a model on a different database than the
    /// model that declares it, and the field has NOT opted out of the
    /// physical constraint. A `REFERENCES` clause can't span databases,
    /// so this would emit invalid DDL. Fix by either routing both
    /// models to the same database, or marking the FK
    /// `#[umbral(db_constraint = false)]` to keep it a logical-only
    /// relation. Closes gaps2 #22.
    CrossDatabaseForeignKey {
        model: &'static str,
        field: &'static str,
        model_db: &'static str,
        target_db: &'static str,
    },
    /// Two plugins declared the same static namespace via
    /// `Plugin::static_dirs()`. Namespaces are the per-plugin URL/disk
    /// segment under `static_url` / `static_root`; a collision would
    /// silently shadow one plugin's assets with another's, so the build
    /// fails loudly and names both plugins.
    DuplicateStaticNamespace {
        namespace: &'static str,
        first_plugin: &'static str,
        second_plugin: &'static str,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::SettingsMissing => write!(
                f,
                "umbral: App::builder() requires Settings; call .settings(Settings::from_env()?) before .build()"
            ),
            BuildError::BackendDetect(err) => write!(f, "{err}"),
            BuildError::SystemCheckFailed { findings } => {
                writeln!(f, "umbral: {} system check(s) failed:", findings.len())?;
                for finding in findings {
                    write!(f, "  - [{}] {}", finding.check_id, finding.message)?;
                    if let Some(hint) = &finding.hint {
                        write!(f, " (hint: {hint})")?;
                    }
                    writeln!(f)?;
                }
                Ok(())
            }
            BuildError::DefaultPoolMissing => write!(
                f,
                "umbral: App::builder() requires a default DB pool; call .database(\"default\", umbral::db::connect(&url).await?) before .build()"
            ),
            BuildError::DependencyNotFound { plugin, missing } => write!(
                f,
                "umbral: plugin `{plugin}` depends on `{missing}`, which isn't registered; \
                 call .plugin({missing}::default()) on the builder"
            ),
            BuildError::PluginCycle { names } => {
                write!(f, "umbral: plugin dependency cycle: {}", names.join(" -> "))
            }
            BuildError::DuplicatePluginName { name } => write!(
                f,
                "umbral: two plugins both report name `{name}`; plugin names are unique keys \
                 (migration tracking, dependency graph)"
            ),
            BuildError::ReservedPluginName => write!(
                f,
                "umbral: the plugin name `app` is reserved for models registered via \
                 .model::<T>(); pick a different name"
            ),
            BuildError::PluginOnReady { plugin, source } => {
                write!(f, "umbral: plugin `{plugin}`'s on_ready failed: {source}")
            }
            BuildError::TemplatesInit(err) => {
                write!(f, "umbral: templates engine failed to initialise: {err}")
            }
            BuildError::PluginDatabaseAlias { plugin, alias } => write!(
                f,
                "umbral: plugin `{plugin}` requested database alias `{alias}`, which isn't \
                 registered; call .database(\"{alias}\", pool) on the builder before .build()"
            ),
            BuildError::CrossDatabaseForeignKey {
                model,
                field,
                model_db,
                target_db,
            } => write!(
                f,
                "umbral: model `{model}` (database `{model_db}`) has a foreign key \
                 `{field}` to a model on database `{target_db}`. A FOREIGN KEY \
                 constraint can't span databases. Either route both models to the \
                 same database, or mark the field `#[umbral(db_constraint = false)]` \
                 to keep it a logical-only relation (joins / select_related still \
                 work; no physical constraint is emitted)."
            ),
            BuildError::DatabaseBackendMismatch {
                url_backend,
                pool_backend,
            } => write!(
                f,
                "umbral: settings.database_url names backend `{url_backend}`, but the \
                 default pool passed to .database(...) is a `{pool_backend}` pool. \
                 Either change UMBRAL_DATABASE_URL to match the pool, or open the pool \
                 against a URL whose scheme matches umbral::db::connect."
            ),
            BuildError::DuplicateStaticNamespace {
                namespace,
                first_plugin,
                second_plugin,
            } => write!(
                f,
                "umbral: plugins `{first_plugin}` and `{second_plugin}` both declare the static \
                 namespace `{namespace}` via static_dirs(); namespaces must be unique \
                 (they key the /static/<namespace>/ URL and the static_root/<namespace>/ \
                 collected-asset dir). Rename one plugin's namespace."
            ),
        }
    }
}

impl std::error::Error for BuildError {}
