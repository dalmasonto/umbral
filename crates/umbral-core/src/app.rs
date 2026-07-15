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
    /// Management commands the *project* registered directly, via
    /// [`AppBuilder::command`] — the ones that belong to the binary
    /// rather than to any plugin. Handed to
    /// [`crate::cli::dispatch_with_app_commands`] alongside the plugins'
    /// own contributions.
    commands: Vec<Box<dyn crate::cli::PluginCommand>>,
    /// gaps3 #23: when true, `umbral_cli::dispatch` applies pending migrations
    /// before starting the server (the `serve` command only) — so a fresh DB
    /// "just works" WITHOUT running migrate during `makemigrations`/`migrate`
    /// or any other subcommand. Opt in via [`AppBuilder::auto_migrate_on_serve`].
    auto_migrate_on_serve: bool,
    /// Kikosi #5 — how long [`App::serve`] keeps serving after a shutdown signal
    /// before it stops accepting, so a load balancer observes `/readyz` flip to
    /// 503 and drains this instance. `Duration::ZERO` (the default) skips the
    /// drain — the historical behaviour. Set via [`AppBuilder::shutdown_drain`].
    drain_delay: std::time::Duration,
    /// Set the first time [`App::ready`] fires the `on_ready` hooks, so the
    /// second call is a no-op. `serve()` and `into_router()` both call it, and
    /// `umbral_cli::dispatch` may have called it already.
    ready_fired: std::sync::atomic::AtomicBool,
}

impl App {
    /// Whether the app opted into auto-migrate on `serve` (gaps3 #23). Read by
    /// `umbral_cli`'s serve path; see [`AppBuilder::auto_migrate_on_serve`].
    pub fn auto_migrate_on_serve_enabled(&self) -> bool {
        self.auto_migrate_on_serve
    }

