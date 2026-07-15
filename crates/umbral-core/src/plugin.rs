//! The Plugin trait — umbral's only extension mechanism.
//!
//! Auth, sessions, admin, tasks, REST, and OpenAPI are all plugins; so
//! is every third-party crate that ships models, routes, or commands.
//! This module defines the contract, the `AppContext` plugins receive,
//! and the `BuildError` variants topological-sort issues surface as.
//!
//! See `docs/specs/02-plugin-contract.md` for the eventual target
//! shape; this file ships the M7 v1 subset (no middleware, no commands,
//! no inventory auto-registration).
//!
//! ## The trait
//!
//! ```ignore
//! use umbral::prelude::*;
//!
//! pub struct BlogPlugin;
//!
//! impl Plugin for BlogPlugin {
//!     fn name(&self) -> &'static str { "blog" }
//!
//!     fn dependencies(&self) -> &'static [&'static str] { &["auth"] }
//!
//!     fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
//!         vec![umbral::migrate::ModelMeta::for_::<Post>()]
//!     }
//!
//!     fn routes(&self) -> Router {
//!         Router::new().route("/posts", get(list))
//!     }
//! }
//! ```
//!
//! `AppBuilder::plugin(BlogPlugin)` registers it; `App::build()`
//! topologically sorts the registered plugins, walks every plugin's
//! routes / models / system_checks, and fires `on_ready` in dependency
//! order.

use std::path::PathBuf;

use crate::db::DbPool;
use axum::Router;

use crate::check::SystemCheck;
use crate::migrate::ModelMeta;
use crate::settings::Settings;

/// Run an async future to completion from inside a synchronous
/// `Plugin::on_ready` implementation.
///
/// `Plugin::on_ready` is a sync trait method (the trait has to be
/// object-safe for `Vec<Box<dyn Plugin>>`), but most real-world async
/// work — schema DDL via sqlx, policy setup, initial seeding — needs
/// to await. This helper bridges that gap safely under every runtime
/// configuration that umbral encounters in practice:
///
/// | Caller context | Bridge used |
/// |---|---|
/// | Multi-thread tokio runtime (`#[tokio::main]`, prod binaries) | `tokio::task::block_in_place` + `Handle::block_on` — parks the OS thread, doesn't block the executor |
/// | Current-thread tokio runtime (`#[tokio::test]` default) | Spawns a dedicated OS thread with its own `Runtime`; `block_in_place` would panic here |
/// | No ambient runtime (bare `main`, exotic callers) | Creates a temporary `Runtime` and `block_on`s |
///
/// ## Why not just `Handle::current().block_on(fut)`?
///
/// `block_on` on a `Handle` panics when called from within a
/// current-thread runtime (which is the default for `#[tokio::test]`).
/// The multi-thread path requires `block_in_place` to hand control
/// back to the executor; the current-thread path requires moving to a
/// different OS thread entirely.
///
/// ## Usage
///
/// ```rust,ignore
/// fn on_ready(&self, ctx: &AppContext) -> Result<(), PluginError> {
///     umbral::plugin::block_on_ready(self.do_async_setup(&ctx.pool))?;
///     Ok(())
/// }
/// ```
pub fn block_on_ready<F>(fut: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // We are inside a tokio runtime. The safe bridging path
            // depends on the runtime flavor:
            //
            // - Multi-thread: `block_in_place` parks the current OS
            //   thread and yields it to the executor so other tasks
            //   keep running. The `Handle::block_on` call inside then
            //   drives the future to completion on that parked thread.
            //
            // - Current-thread: `block_in_place` panics because a
            //   single-threaded executor can't lend the thread to sync
            //   work while simultaneously needing it to drive the
            //   reactor. The only safe path is to escape to a new OS
            //   thread. We use `std::thread::scope` (stable since
            //   Rust 1.63, our MSRV is 1.85) so non-`'static`
            //   borrows from the call frame can cross the thread
            //   boundary safely — the scope join guarantees the
            //   spawned thread exits before the frame does.
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                tokio::task::block_in_place(|| handle.block_on(fut))
            } else {
                // Current-thread (or unknown flavor): escape to a
                // scoped thread with its own single-thread runtime.
                std::thread::scope(|s| {
                    s.spawn(|| {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("block_on_ready: failed to build current-thread runtime")
                            .block_on(fut)
                    })
                    .join()
                    .expect("block_on_ready: scoped thread panicked")
                })
            }
        }
        Err(_) => {
            // No ambient runtime. Build a temporary one for this call.
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("block_on_ready: failed to build runtime")
                .block_on(fut)
        }
    }
}

