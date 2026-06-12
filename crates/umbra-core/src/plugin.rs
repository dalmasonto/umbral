//! The Plugin trait — umbra's only extension mechanism.
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
//! use umbra::prelude::*;
//!
//! pub struct BlogPlugin;
//!
//! impl Plugin for BlogPlugin {
//!     fn name(&self) -> &'static str { "blog" }
//!
//!     fn dependencies(&self) -> &'static [&'static str] { &["auth"] }
//!
//!     fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
//!         vec![umbra::migrate::ModelMeta::for_::<Post>()]
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

/// The contract every umbra extension implements.
///
/// Every method except `name()` has a default that returns the empty
/// contribution. A plugin opts in only to what it contributes: a
/// pure-route plugin overrides `routes()`; a pure-data plugin
/// overrides `models()`; the auth plugin overrides almost all of them.
///
/// The trait is `Send + Sync + 'static` so `App::builder()` can store a
/// homogeneous `Vec<Box<dyn Plugin>>` and the runtime can hand the
/// plugin reference to threads (e.g. for background tasks spawned in
/// `on_ready`). The bounds match Django's `AppConfig` ergonomics: any
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
    fn routes(&self) -> Router {
        Router::new()
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
    /// list bug, not a correctness bug.
    ///
    /// [`routes`]: Plugin::routes
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
    /// [`umbra-openapi`] walks every registered plugin's
    /// contribution at spec-build time and merges them into the
    /// emitted document's `paths` object. Closes BUG-20 from
    /// `bugs/tests/testBugs.md` — auto-generated CRUD routes were
    /// the only thing the spec described before; plugin-
    /// contributed routes (auth, custom actions) were invisible
    /// to Swagger UI.
    ///
    /// Plugins that don't ship OpenAPI documentation leave this
    /// alone. The umbra-openapi plugin's own routes (the
    /// `/openapi.json` and Swagger UI mount) are not in the
    /// generated spec — they're delivery, not API.
    ///
    /// [1]: https://spec.openapis.org/oas/v3.0.3#path-item-object
    /// [`umbra-openapi`]: https://docs.rs/umbra-openapi
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

    /// The database alias every model this plugin contributes should
    /// be read from and written to. Returns `None` to use the
    /// `"default"` pool (the same one `umbra::db::pool()` returns).
    ///
    /// This is umbra's answer to Django's `DATABASE_ROUTERS`. The
    /// builder reads it during phase 3 and the QuerySet's
    /// `resolve_pool` defers to it when no `.on(&pool)` override is
    /// set on the chain. Per-plugin granularity (every model the
    /// plugin owns goes to one database) is the v1 shape; per-model
    /// overrides via attribute lands when a real workload needs it.
    ///
    /// The named alias must have been registered via
    /// `AppBuilder::database(alias, pool)` or
    /// `Settings.databases[alias]` before `App::build()`. A reference
    /// to an unregistered alias surfaces as
    /// `BuildError::PluginDatabaseAlias` at boot.
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
    /// visible. This matches Django's `APP_DIRS` loader semantics.
    ///
    /// Default: no directories. A plugin that renders no HTML leaves
    /// this alone.
    fn templates_dirs(&self) -> Vec<PathBuf> {
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

    /// Static files the plugin ships baked into its binary.
    ///
    /// Each entry produces one `GET <url_path>` route that returns the
    /// file body with the supplied `Content-Type` and `Cache-Control`.
    /// Bodies are `&'static [u8]` — typically `include_bytes!` —
    /// because the canonical use is "the binary ships its own CSS / JS
    /// / fonts."
    ///
    /// Use cases:
    ///   - `umbra-admin` ships its precompiled Tailwind CSS this way.
    ///   - A plugin that adds an HTMX page can ship an icon or font.
    ///   - User code can register arbitrary embedded assets.
    ///
    /// Conflicts across plugins (two plugins claiming the same
    /// `url_path`) surface as the axum `Router::route` panic at
    /// `App::build` time, with the second registrant losing.
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
    /// single mount: a `StaticPlugin` pointed at the configured
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

/// The handle plugins receive in `on_ready`.
///
/// Carries clones of the ambient state so a plugin can spawn background
/// work or seal late registrations without touching globals. M7 v1
/// surfaces the default pool and a settings snapshot; the runtime
/// handle lands when the first plugin needs it (likely `umbra-tasks`
/// at M9).
#[derive(Debug, Clone)]
pub struct AppContext {
    /// The default connection pool, typed by backend. Same value as
    /// `umbra::db::pool_dispatched().clone()` returns. Plugin code
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