    /// Fire every plugin's [`Plugin::on_ready`] hook, in topological order.
    /// Idempotent: the second and later calls do nothing.
    ///
    /// # Why this is not part of `build()`
    ///
    /// `on_ready` means *the application is up*. Plugins use it to seed content,
    /// backfill rows, install RLS policies, and (in `umbral-permissions`) create
    /// the standard permission rows for every registered model. All of that
    /// needs a migrated schema.
    ///
    /// [`AppBuilder::build`] still calls this for you, so a test or an embedder
    /// that holds an `App` sees no change. What changed is `umbral_cli::dispatch`:
    /// it takes the *builder*, calls [`AppBuilder::build_deferred`], resolves
    /// argv, and only then calls `ready()` — skipping it entirely for the schema
    /// commands (`migrate`, `makemigrations`, `inspectdb`, …).
    ///
    /// Before that, the generated `main.rs` was
    /// `let app = App::builder()…build()?; umbral_cli::dispatch(app)`, so the
    /// hooks ran before `dispatch` had even parsed argv — including when argv
    /// said `migrate`. Against a fresh database that produced a wall of
    /// `relation "…" does not exist` before the migration engine had created a
    /// single table (gaps3 #41, seen on the first umbralrs.dev deploy). Nothing
    /// crashed only because those seeds log-and-swallow; a plugin that propagated
    /// the error made `migrate` unrunnable, and one that wrote rows silently
    /// skipped the write.
    ///
    /// [`App::serve`] calls this too, so a hand-rolled `main` that builds with
    /// `build_deferred()` and serves directly still gets its hooks.
    /// Whether [`App::ready`] has already run the `on_ready` hooks.
    ///
    /// `umbral_cli::dispatch` reads this to warn when a binary still builds with
    /// `App::build()` — the hooks fired before argv was parsed, so a schema
    /// command has already run every plugin's seed (gaps3 #41).
    pub fn ready_already_fired(&self) -> bool {
        self.ready_fired.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn ready(&self) -> Result<(), BuildError> {
        use std::sync::atomic::Ordering;
        if self.ready_fired.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let ctx = crate::plugin::AppContext {
            pool: crate::db::pool_dispatched().clone(),
            settings: crate::settings::get().clone(),
        };
        for plugin in &self.plugins {
            plugin
                .on_ready(&ctx)
                .map_err(|source| BuildError::PluginOnReady {
                    plugin: plugin.name(),
                    source,
                })?;
        }
        Ok(())
    }

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
    /// Fires [`App::ready`] first (idempotent, so `umbral_cli::dispatch` having
    /// already called it is fine): a server that is about to accept requests is
    /// by definition ready, and a plugin's `on_ready` may install the ambient
    /// state its handlers read. A hook that fails surfaces as an
    /// [`std::io::ErrorKind::Other`] carrying the `BuildError`'s message —
    /// `serve` has always returned `io::Error`, and a plugin that can't start is
    /// as fatal as a port that won't bind.
    ///
    /// This call blocks until the server stops. At M0 there is no graceful
    /// shutdown hook; that lands with the signal-handling work in a later
    /// milestone.
    pub async fn serve(self, addr: impl Into<SocketAddr>) -> Result<(), std::io::Error> {
        self.ready().map_err(std::io::Error::other)?;

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
        // audit_2 core-app-config #13: graceful shutdown. Without it, a deploy
        // (SIGTERM) drops every in-flight request and never drains the pools —
        // Postgres logs abrupt terminations, SQLite skips its WAL checkpoint.
        // `with_graceful_shutdown` stops accepting new connections on the
        // signal and waits for in-flight requests to finish; then we close the
        // pools so connections shut down cleanly.
        // Kikosi #5: when a drain delay is configured, the shutdown future flips
        // readiness to draining and holds for the delay BEFORE resolving, so the
        // server keeps accepting during the window the load balancer needs to
        // notice `/readyz` = 503 and stop routing here. With ZERO delay this is
        // the plain signal wait — the historical behaviour.
        let drain_delay = self.drain_delay;
        axum::serve(listener, self.router.into_make_service())
            .with_graceful_shutdown(drain_after(shutdown_signal(), drain_delay))
            .await?;
        tracing::info!("umbral: server stopped accepting; draining DB pools");
        crate::db::close().await;
        Ok(())
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

    /// Borrow the project's own commands — the ones registered directly on
    /// the builder via [`AppBuilder::command`] rather than contributed by a
    /// plugin.
    ///
    /// Mirrors [`App::plugins`]: borrowed, not moved, so the App stays
    /// usable after [`crate::cli::dispatch_with_app_commands`] returns
    /// `Unmatched` and the caller falls through to its built-ins.
    pub fn commands(&self) -> &[Box<dyn crate::cli::PluginCommand>] {
        &self.commands
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
    /// gaps3 #46 — collect link-registered models at build time.
    auto_models: bool,
    plugins: Vec<Box<dyn Plugin>>,
    /// Project-owned management commands, added via [`AppBuilder::command`].
    /// Kept out of the plugin list on purpose: a command the binary owns
    /// isn't a reusable unit, and wrapping it in a dummy plugin to reach
    /// argv would be a workaround for a missing contract, not a design.
    commands: Vec<Box<dyn crate::cli::PluginCommand>>,
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
    /// gaps3 #23: apply pending migrations on `serve` (opt-in).
    auto_migrate_on_serve: bool,
    /// Kikosi #5: shutdown drain delay. `Duration::ZERO` = no drain.
    drain_delay: std::time::Duration,
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
    /// Framework-wide request-body size cap (bytes). `build()` installs a
    /// `tower-http` `RequestBodyLimitLayer` so any body over the cap is
    /// rejected with `413` before a handler buffers it (audit_2 core-web H11).
    /// Defaults to 32 MiB; `None` disables the global limit. Set via
    /// [`AppBuilder::max_request_body`].
    max_request_body_bytes: Option<usize>,
    /// Per-request timeout. `build()` installs a `tower-http` `TimeoutLayer`
    /// so a hung/slowloris request is aborted with `408` instead of pinning a
    /// task forever (audit_2 core-web H11/#3). Defaults to 30s; `None`
    /// disables. Set via [`AppBuilder::request_timeout`].
    request_timeout: Option<std::time::Duration>,
    /// Ship minimal hardening response headers from core (audit_2 H10):
    /// `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
    /// `Referrer-Policy: strict-origin-when-cross-origin` — set ONLY if not
    /// already present, so `SecurityPlugin` (which owns the configurable values
    /// + CSRF + HSTS) wins when mounted. Default `true`; opt out via
    /// [`AppBuilder::default_security_headers`].
    default_security_headers: bool,
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
    /// When `true`, `build()` FAILS (not just warns) if any app-level mutating
    /// route (POST/PUT/PATCH/DELETE) registered via `.routes(...)` carries no
    /// recorded permission (gaps3 #28 P1 — enforces the audit_2 H19 audit).
    /// Opt-in "gated by construction": a forgotten authorization gate becomes a
    /// boot error instead of a silently-open endpoint. Default `false` (warn
    /// only). Set via [`AppBuilder::deny_ungated_mutations`].
    deny_ungated_mutations: bool,
}

impl Default for AppBuilder {
    fn default() -> Self {
        Self {
            settings: None,
            databases: HashMap::new(),
            router: None,
            route_paths: Vec::new(),
            models: Vec::new(),
            auto_models: false,
            plugins: Vec::new(),
            commands: Vec::new(),
            templates_dir: None,
            slash_redirect: crate::slash::SlashRedirect::default(),
            not_found_template: None,
            server_error_template: None,
            error_templates: HashMap::new(),
            server_error_hook: None,
            default_error_pages: true,
            auto_migrate_on_serve: false,
            drain_delay: std::time::Duration::ZERO,
            cors: None,
            cors_scoped: Vec::new(),
            atomic_transactions: None,
            deny_ungated_mutations: false,
            compress: false,
            // Safe-by-default request hardening (audit_2 core-web H11): a 32
            // MiB body ceiling and a 30s timeout, both opt-out-able.
            max_request_body_bytes: Some(32 * 1024 * 1024),
            request_timeout: Some(std::time::Duration::from_secs(30)),
            default_security_headers: true,
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

    /// Register every `#[derive(Model)]` type the binary links, instead of naming
    /// each one (gaps3 #46).
    ///
    /// ```ignore
    /// App::builder()
    ///     .settings(settings)
    ///     .database("default", pool)
    ///     .auto_models()          // replaces .model::<Post>().model::<Tag>()....
    ///     .plugin(RestPlugin::default())
    ///     .build()?
    /// ```
    ///
    /// Explicit `.model::<T>()` still works and composes with this — the two are
    /// merged and de-duplicated by table, so adding it to an existing app is safe.
    ///
    /// # Why this is opt-in
    ///
    /// Discovery is link-time. A model in your **binary** crate is always linked
    /// and always found. A model in a **library** crate that nothing else
    /// references can be dropped by the linker, and it would then be missing from
    /// the registry — which means missing from `makemigrations`, i.e. a table that
    /// silently never gets created. Making this the default would trade a little
    /// typing for a failure mode that is invisible until production.
    ///
    /// If your models live in a library crate, either `use` it from `main.rs` (so
    /// the linker keeps it) or keep naming them with `.model::<T>()`.
    pub fn auto_models(mut self) -> Self {
        self.auto_models = true;
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

    /// Register one project-owned management command.
    ///
    /// The command shows up in `cargo run -- <name>`, in `umbral help`, and
    /// under `umbral <name> --help`, exactly like a plugin's. The difference
    /// is ownership: this one belongs to the binary, so there is no plugin
    /// to wrap it in and nothing to publish.
    ///
    /// ```ignore
    /// use umbral::cli::{CliError, PluginCommand, clap};
    ///
    /// struct BackfillSlugs;
    ///
    /// #[umbral::async_trait]
    /// impl PluginCommand for BackfillSlugs {
    ///     fn command(&self) -> clap::Command {
    ///         clap::Command::new("backfill_slugs").about("Fill empty post slugs")
    ///     }
    ///     async fn run(&self, _m: &clap::ArgMatches) -> Result<(), CliError> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// App::builder().command(BackfillSlugs)
    /// ```
    ///
    /// `umbral startcommand` writes that file for you and wires this call.
    ///
    /// On a name clash with a plugin's command, the app's wins — the
    /// project is the most specific layer — and the losing plugin is named
    /// in a warning.
    ///
    /// A framework built-in (`migrate`, `serve`, …) cannot be overridden: the
    /// dispatcher drops any registered command that lands on one of those
    /// names and prints a warning telling you to rename it. That is enforced
    /// in `cli::collect_commands`, not merely checked by `startcommand` —
    /// a command named `migrate` would otherwise quietly take over, and the
    /// next deploy would apply zero migrations and exit 0.
    pub fn command(mut self, command: impl crate::cli::PluginCommand) -> Self {
        self.commands.push(Box::new(command));
        self
    }

    /// Register a whole list of project-owned commands at once — what the
    /// generated `src/commands/mod.rs` hands back from its `all()` registry.
    ///
    /// ```ignore
    /// App::builder().commands(commands::all())
    /// ```
    ///
    /// That indirection is what makes `startcommand` idempotent: adding a
    /// second command appends to `all()` and never touches `main.rs` again.
    /// Rust has no way to discover a module by scanning a directory at
    /// runtime, so `all()` *is* the auto-detection — a registry the tool
    /// maintains for you.
    pub fn commands(mut self, commands: Vec<Box<dyn crate::cli::PluginCommand>>) -> Self {
        self.commands.extend(commands);
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

    /// Set the trailing-slash redirect policy. See
    /// [`crate::slash::SlashRedirect`].
    ///
    /// Default is `Off` (axum's strict matching). Most apps want
    /// `Append` (`/foo` 404 → 308 → `/foo/`) so that
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

    /// Set the template rendered on a 404. Follows the
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

    /// Set the template rendered on a panicking handler. Follows
    /// the `500.html` convention.
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
    /// gaps3 #23: apply pending migrations automatically when the app is
    /// STARTED (`umbral_cli::dispatch` → the `serve` command), and NEVER during
    /// `makemigrations` / `migrate` / any other subcommand. This replaces the
    /// argv-sniffing guard consumers hand-rolled in `main.rs` to avoid
    /// auto-migrating during CLI commands:
    ///
    /// ```ignore
    /// let app = App::builder().auto_migrate_on_serve().plugin(...).build()?;
    /// umbral_cli::dispatch(app).await   // migrate runs iff this serves
    /// ```
    ///
    /// A convenience for demos / small apps; a large deploy still runs
    /// `migrate` as an explicit release step. Seeding stays app-owned (a
    /// plugin's `on_ready` or an explicit call).
    pub fn auto_migrate_on_serve(mut self) -> Self {
        self.auto_migrate_on_serve = true;
        self
    }

    /// Drain for `delay` on shutdown before the server stops accepting
    /// connections (Kikosi #5 — the zero-downtime rollout piece).
    ///
    /// On `SIGTERM` / Ctrl-C, [`App::serve`] marks the process draining
    /// ([`crate::shutdown::is_draining`]) so `umbral-health`'s `/readyz` returns
    /// 503 at once, keeps serving for `delay`, and only then lets the graceful
    /// shutdown proceed (stop accepting, finish in-flight, close pools). The
    /// delay is the window in which a load balancer polls `/readyz`, sees the
    /// 503, and pulls this instance out of rotation — so the requests it *would*
    /// have routed here go elsewhere instead of hitting a socket that is about
    /// to close.
    ///
    /// Pick a delay a little longer than your LB's readiness probe interval
    /// (k8s default 10s; a `HEALTHCHECK --interval=10s` the same). `5`–`15s` is
    /// typical. `Duration::ZERO` (the default) skips the drain entirely — right
    /// for a single-instance app or local dev, where there is no LB to notify
    /// and an instant Ctrl-C is what you want.
    ///
    /// Only meaningful alongside a readiness probe (mount `HealthPlugin`); with
    /// no `/readyz` for the LB to poll, the delay is dead time on shutdown.
    pub fn shutdown_drain(mut self, delay: std::time::Duration) -> Self {
        self.drain_delay = delay;
        self
    }

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

    /// Make a forgotten authorization gate a **boot error** instead of a
    /// warning (gaps3 #28 P1). With this set, `build()` fails with
    /// [`BuildError::UngatedMutatingRoutes`] if any app-level mutating route
    /// (POST/PUT/PATCH/DELETE) registered via [`Self::routes`] carries no
    /// recorded permission — i.e. it wasn't gated through the umbral-permissions
    /// `Routes::*_gated(...)` builders (a hand-applied
    /// `.layer(permission_required(...))` is opaque to the audit, so prefer the
    /// builder). This is the opt-in "gated by construction" posture: authorization
    /// on every mutating route is enforced at boot rather than trusted to review.
    ///
    /// Default off — `build()` only *warns*. An intentionally-public mutating
    /// route (a webhook receiver, a health `POST`) must be registered through a
    /// permission-aware builder anyway (or kept out of `.routes(...)`) once this
    /// is on, so the decision is explicit.
    pub fn deny_ungated_mutations(mut self) -> Self {
        self.deny_ungated_mutations = true;
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

    /// Set (or disable) the framework-wide request-body size cap.
    ///
    /// `build()` installs a `tower-http` `RequestBodyLimitLayer` with this
    /// ceiling, so any request whose body exceeds it is rejected with `413
    /// Payload Too Large` before a handler (or the multipart parser) buffers
    /// it — the memory-exhaustion backstop axum's per-extractor default does
    /// NOT give streaming/multipart consumers (audit_2 core-web H11).
    ///
    /// Defaults to **32 MiB**. Pass `Some(bytes)` to raise/lower it, or `None`
    /// to remove the global limit entirely (appropriate when a reverse proxy
    /// already caps body size).
    ///
    /// ```ignore
    /// App::builder()
    ///     .max_request_body(Some(8 * 1024 * 1024)) // 8 MiB
    ///     .build().await?;
    /// ```
    pub fn max_request_body(mut self, limit: Option<usize>) -> Self {
        self.max_request_body_bytes = limit;
        self
    }

    /// Set (or disable) the default per-request timeout.
    ///
    /// `build()` installs a `tower-http` `TimeoutLayer` so a request that runs
    /// longer than this is aborted with `408 Request Timeout`, freeing the
    /// task/connection instead of letting a hung handler or slowloris client
    /// pin it indefinitely (audit_2 core-web H11/#3).
    ///
    /// Defaults to **30 seconds**. Pass `Some(duration)` to change it, or
    /// `None` to disable — do that for legitimately long-lived streaming/SSE
    /// routes, or when a proxy owns request timeouts.
    ///
    /// ```ignore
    /// use std::time::Duration;
    /// App::builder()
    ///     .request_timeout(Some(Duration::from_secs(10)))
    ///     .build().await?;
    /// ```
    pub fn request_timeout(mut self, timeout: Option<std::time::Duration>) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Toggle the core-shipped hardening response headers (audit_2 H10):
    /// `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, and
    /// `Referrer-Policy: strict-origin-when-cross-origin`. On by default and
    /// applied only when the header isn't already set, so `SecurityPlugin`'s
    /// configured values win. Pass `false` to fully own response headers
    /// yourself (e.g. an API behind a gateway that adds them at the edge).
    pub fn default_security_headers(mut self, enabled: bool) -> Self {
        self.default_security_headers = enabled;
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
    ///    `BuildError::PluginOnReady`. Phases 1-6 are
    ///    [`AppBuilder::build_deferred`]; this last phase is [`App::ready`],
    ///    and a CLI binary lets `umbral_cli::dispatch` decide when it fires
    ///    (gaps3 #41).
    ///
    /// `build()` is intentionally sync. Earlier iterations auto-opened
    /// the default pool from `settings.database_url` by spinning up a
    /// throwaway tokio runtime to drive `db::connect`. That panicked
    /// when called from inside any caller that was already in a tokio
    /// runtime ("Cannot start a runtime from within a runtime"), which
    /// is every realistic case. Requiring an explicit `.database(...)`
    /// is both spec-correct and avoids the trap.
    pub fn build(self) -> Result<App, BuildError> {
        let app = self.build_deferred()?;
        app.ready()?;
        Ok(app)
    }

    /// Everything [`AppBuilder::build`] does *except* firing `on_ready`.
    ///
    /// The app is fully wired — pools open, registry published, router merged,
    /// system checks passed — but no plugin has been told the app is up. Call
    /// [`App::ready`] when it actually is.
    ///
    /// This exists for `umbral_cli::dispatch`, which has to build the app in
    /// order to read its plugins' `commands()` and *then* decide what argv asked
    /// for. A schema command (`migrate`, `makemigrations`, `inspectdb`) must not
    /// fire hooks that seed content into a schema that doesn't exist yet
    /// (gaps3 #41). Reach for it directly only if you are writing your own
    /// dispatcher; otherwise `build()` is the one you want.
    pub fn build_deferred(mut self) -> Result<App, BuildError> {
        // Phase 1 — collect
        let settings = self.settings.take().ok_or(BuildError::SettingsMissing)?;

        if !self.databases.contains_key("default") {
            return Err(BuildError::DefaultPoolMissing);
        }

        // Phase 1.4 — audit_2 H17: open the pools declared in `settings.databases`.
        // Each `[databases] <alias> = "<url>"` entry that a builder `.database()`
        // call didn't already register is opened LAZILY (sync; connects on first
        // use) and added to the pool set, so a model/router routed to that alias
        // resolves instead of panicking at query time — and the documented
        // `settings.databases` config actually does something. A builder-registered
        // alias wins (an explicitly-built pool overrides the settings URL).
        for (alias, url) in &settings.databases {
            if self.databases.contains_key(alias) {
                continue;
            }
            let pool =
                crate::db::connect_lazy(url).map_err(|error| BuildError::SettingsDatabasePool {
                    alias: alias.clone(),
                    error,
                })?;
            self.databases.insert(alias.clone(), pool);
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

        // (audit_2 H17: `settings.databases` pools were opened lazily in Phase 1.4
        // above, so every declared alias is now a registered pool — the earlier
        // "not auto-opened" boot warning is gone.)

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
        // gaps3 #46: pull in every model the binary link-registered. Merged with
        // (not instead of) the explicit `.model::<T>()` list, and de-duplicated by
        // table, so the two compose and adding `auto_models()` to an existing app
        // can't double-register anything.
        if self.auto_models {
            let known: std::collections::HashSet<String> = self
                .models
                .iter()
                .map(|m| m.table.clone())
                .chain(
                    sorted_plugins
                        .iter()
                        .flat_map(|p| p.models())
                        .map(|m| m.table),
                )
                .collect();
            for meta in crate::migrate::link_registered_models() {
                if !known.contains(&meta.table) {
                    self.models.push(meta);
                }
            }
        }

        // gaps3 #54: an `#[umbral(audited)]` model implies the audit table. Register
        // it automatically so `makemigrations` creates it through the normal
        // declare→migrate loop — no special-cased DDL, and no ceremony for the app.
        let any_audited = sorted_plugins
            .iter()
            .flat_map(|p| p.models())
            .chain(self.models.iter().cloned())
            .any(|m| m.audited);
        if any_audited
            && !self
                .models
                .iter()
                .any(|m| m.table == crate::orm::audit::AUDIT_TABLE)
        {
            self.models.push(crate::orm::audit::audit_meta());
        }

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
        // walks plugins in dependency order. The implicit "app" plugin
        // (owner of `.model::<T>()` registrations) lands LAST: app models
        // typically hold ForeignKeys INTO plugin-owned tables (e.g.
        // `Post.author -> auth_user`), so those tables must be created
        // first. Postgres enforces FK targets at CREATE TABLE, so ordering
        // "app" first made app-model migrations fail there with
        // `relation "auth_user" does not exist` (SQLite silently allowed
        // the dangling FK, hiding the bug in local dev).
        let mut order: Vec<String> = Vec::with_capacity(sorted_plugins.len() + 1);
        for plugin in &sorted_plugins {
            order.push(plugin.name().to_string());
        }
        order.push(crate::migrate::APP_PLUGIN_NAME.to_string());
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

        // audit_2 H19: surface at boot the app's own mutating routes
        // (POST/PUT/PATCH/DELETE) that carry no RECORDED permission, so a
        // forgotten authorization gate surfaces here instead of as a silently
        // open endpoint. Only `.routes(...)` (the app's hand-written routes)
        // are audited — plugin routes gate via their own conventions and are
        // merged separately. Runs before `self.route_paths` is moved below.
        // With `.deny_ungated_mutations()` (gaps3 #28 P1) the same finding is a
        // hard `BuildError` instead of a warning: authorization on every
        // mutating route is enforced by construction.
        let ungated = ungated_mutating_routes(&self.route_paths);
        if !ungated.is_empty() {
            if self.deny_ungated_mutations {
                return Err(BuildError::UngatedMutatingRoutes { routes: ungated });
            }
            warn_ungated_mutating_routes(&ungated);
        }

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
        // gaps4 #31: when a plugin implements the recording `routes_builder()`,
        // its router AND its declared specs come from that ONE source, so the
        // registry cannot drift from what's mounted. Call it once here, record
        // the specs now, and stash the router for the merge loop below (so a
        // legacy plugin's `routes()` side-effects still fire at their original
        // point, and a builder plugin's router is never rebuilt twice).
        let mut builder_routers: HashMap<String, Router> = HashMap::new();
        for plugin in &sorted_plugins {
            let specs = match plugin.routes_builder() {
                Some(builder) => {
                    let (router, specs) = builder.into_parts();
                    builder_routers.insert(plugin.name().to_string(), router);
                    specs
                }
                None => plugin.route_paths(),
            };
            route_registry
                .by_plugin
                .insert(plugin.name().to_string(), specs);
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
        // Search order (first-match-wins across all template directories):
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
            // gaps4 #31: a plugin that provided a `routes_builder()` had its
            // router built above (drift-free with its specs); reuse it. Everyone
            // else mounts through the legacy `routes()`, unchanged.
            let plugin_router = builder_routers
                .remove(plugin.name())
                .unwrap_or_else(|| plugin.routes());
            router = router.merge(plugin_router);
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

        // Phase 5.95 — Host-header validation (allowed-hosts allowlist). Applied
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

        // Request hardening (audit_2 core-web H11) — a framework-wide body-size
        // cap and a per-request timeout, both safe-by-default and opt-out-able
        // via `AppBuilder::max_request_body` / `request_timeout`. Layered
        // outermost (just under the trace span) so they bound EVERY request —
        // including host-guard rejections — before an inner extractor or the
        // multipart parser can buffer an oversized body or a hung handler can
        // pin a task. `RequestBodyLimitLayer` returns 413; `TimeoutLayer`
        // returns 408.
        if let Some(limit) = self.max_request_body_bytes {
            router = router.layer(tower_http::limit::RequestBodyLimitLayer::new(limit));
        }
        if let Some(timeout) = self.request_timeout {
            router = router.layer(tower_http::timeout::TimeoutLayer::with_status_code(
                axum::http::StatusCode::REQUEST_TIMEOUT,
                timeout,
            ));
        }

        // audit_2 H10 — minimal hardening response headers from core, so a
        // default app that forgot SecurityPlugin is still not clickjackable /
        // MIME-sniffable. Set ONLY if absent (SecurityPlugin's configured
        // values win), and applied outer to the host/limit/timeout layers so
        // their 4xx responses carry them too. HSTS is deliberately NOT set here
        // — it's sticky and subdomain-scoped, so it stays SecurityPlugin's
        // configurable responsibility.
        if self.default_security_headers {
            router = router.layer(axum::middleware::from_fn(default_security_headers_layer));
        }

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

        // Phase 6 — `on_ready` USED to fire here. It doesn't any more: the hooks
        // seed content and backfill rows, and `build()` runs before the CLI has
        // parsed argv, so `migrate` against a fresh database ran every seed
        // before a single table existed (gaps3 #41). The caller now decides when
        // the app is ready; see [`App::ready`], which `serve()` and
        // `umbral_cli::dispatch` call at the right moment.
        Ok(App {
            router,
            plugins: sorted_plugins,
            commands: self.commands,
            auto_migrate_on_serve: self.auto_migrate_on_serve,
            drain_delay: self.drain_delay,
            ready_fired: std::sync::atomic::AtomicBool::new(false),
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

/// Resolve when the process receives a shutdown signal — `SIGTERM` (the deploy
/// / container-stop signal) or `SIGINT` (Ctrl-C). Drives `serve`'s graceful
/// shutdown (audit_2 core-app-config #13). On non-Unix only Ctrl-C is wired.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            // If the handler can't be installed, never fire this arm.
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("umbral: shutdown signal received; finishing in-flight requests");
}

/// The future `serve` hands to axum's `with_graceful_shutdown` (Kikosi #5).
///
/// Awaits `signal`, marks the process draining so readiness probes go 503, then
/// — if `drain_delay` is non-zero — keeps serving for that long before resolving,
/// which is when axum stops accepting new connections. The delay is the window a
/// load balancer uses to observe the 503 and route new traffic elsewhere.
///
/// Generic over the signal future so the drain sequencing is testable without
/// actually delivering a `SIGTERM`.
async fn drain_after<F: std::future::Future>(signal: F, drain_delay: std::time::Duration) {
    signal.await;
    crate::shutdown::begin_drain();
    if !drain_delay.is_zero() {
        tracing::info!(
            drain_secs = drain_delay.as_secs_f64(),
            "umbral: draining — readiness now reports 503; holding before stop"
        );
        tokio::time::sleep(drain_delay).await;
    }
}

/// audit_2 H19 — warn about the app's own mutating routes that carry no
/// recorded permission. A default-DENY router is a future-major change; this
/// boot Warning is the non-breaking default: it makes a forgotten
/// authorization gate visible at boot instead of shipping as an open endpoint.
/// [`AppBuilder::deny_ungated_mutations`] promotes the same finding to a hard
/// [`BuildError::UngatedMutatingRoutes`] for apps that want it enforced.
///
/// Scope + honesty: only routes registered through `Routes` (the app's
/// `.routes(...)`) are checked — plugin routes gate via their own conventions.
/// A route gated by a hand-applied `.layer(permission_required(...))` is opaque
/// to `RouteSpec`, so it can't be distinguished from an ungated one; the
/// warning says so and points at the `require_permission(...)` builder (which
/// records the permission). An intentionally-public route is a false positive
/// the operator ignores.
fn warn_ungated_mutating_routes(ungated: &[String]) {
    tracing::warn!(
        "audit_2 H19: {} app mutating route(s) have no recorded permission: [{}]. \
         Gate them with the umbral-permissions `Routes::require_permission(...)` builder \
         so the framework records the permission (a hand-applied \
         `.layer(permission_required(...))` is NOT visible to this audit — prefer the \
         builder). If a route is intentionally public, ignore this. To make this a hard \
         boot error instead, call `App::builder().deny_ungated_mutations()`.",
        ungated.len(),
        ungated.join(", ")
    );
}

/// The pure core of the H19 audit: the `"METHOD /path"`
/// labels of every route with a mutating method and no recorded permission.
/// Split out so the audit's selection logic is unit-testable without a live
/// `App::build()` / tracing subscriber.
fn ungated_mutating_routes(specs: &[crate::routes::RouteSpec]) -> Vec<String> {
    const MUTATING: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];
    specs
        .iter()
        .filter(|s| s.permission.is_none() && s.methods.iter().any(|m| MUTATING.contains(m)))
        .map(|s| format!("{} {}", s.methods.join("/"), s.path))
        .collect()
}

/// Set minimal hardening response headers, each ONLY if the response doesn't
/// already carry it — so `SecurityPlugin` (or a handler) can override, and no
/// header is ever duplicated (audit_2 H10).
async fn default_security_headers_layer(
    req: crate::web::Request,
    next: axum::middleware::Next,
) -> crate::web::Response {
    use axum::http::HeaderValue;
    use axum::http::header::{
        HeaderName, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
    };

    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    let mut set_if_absent = |name: HeaderName, value: &'static str| {
        if !headers.contains_key(&name) {
            headers.insert(name, HeaderValue::from_static(value));
        }
    };
    set_if_absent(X_CONTENT_TYPE_OPTIONS, "nosniff");
    set_if_absent(X_FRAME_OPTIONS, "DENY");
    set_if_absent(REFERRER_POLICY, "strict-origin-when-cross-origin");
    resp
}

/// One cross-plugin foreign-key edge derived from the model registry.
///
/// `plugin`'s table `table` carries a physical `REFERENCES "<fk_target>"`, and
/// `fk_target` is owned by `depends_on`. Reported on
/// [`BuildError::ForeignKeyCycle`] so the operator sees the *column* that
/// forced the ordering, not just two plugin names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FkEdge {
    /// The plugin whose table holds the foreign key.
    pub plugin: &'static str,
    /// The plugin that owns the referenced table, and so must migrate first.
    pub depends_on: &'static str,
    /// The referencing table (`Model::TABLE` of the model holding the FK).
    pub table: String,
    /// The referenced table (`fk_target` of the FK column).
    pub fk_target: String,
}

/// Derive the cross-plugin ordering edges the *schema already states*.
///
/// `Plugin::dependencies()` is the edge set an author declares by hand. This is
/// the edge set the models spell out: a `ForeignKey<T>` field renders
/// `REFERENCES "<T::TABLE>"` inside `CREATE TABLE`, so the plugin owning
/// `T::TABLE` must create it first. Postgres enforces the target's existence at
/// `CREATE TABLE` time; SQLite silently accepts the dangling reference, which is
/// why omitting the edge only ever failed on a fresh Postgres (gaps3 #40).
///
/// Two exclusions, both deliberate:
///
/// - **Same-plugin FKs** impose no *plugin* ordering. Column order inside one
///   plugin's migration is the diff engine's problem, not the sort's.
/// - **`#[umbral(db_constraint = false)]`** renders no `REFERENCES` clause (the
///   only valid shape for a cross-database FK, gaps2 #22), so it creates no DDL
///   ordering obligation.
///
/// An FK whose target table belongs to no registered plugin — an app-owned model
/// from `.model::<T>()` — yields no edge: the implicit `"app"` plugin is pinned
/// last by `App::build()` precisely because app models FK *into* plugin tables.
fn fk_plugin_edges(plugins: &[Box<dyn Plugin>]) -> Vec<FkEdge> {
    use std::collections::BTreeMap;

    // table -> owning plugin. Built across every registered plugin first so a
    // forward reference (a plugin FK-ing a table owned by a plugin registered
    // later) still resolves.
    let mut owner_of_table: BTreeMap<String, &'static str> = BTreeMap::new();
    for plugin in plugins {
        for model in plugin.models() {
            owner_of_table.insert(model.table, plugin.name());
        }
    }

    let mut edges: Vec<FkEdge> = Vec::new();
    for plugin in plugins {
        let name = plugin.name();
        for model in plugin.models() {
            for column in &model.fields {
                let Some(target) = column.fk_target.as_deref() else {
                    continue;
                };
                if !column.db_constraint {
                    continue;
                }
                let Some(&owner) = owner_of_table.get(target) else {
                    continue;
                };
                if owner == name {
                    continue;
                }
                edges.push(FkEdge {
                    plugin: name,
                    depends_on: owner,
                    table: model.table.clone(),
                    fk_target: target.to_string(),
                });
            }
        }
    }
    edges
}

/// Kahn's algorithm over `deps` (plugin -> the set it waits on), with a
/// name-sorted ready queue so ties resolve deterministically. Returns the
/// topological order, or the still-unsorted names when the graph has a cycle.
fn toposort(
    mut remaining_deps: std::collections::BTreeMap<
        &'static str,
        std::collections::BTreeSet<&'static str>,
    >,
) -> Result<Vec<&'static str>, Vec<&'static str>> {
    use std::collections::BTreeSet;

    let mut ready: BTreeSet<&'static str> = remaining_deps
        .iter()
        .filter_map(|(name, deps)| if deps.is_empty() { Some(*name) } else { None })
        .collect();

    let mut order: Vec<&'static str> = Vec::with_capacity(remaining_deps.len());
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

    if remaining_deps.is_empty() {
        Ok(order)
    } else {
        Err(remaining_deps.keys().copied().collect())
    }
}

/// Validate the registered plugins and return them in a stable
/// topological order. Standard Kahn's algorithm with a name-sorted ready queue
/// so ties resolve deterministically.
///
/// The edge set is the union of two sources:
///
/// 1. `Plugin::dependencies()` — what the author declared.
/// 2. Cross-plugin foreign keys read off the models ([`fk_plugin_edges`]) — what
///    the schema already states.
///
/// Before gaps3 #40 only (1) fed the sort, so an app where *no* plugin declared
/// a dependency had every plugin at in-degree 0 and the "topological" order
/// collapsed to alphabetical. `"accounts"` sorts before `"auth"`, and its
/// `CREATE TABLE ... REFERENCES "auth_user"` ran against a database with no
/// `auth_user`. Declaring your own dependencies is still the plugin author's job;
/// the framework just no longer lets the omission reach production silently.
///
/// Rejects:
///
/// - A plugin claiming the reserved `"app"` name.
/// - Two plugins reporting the same `name()`.
/// - A `dependencies()` entry that doesn't name a registered plugin.
/// - A declared dependency cycle (`BuildError::PluginCycle`).
/// - A cycle introduced by the foreign keys themselves
///   (`BuildError::ForeignKeyCycle`, which names the offending columns).
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

    let declared: BTreeMap<&'static str, BTreeSet<&'static str>> = plugins
        .iter()
        .map(|p| (p.name(), p.dependencies().iter().copied().collect()))
        .collect();

    let fk_edges = fk_plugin_edges(&plugins);
    let mut combined = declared.clone();
    for edge in &fk_edges {
        combined
            .get_mut(edge.plugin)
            .expect("every FK edge names a registered plugin")
            .insert(edge.depends_on);
    }

    let order = match toposort(combined) {
        Ok(order) => order,
        Err(stuck) => {
            // The combined graph cycles. Re-run on the declared edges alone to
            // find out who is to blame. If the declared graph is acyclic, the
            // foreign keys introduced the cycle — report the columns that did
            // it rather than a bare `PluginCycle` the author never wrote.
            //
            // Across crates this is unreachable: `ForeignKey<T>` needs `T` in
            // scope, so mutually-referencing plugin crates would be a circular
            // Cargo dependency. Two plugins defined in ONE crate can still do
            // it, and a cross-plugin FK cycle has no valid `CREATE TABLE` order
            // on a fresh database either way.
            if toposort(declared).is_ok() {
                let stuck: BTreeSet<&'static str> = stuck.into_iter().collect();
                let edges: Vec<FkEdge> = fk_edges
                    .into_iter()
                    .filter(|e| stuck.contains(e.plugin) && stuck.contains(e.depends_on))
                    .collect();
                return Err(BuildError::ForeignKeyCycle { edges });
            }
            return Err(BuildError::PluginCycle { names: stuck });
        }
    };

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
    /// The plugins' *foreign keys* form a cycle, so no `CREATE TABLE` order
    /// satisfies every `REFERENCES` clause on a fresh database. Distinct from
    /// [`BuildError::PluginCycle`], which reports a cycle the author declared
    /// via `dependencies()`; here nothing was declared and the cycle is implied
    /// by the models. Carries the FK edges that close the loop so the message
    /// can name the columns, not just the plugins.
    ForeignKeyCycle { edges: Vec<FkEdge> },
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
    /// A `settings.databases` entry could not be opened as a lazy pool at boot
    /// (audit_2 H17) — e.g. an unsupported URL scheme. Carries the alias and the
    /// sqlx error.
    SettingsDatabasePool { alias: String, error: sqlx::Error },
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
    /// `.deny_ungated_mutations()` was set and one or more app-level mutating
    /// routes (POST/PUT/PATCH/DELETE registered via `.routes(...)`) carry no
    /// recorded permission (gaps3 #28 P1, enforcing the audit_2 H19 audit).
    /// Carries the `"METHOD /path"` label of each offending route. Fix by gating
    /// them with the umbral-permissions `Routes::require_permission(...)` builder
    /// (which records the permission), or drop the strict flag if a route is
    /// intentionally public.
    UngatedMutatingRoutes { routes: Vec<String> },
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
            BuildError::ForeignKeyCycle { edges } => {
                writeln!(
                    f,
                    "umbral: the plugins' foreign keys form a cycle, so no CREATE TABLE order \
                     satisfies every REFERENCES clause on a fresh database:"
                )?;
                for edge in edges {
                    writeln!(
                        f,
                        "  `{}`.\"{}\" REFERENCES \"{}\", owned by `{}`",
                        edge.plugin, edge.table, edge.fk_target, edge.depends_on
                    )?;
                }
                write!(
                    f,
                    "break the cycle by making one side a nullable FK added in a later \
                     migration, or by opting that column out of the physical constraint with \
                     #[umbral(db_constraint = false)]"
                )
            }
            BuildError::DuplicatePluginName { name } => write!(
                f,
                "umbral: two plugins both report name `{name}`; plugin names are unique keys \
                 (migration tracking, dependency graph)"
            ),
            BuildError::SettingsDatabasePool { alias, error } => write!(
                f,
                "umbral: could not open the `settings.databases` pool for alias `{alias}`: \
                 {error}"
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
            BuildError::UngatedMutatingRoutes { routes } => write!(
                f,
                "umbral: deny_ungated_mutations() is set and {} app mutating route(s) have no \
                 recorded permission: [{}]. Gate each with the umbral-permissions \
                 `Routes::require_permission(...)` builder so the framework records the \
                 permission (a hand-applied `.layer(permission_required(...))` is NOT visible \
                 to this audit — prefer the builder). If a route is intentionally public, \
                 register it through a permission-aware builder or drop the strict flag.",
                routes.len(),
                routes.join(", ")
            ),
        }
    }
}

impl std::error::Error for BuildError {}

#[cfg(test)]
mod audit_tests {
    use super::ungated_mutating_routes;
    use crate::routes::RouteSpec;

    fn spec(methods: Vec<&'static str>, path: &str, perm: Option<&str>) -> RouteSpec {
        RouteSpec {
            path: path.to_string(),
            methods,
            permission: perm.map(str::to_string),
        }
    }

    #[test]
    fn flags_ungated_mutating_routes_only() {
        let specs = vec![
            spec(vec!["GET"], "/", None),                     // read → ignored
            spec(vec!["POST"], "/contact", None),             // ungated mutating → flagged
            spec(vec!["POST"], "/posts", Some("blog.add")),   // gated → ignored
            spec(vec!["DELETE"], "/posts/{id}", None),        // ungated mutating → flagged
            spec(vec!["GET", "POST"], "/api/comments", None), // has a mutating verb → flagged
        ];
        let flagged = ungated_mutating_routes(&specs);
        assert_eq!(
            flagged,
            vec![
                "POST /contact".to_string(),
                "DELETE /posts/{id}".to_string(),
                "GET/POST /api/comments".to_string(),
            ]
        );
    }

    #[test]
    fn no_warning_when_all_mutating_routes_are_gated_or_read_only() {
        let specs = vec![
            spec(vec!["GET"], "/", None),
            spec(vec!["POST"], "/posts", Some("blog.add")),
        ];
        assert!(ungated_mutating_routes(&specs).is_empty());
    }
}

#[cfg(test)]
mod drain_tests {
    use super::drain_after;
    use std::time::{Duration, Instant};

    /// `drain_after` awaits its signal, flips the process to draining, then holds
    /// for the delay before resolving — the sequence that lets `/readyz` report
    /// 503 while the server keeps accepting during the drain window (Kikosi #5).
    ///
    /// One test, walked in sequence: the draining flag is a process-global that
    /// `begin_drain` only ever sets, so splitting the zero-delay and with-delay
    /// cases into separate concurrent tests would race on it. A ready signal
    /// (`async {}`) exercises the drain logic without delivering a real SIGTERM.
    #[tokio::test]
    async fn signals_draining_and_holds_for_the_delay() {
        assert!(
            !crate::shutdown::is_draining(),
            "draining must start false — nothing has signalled shutdown yet",
        );

        // Zero delay: marks draining, does not sleep (the historical
        // instant-shutdown behaviour).
        let started = Instant::now();
        drain_after(async {}, Duration::ZERO).await;
        assert!(
            crate::shutdown::is_draining(),
            "the signal must mark the process draining so /readyz goes 503",
        );
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "zero delay must not sleep; took {:?}",
            started.elapsed(),
        );

        // A non-zero delay holds before resolving, even though the process is
        // already draining (begin_drain is idempotent; the hold still applies).
        let started = Instant::now();
        drain_after(async {}, Duration::from_millis(120)).await;
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "must hold for ~the drain delay before resolving; held {:?}",
            started.elapsed(),
        );
    }
}