/// The contract every umbral extension implements.
///
/// Every method except `name()` has a default that returns the empty
/// contribution. A plugin opts in only to what it contributes: a
/// pure-route plugin overrides `routes()`; a pure-data plugin
/// overrides `models()`; the auth plugin overrides almost all of them.
///
/// The trait is `Send + Sync + 'static` so `App::builder()` can store a
/// homogeneous `Vec<Box<dyn Plugin>>` and the runtime can hand the
/// plugin reference to threads (e.g. for background tasks spawned in
/// `on_ready`). The bounds are deliberately permissive: any
/// reasonable Rust struct meets them by default.
pub trait Plugin: Send + Sync + 'static {
    /// A stable identifier. Used as the key in the migration tracking
    /// table, in dependency lists, and as the directory name under
    /// `migrations/`. Plugin names live in the same namespace as
    /// `migrate::APP_PLUGIN_NAME` (`"app"`), so user crates must not
    /// pick the name `"app"`.
    fn name(&self) -> &'static str;

    /// Names of plugins that must load before this one. The
    /// `App::builder()` topological sort uses this; cycles surface as
    /// `BuildError::PluginCycle`. The default is no dependencies.
    fn dependencies(&self) -> &'static [&'static str] {
        &[]
    }

    /// The plugin's models, in declaration order. The M7 migration
    /// engine collects these across every registered plugin and uses
    /// them as the diff target for `makemigrations`.
    ///
    /// Default: no models. A pure-route or pure-middleware plugin
    /// leaves this alone.
    fn models(&self) -> Vec<ModelMeta> {
        Vec::new()
    }

    /// The plugin's HTTP routes. Merged into the app router after the
    /// hand-written one passed to `AppBuilder::routes()`. Plugins
    /// choose their own path prefixes (spec 02 §"What a plugin can
    /// contribute": routes are flat, not auto-prefixed).
    ///
    /// **Drift warning.** This method and [`route_paths`] are two
    /// independent lists; nothing forces them to agree, so a route
    /// mounted here but not declared in `route_paths()` is invisible to
    /// every audit / discovery surface (the dev 404 page, the ungated-
    /// route audit, future OpenAPI security annotations). For routes
    /// whose accuracy matters to those surfaces, implement
    /// [`routes_builder`] instead — it records each path AS you mount
    /// it, so the registry cannot drift from what's actually served.
    ///
    /// [`route_paths`]: Plugin::route_paths
    /// [`routes_builder`]: Plugin::routes_builder
    fn routes(&self) -> Router {
        Router::new()
    }

    /// The drift-free alternative to [`routes`] + [`route_paths`]
    /// (gaps4 #31): mount routes through the recording [`Routes`]
    /// builder and the framework takes BOTH the axum router and the
    /// declared [`RouteSpec`]s from that ONE source, so the route
    /// registry can never fall out of sync with what's mounted.
    ///
    /// ```ignore
    /// fn routes_builder(&self) -> Option<umbral::routes::Routes> {
    ///     Some(
    ///         Routes::new()
    ///             .get("/health", health)          // path recorded as it mounts
    ///             .post("/api/thing", create_thing) //   "        "       "
    ///     )
    /// }
    /// ```
    ///
    /// When this returns `Some`, the framework uses the builder's
    /// router for merging AND its specs for the registry, and it
    /// **ignores** this plugin's [`routes`] / [`route_paths`] entirely
    /// — implement one mechanism or the other, never both. Returning
    /// `None` (the default) keeps the legacy two-method pair.
    ///
    /// The one residual: paths inside a router merged via
    /// [`Routes::with_router`] (or an axum `nest`) still aren't
    /// recorded — axum exposes no route-table introspection — so those
    /// escape-hatch paths carry the same drift caveat as `routes()`.
    /// Everything mounted through the builder's own `get/post/route/…`
    /// methods is recorded, including per-route `.layer(...)`.
    ///
    /// [`routes`]: Plugin::routes
    /// [`route_paths`]: Plugin::route_paths
    /// [`Routes`]: crate::routes::Routes
    /// [`Routes::with_router`]: crate::routes::Routes::with_router
    /// [`RouteSpec`]: crate::routes::RouteSpec
    fn routes_builder(&self) -> Option<crate::routes::Routes> {
        None
    }

    /// Declared URL routes this plugin contributes — a companion to
    /// [`routes`] used for surfacing route lists outside the request
    /// flow (currently: the dev-mode default 404 page). axum doesn't
    /// expose its internal route table, so plugins report what they
    /// declare here; the framework treats this as informational only
    /// — not a source of truth for routing.
    ///
    /// Each entry carries a path pattern and the HTTP methods it
    /// accepts; the dev-mode 404 page renders method badges so a
    /// developer can tell at a glance which verb to use. Conversions
    /// (see [`RouteSpec`]'s `From` impls) cover the ergonomic shapes:
    /// `"/admin/login".into()`, `("GET", "/articles").into()`,
    /// `(&["GET", "POST"][..], "/api/post").into()`.
    ///
    /// Default empty. Mismatch with the real `routes()` is a stale-
    /// list bug, not a correctness bug — but if you'd rather it be
    /// impossible than merely benign, implement [`routes_builder`]
    /// (gaps4 #31), which derives this list from the routes you mount.
    ///
    /// [`routes`]: Plugin::routes
    /// [`routes_builder`]: Plugin::routes_builder
    /// [`RouteSpec`]: crate::routes::RouteSpec
    fn route_paths(&self) -> Vec<crate::routes::RouteSpec> {
        Vec::new()
    }

    /// OpenAPI path items the plugin contributes. Returned as a
    /// `Vec<(path, value)>` where `path` is the URL template
    /// (`/api/auth/login`, `/api/foo/{id}`) and `value` is the
    /// matching OpenAPI 3.0 [Path Item Object][1] serialised as
    /// a `serde_json::Value`.
    ///
    /// [`umbral-openapi`] walks every registered plugin's
    /// contribution at spec-build time and merges them into the
    /// emitted document's `paths` object. Closes BUG-20 from
    /// `bugs/tests/testBugs.md` — auto-generated CRUD routes were
    /// the only thing the spec described before; plugin-
    /// contributed routes (auth, custom actions) were invisible
    /// to Swagger UI.
    ///
    /// Plugins that don't ship OpenAPI documentation leave this
    /// alone. The umbral-openapi plugin's own routes (the
    /// `/openapi.json` and Swagger UI mount) are not in the
    /// generated spec — they're delivery, not API.
    ///
    /// [1]: https://spec.openapis.org/oas/v3.0.3#path-item-object
    /// [`umbral-openapi`]: https://docs.rs/umbral-openapi
    fn openapi_paths(&self) -> Vec<(String, serde_json::Value)> {
        Vec::new()
    }

    /// Boot-time checks the plugin needs to pass. Run in phase 4 of
    /// `App::build()` alongside the framework's built-in checks.
    /// `Severity::Error` blocks boot; `Severity::Warning` logs and
    /// continues.
    fn system_checks(&self) -> Vec<SystemCheck> {
        Vec::new()
    }

    /// `true` if this plugin registers a [`Storage`](crate::storage::Storage)
    /// backend (e.g. `StoragePlugin`, which calls
    /// [`crate::storage::set_storage`] in [`Plugin::on_ready`]).
    ///
    /// The boot system check `field.storage_backend` reads this flag to
    /// decide whether a model that declares a `FileField` / `ImageField`
    /// has somewhere to resolve its uploads. It checks the *capability
    /// flag* rather than the ambient `storage_opt()` because storage is
    /// registered in `on_ready`, which runs *after* the system-check
    /// phase — at check time the ambient backend isn't published yet, but
    /// the declared capability is knowable from the plugin list. Override
    /// this (return `true`) in any plugin whose `on_ready` registers a
    /// backend.
    fn provides_storage(&self) -> bool {
        false
    }

    /// The database alias every model this plugin contributes should
    /// be read from and written to. Returns `None` to use the
    /// `"default"` pool (the same one `umbral::db::pool()` returns).
    ///
    /// This is umbral's per-plugin database routing hook. The
    /// builder reads it during phase 3 and the QuerySet's
    /// `resolve_pool` defers to it when no `.on(&pool)` override is
    /// set on the chain. Per-plugin granularity (every model the
    /// plugin owns goes to one database) is the v1 shape; per-model
    /// overrides via attribute lands when a real workload needs it.
    ///
    /// The named alias must have been registered via
    /// `AppBuilder::database(alias, pool)` before `App::build()`. A
    /// reference to an unregistered alias surfaces as
    /// `BuildError::PluginDatabaseAlias` at boot.
    ///
    /// Note: `Settings.databases[alias]` does **not** register a pool on
    /// its own today — it is parsed config, but nothing opens a pool from
    /// it (audit_2 core-app-config #4). Open the pool yourself
    /// (`umbral::db::connect(&url).await?`) and pass it to
    /// `AppBuilder::database(alias, pool)`.
    fn database(&self) -> Option<&'static str> {
        None
    }

    /// Template directories this plugin contributes.
    ///
    /// Each path is added to the global template search list in plugin
    /// registration order. The app-level `templates_dir` (set via
    /// `AppBuilder::templates_dir`) is always searched first; plugin
    /// directories follow in topological dependency order so a plugin
    /// with no dependencies appears before its dependents.
    ///
    /// When two plugins (or the app directory and a plugin) ship a
    /// template with the same name, the first directory in the list wins
    /// and a tracing warning is emitted at boot so the collision is
    /// visible. First-match-wins across all template directories.
    ///
    /// Default: no directories. A plugin that renders no HTML leaves
    /// this alone.
    fn templates_dirs(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Custom template tags / filters this plugin contributes
    /// (feature #67 - a loadable template tag/filter library).
    ///
    /// Each returned [`TemplateRegistrar`] is a closure that mutates the
    /// minijinja [`Environment`](minijinja::Environment) at engine-build
    /// time — `env.add_filter(...)`, `env.add_function(...)`,
    /// `env.add_global(...)`. They are collected across all plugins in
    /// topological order and applied *after* the framework built-ins
    /// (`static`, `media_url`, `markdown`, `now`, `currency`, …), so a
    /// plugin may deliberately override a built-in by re-registering the
    /// same name.
    ///
    /// The closures must be owned and `'static` (no borrow of `self`) so
    /// the framework can stash them and re-run them on every dev-mode
    /// hot-reload rebuild. Capture any per-plugin config by value.
    ///
    /// ```ignore
    /// fn template_registrars(&self) -> Vec<TemplateRegistrar> {
    ///     vec![Box::new(|env| {
    ///         env.add_filter("shout", |s: String| s.to_uppercase());
    ///     })]
    /// }
    /// ```
    ///
    /// Default: no custom tags. A plugin that ships none leaves this alone.
    fn template_registrars(&self) -> Vec<crate::templates::TemplateRegistrar> {
        Vec::new()
    }

    /// Wrap the app router with the plugin's middleware layers.
    ///
    /// Called once per plugin during `App::build`'s phase 5, in
    /// topological dependency order. The plugin receives the router
    /// after its routes have already been merged in, applies any
    /// `.layer(...)` calls it needs (tower layers, axum's middleware
    /// fn helpers, etc.), and returns the wrapped router.
    ///
    /// Returning the router shape (instead of a `Vec<Layer>` like
    /// the spec sketched) sidesteps the trait-object lifetime
    /// problem Layer's generics produce. Plugins keep full access
    /// to the axum / tower API at the call site.
    ///
    /// Default: return the router unchanged. A pure-data plugin
    /// (models only) inherits this and never touches the router.
    fn wrap_router(&self, router: Router) -> Router {
        router
    }

    /// Framework-level request/response middleware this plugin contributes
    /// (feature #68).
    ///
    /// Where [`wrap_router`](Plugin::wrap_router) hands you the raw axum
    /// `Router` for arbitrary tower `Layer`s, this is the ergonomic
    /// surface: each [`Middleware`](crate::middleware::Middleware) gets a
    /// `before_request` / `after_response` hook and nothing else to wire.
    /// All plugins' middleware (plus the app's) are collected into one
    /// stack and installed as a single layer at `App::build`, in plugin
    /// topological order — a plugin's `before_request` runs after those of
    /// the plugins it depends on, and its `after_response` runs before
    /// them (onion order).
    ///
    /// Reach for `wrap_router` when you need a real tower `Layer` (timeouts,
    /// tracing spans, body-limit); reach for this when you just want to
    /// look at the request or response.
    ///
    /// Default: no middleware.
    fn middleware(&self) -> Vec<std::sync::Arc<dyn crate::middleware::Middleware>> {
        Vec::new()
    }

    /// Static files the plugin ships baked into its binary.
    ///
    /// Each entry produces one `GET <url_path>` route that returns the
    /// file body with the supplied `Content-Type` and `Cache-Control`.
    /// Bodies are `&'static [u8]` — typically `include_bytes!` —
    /// because the canonical use is "the binary ships its own CSS / JS
    /// / fonts."
    ///
    /// Use cases:
    ///   - `umbral-admin` ships its precompiled Tailwind CSS this way.
    ///   - A plugin that adds an HTMX page can ship an icon or font.
    ///   - User code can register arbitrary embedded assets.
    ///
    /// Conflicts across plugins (two plugins claiming the same
    /// `url_path`) are **not** silently resolved — axum's
    /// `Router::route` panics at `App::build` time with an "overlapping
    /// method route" error naming the path. The build fails loudly; fix
    /// the collision by giving each plugin a distinct `url_path`
    /// (namespacing under the plugin name is the convention).
    ///
    /// Default: no files. Plugins that ship no embedded assets leave
    /// this alone.
    fn static_files(&self) -> Vec<StaticFile> {
        Vec::new()
    }

    /// On-disk source directories this plugin contributes to the
    /// unified static pipeline.
    ///
    /// Where [`static_files`] bakes assets into the binary (zero-config,
    /// always available), `static_dirs` declares a *filesystem* source
    /// the framework's static handler serves live. Each entry pairs a
    /// `namespace` (the per-plugin URL/disk segment that prevents
    /// collisions — `"admin"`, `"playground"`) with the absolute
    /// `source_dir` holding that plugin's source assets (plugins
    /// typically compute it from `env!("CARGO_MANIFEST_DIR")`).
    ///
    /// At `App::build()` the framework walks every plugin's
    /// `static_dirs()` into a `namespace -> source_dir` registry and
    /// mounts one handler at the configured `static_url` (default
    /// `/static/`). A request `/static/<namespace>/<rest>` resolves:
    ///
    /// - **Dev** — `<source_dir>/<rest>` first (live source serving: drop
    ///   a rebuilt file and it's served on the next request), falling
    ///   back to `<static_root>/<namespace>/<rest>` when the namespace
    ///   isn't registered or the file is missing.
    /// - **Prod / Test** — `<static_root>/<namespace>/<rest>` only.
    ///
    /// Two plugins declaring the same `namespace` is a boot-time error
    /// ([`BuildError::DuplicateStaticNamespace`]) — collisions fail
    /// loudly, never silently shadow.
    ///
    /// Default: no directories. A plugin that ships no filesystem assets
    /// leaves this alone.
    ///
    /// [`static_files`]: Plugin::static_files
    /// [`BuildError::DuplicateStaticNamespace`]: crate::app::BuildError::DuplicateStaticNamespace
    fn static_dirs(&self) -> Vec<StaticDir> {
        Vec::new()
    }

    /// On-disk directories served at the **root** of `static_url` — with
    /// no namespace segment.
    ///
    /// Where [`static_dirs`] serves a plugin's assets under a namespaced
    /// path (`/static/<namespace>/<file>`), these directories back the
    /// bare `/static/<file>` space for app/site-level static (a project's
    /// own CSS, images, favicon). The framework's single static handler
    /// resolves a request by trying registered namespaces first, then
    /// these root directories with the full request path.
    ///
    /// This is the seam that lets the framework own `static_url` as a
    /// single mount: a `StoragePlugin`'s static side pointed at the configured
    /// `static_url` contributes its directory here instead of nesting its
    /// own (conflicting) catch-all route. A plugin serving its directory
    /// at a *different* mount returns nothing here and nests as usual.
    ///
    /// Default: none.
    ///
    /// [`static_dirs`]: Plugin::static_dirs
    fn static_root_dirs(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    /// CLI subcommands the plugin contributes.
    ///
    /// Each command implements [`crate::cli::PluginCommand`] and ships
    /// a `clap::Command` plus an async `run` handler. The framework's
    /// binary (or any user-written one) calls
    /// [`crate::cli::dispatch`] with the App's plugin list to wire
    /// these into a single CLI tree.
    ///
    /// Default: no commands. Plugins that only contribute models,
    /// routes, or middleware leave this alone.
    fn commands(&self) -> Vec<Box<dyn crate::cli::PluginCommand>> {
        Vec::new()
    }

    /// Callable HTTP endpoints this plugin wants advertised in a
    /// machine-readable index (e.g. a REST API root, or a client's
    /// service-discovery fetch).
    ///
    /// This is *not* how a plugin mounts routes — that's [`routes`].
    /// It's a declaration of which of those routes are worth surfacing
    /// to an API client, with a human label and a grouping key. The
    /// framework collects every plugin's list at `App::build()` into a
    /// global readable via [`crate::migrate::registered_api_endpoints`];
    /// a plugin like `umbral-rest` reads that global to render an API
    /// root without ever naming the plugins that contributed.
    ///
    /// Paths are relative (`/oauth/google/login`) — the core type stays
    /// origin-agnostic; a consumer joins its own origin when it needs an
    /// absolute URL.
    ///
    /// Default: nothing advertised. Plugins that don't expose a
    /// client-facing API leave this alone.
    ///
    /// [`routes`]: Plugin::routes
    fn api_endpoints(&self) -> Vec<ApiEndpoint> {
        Vec::new()
    }

    /// Wire signals, start background work, seal admin registrations.
    /// Called after phase 4 (system checks) passes, in topological
    /// dependency order. Sync, on purpose; spawn async work via
    /// `ctx.runtime()` when the runtime handle lands.
    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

/// One static file a plugin ships baked into its binary. Returned
/// from [`Plugin::static_files`].
///
/// The body is a `&'static [u8]` (usually from `include_bytes!`) so
/// the file ships with the binary; no on-disk asset directory needs
/// to exist at runtime. `cache_control` defaults to one day if left
/// `None`.
#[derive(Debug, Clone)]
pub struct StaticFile {
    /// URL path the asset is served at, e.g. `/admin/static/admin.css`.
    pub url_path: &'static str,
    /// `Content-Type` header value, e.g. `text/css; charset=utf-8`.
    pub content_type: &'static str,
    /// File body. Usually `include_bytes!("relative/path")`.
    pub body: &'static [u8],
    /// Optional `Cache-Control` header. `None` → `public, max-age=86400`.
    pub cache_control: Option<&'static str>,
}

/// One on-disk source directory a plugin contributes to the unified
/// static pipeline. Returned from [`Plugin::static_dirs`].
///
/// `namespace` is the URL/disk segment that isolates this plugin's
/// assets from every other plugin's — a request `/static/<namespace>/…`
/// and the collected output dir `<static_root>/<namespace>/…` both key
/// off it. It is a `&'static str` because plugins declare it as a
/// literal.
///
/// `source_dir` is the absolute on-disk directory holding the plugin's
/// source assets, served live in dev. It is a `PathBuf` (not a
/// `&'static str`) because plugins compute it at runtime — typically
/// `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static")`.
#[derive(Debug, Clone)]
pub struct StaticDir {
    /// Per-plugin URL/disk segment, e.g. `"admin"` or `"playground"`.
    pub namespace: &'static str,
    /// Absolute on-disk directory holding the plugin's source assets.
    pub source_dir: PathBuf,
}

impl StaticDir {
    /// Build a [`StaticDir`] from a namespace literal and any
    /// `Into<PathBuf>` source (a `PathBuf`, `&Path`, or `String`/`&str`
    /// computed from `env!("CARGO_MANIFEST_DIR")`).
    pub fn new(namespace: &'static str, source_dir: impl Into<PathBuf>) -> Self {
        Self {
            namespace,
            source_dir: source_dir.into(),
        }
    }
}

/// One callable endpoint a plugin advertises for service discovery.
/// Returned from [`Plugin::api_endpoints`] and collected at
/// `App::build()` into [`crate::migrate::registered_api_endpoints`].
///
/// The shape is deliberately minimal and origin-agnostic: `path` is
/// relative, so the type carries no assumption about the public host.
/// A consumer (a REST API root, a SPA) joins its own origin to build an
/// absolute URL. `group` lets a consumer bucket endpoints by source
/// (`"oauth"`, `"tasks"`); `name` is a stable machine key within the
/// group (`"google.login"`); `label` is the human string a UI renders.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiEndpoint {
    /// Grouping key, e.g. `"oauth"`. Lets a consumer bucket endpoints
    /// by the plugin/area that contributed them.
    pub group: String,
    /// Stable machine name within the group, e.g. `"google.login"`.
    pub name: String,
    /// HTTP method, uppercase: `"GET"`, `"POST"`, …
    pub method: String,
    /// Relative path, e.g. `"/oauth/google/login"`. No origin.
    pub path: String,
    /// Human label a UI renders, e.g. `"Sign in with Google"`.
    pub label: String,
}

/// The handle plugins receive in `on_ready`.
///
/// Carries clones of the ambient state so a plugin can spawn background
/// work or seal late registrations without touching globals. M7 v1
/// surfaces the default pool and a settings snapshot; the runtime
/// handle lands when the first plugin needs it (likely `umbral-tasks`
/// at M9).
#[derive(Debug, Clone)]
pub struct AppContext {
    /// The default connection pool, typed by backend. Same value as
    /// `umbral::db::pool_dispatched().clone()` returns. Plugin code
    /// that needs the pool typically goes through the ORM instead
    /// (`Model::objects()…`); this field is the escape hatch for
    /// schema-DDL bootstrap (the documented exception in CLAUDE.md)
    /// and backend-specific features like Postgres RLS.
    pub pool: DbPool,
    /// A clone of the active settings.
    pub settings: Settings,
}

/// Errors a plugin's `on_ready` can return. Boxed under
/// `BuildError::PluginOnReady` so the build phase surfaces them with
/// the plugin name attached.
pub type PluginError = Box<dyn std::error::Error + Send + Sync>;
