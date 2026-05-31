use axum::Router;
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::db::{self, DbPool};
use crate::migrate::ModelMeta;
use crate::orm::Model;
use crate::plugin::Plugin;
use crate::settings::Settings;

/// A built and ready-to-serve umbra application.
///
/// Created via `App::builder().build()`. Owns the merged router that
/// carries every registered plugin's routes (and, at M0, the hand-written
/// route passed to `AppBuilder::router()`).
pub struct App {
    router: Router,
    plugins: Vec<Box<dyn Plugin>>,
}

impl App {
    /// Create a new [`AppBuilder`].
    pub fn builder() -> AppBuilder {
        AppBuilder::default()
    }

    /// Bind the axum listener and serve requests.
    ///
    /// This call blocks until the server stops. At M0 there is no graceful
    /// shutdown hook; that lands with the signal-handling work in a later
    /// milestone.
    pub async fn serve(self, addr: impl Into<SocketAddr>) -> Result<(), std::io::Error> {
        let listener = tokio::net::TcpListener::bind(addr.into()).await?;

        tracing::info!("umbra serving on {}", listener.local_addr()?);

        axum::serve(listener, self.router).await
    }

    /// Consume the [`App`] and return its merged axum router.
    ///
    /// Useful when the caller wants to drive the router themselves: an
    /// integration test that sends synthetic requests via
    /// `tower::ServiceExt::oneshot`, an embedding scenario that nests
    /// umbra under another axum tree, or any other path that doesn't
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
#[derive(Default)]
pub struct AppBuilder {
    settings: Option<Settings>,
    databases: HashMap<String, DbPool>,
    router: Option<Router>,
    models: Vec<ModelMeta>,
    plugins: Vec<Box<dyn Plugin>>,
    templates_dir: Option<std::path::PathBuf>,
    slash_redirect: crate::slash::SlashRedirect,
}

impl AppBuilder {
    /// Set the application settings.
    pub fn settings(mut self, settings: Settings) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Register a database pool under the given alias.
    ///
    /// The `"default"` pool is the one returned by `umbra::db::pool()`
    /// and is required: `build()` fails with `BuildError::
    /// DefaultPoolMissing` if it isn't registered. The caller opens
    /// the pool via `umbra::db::connect(&url).await` and passes it
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

    /// Attach a hand-written axum router.
    ///
    /// At M0 this is the primary way to register routes. From M7 onward,
    /// plugins contribute routes through their `Plugin::routes()` method
    /// and this escape hatch is reserved for ad-hoc routes outside any
    /// plugin.
    pub fn router(mut self, router: Router) -> Self {
        self.router = Some(router);
        self
    }

    /// Set the templates directory.
    ///
    /// Defaults to `./templates` (relative to the binary's cwd) when
    /// the builder method isn't called. If the resolved path doesn't
    /// exist, the engine still publishes — calls to
    /// `umbra::templates::render` then return `TemplateError::Missing`
    /// with a clear diagnostic, which matches the "absence isn't an
    /// error unless something tries to render" rule from the spec.
    ///
    /// Per-plugin templates directories with a dependency-ordered
    /// search path land with the admin plugin at M11; today the
    /// project root is the only path the engine knows about.
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
    /// use umbra::prelude::*;
    /// use umbra::web::SlashRedirect;
    ///
    /// App::builder()
    ///     .slash_redirect(SlashRedirect::Append)
    ///     .build()?;
    /// ```
    pub fn slash_redirect(mut self, policy: crate::slash::SlashRedirect) -> Self {
        self.slash_redirect = policy;
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
    ///    opens the pool first (with `umbra::db::connect(...).await`)
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
        // post-build callers (notably `umbra::cli::dispatch`) can walk
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
        // below.
        let mut model_aliases: HashMap<String, String> = HashMap::new();
        for plugin in &sorted_plugins {
            let Some(alias) = plugin.database() else {
                continue;
            };
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

        // Phase 3 — publish ambient state. The model registry now carries
        // one entry per registered plugin (the implicit `"app"` plugin
        // for `.model::<T>()` registrations, plus every `.plugin(...)`
        // contribution). Plugins that contribute zero models still get a
        // map entry; the flattening in `migrate::init_plugins` collapses
        // them to nothing in the registry but the per-plugin model walk
        // stays deterministic.
        crate::settings::init(&settings);
        db::init(self.databases);
        crate::backend::init(backend);

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

        // Publish the per-plugin model alias map collected in phase
        // 2.5. Done after `migrate::init_plugins` so the migration
        // registry is alive when QuerySet's resolve_pool starts
        // looking up by `Model::NAME`.
        crate::migrate::init_model_aliases(model_aliases);

        // Templates engine — published before phase 4 so a future
        // plugin system_check that wants to inspect the loaded
        // templates can. Default templates directory is `./templates`
        // relative to the binary's cwd; the builder method overrides.
        let templates_dir = self
            .templates_dir
            .take()
            .unwrap_or_else(|| std::path::PathBuf::from("templates"));
        crate::templates::init(&templates_dir).map_err(BuildError::TemplatesInit)?;

        // Phase 4 — system check. Build the context against ambient
        // state, run the framework checks plus every plugin's
        // contribution in topological order, partition into errors vs
        // warnings, log the warnings, fail the build on any errors.
        let ctx = crate::check::CheckContext {
            backend,
            settings: crate::settings::get(),
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
                        "umbra system check warning: {}",
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
            Router::new().fallback(|| async { "umbra is running, but no routes are registered." })
        });
        for plugin in &sorted_plugins {
            router = router.merge(plugin.routes());
        }

        // Phase 5.5 — apply each plugin's middleware in topological
        // order. Later plugins wrap earlier ones, so a security
        // plugin declared after the auth plugin sees the auth-
        // augmented router and can add its own layer on top. This
        // is the M7 deferral being lifted now that umbra-security
        // needs it.
        for plugin in &sorted_plugins {
            router = plugin.wrap_router(router);
        }

        // Phase 5.6 — install the trailing-slash redirect fallback if
        // the user opted in via `.slash_redirect()`. We snapshot the
        // router BEFORE installing the fallback — the snapshot is what
        // gets probed for the alternate path, and it can't recursively
        // hit this fallback because it doesn't have one. axum
        // `Router::layer()` wraps individual routes only; it doesn't
        // run on requests that miss every route, so it can't catch
        // those 404s. The fallback handler is the right surface for
        // catching missed paths.
        if self.slash_redirect != crate::slash::SlashRedirect::Off {
            let snapshot = router.clone();
            let fallback = crate::slash::slash_redirect_fallback(snapshot, self.slash_redirect);
            router = router.fallback(fallback);
        }

        // Phase 6 — fire each plugin's `on_ready` in topological order.
        // Runs after the system check passes and after the router is
        // built, so a plugin can rely on ambient state being live and on
        // any earlier dependency's `on_ready` having already run.
        let ctx = crate::plugin::AppContext {
            pool: crate::db::pool(),
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
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::SettingsMissing => write!(
                f,
                "umbra: App::builder() requires Settings; call .settings(Settings::from_env()?) before .build()"
            ),
            BuildError::BackendDetect(err) => write!(f, "{err}"),
            BuildError::SystemCheckFailed { findings } => {
                writeln!(f, "umbra: {} system check(s) failed:", findings.len())?;
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
                "umbra: App::builder() requires a default DB pool; call .database(\"default\", umbra::db::connect(&url).await?) before .build()"
            ),
            BuildError::DependencyNotFound { plugin, missing } => write!(
                f,
                "umbra: plugin `{plugin}` depends on `{missing}`, which isn't registered; \
                 call .plugin({missing}::default()) on the builder"
            ),
            BuildError::PluginCycle { names } => {
                write!(f, "umbra: plugin dependency cycle: {}", names.join(" -> "))
            }
            BuildError::DuplicatePluginName { name } => write!(
                f,
                "umbra: two plugins both report name `{name}`; plugin names are unique keys \
                 (migration tracking, dependency graph)"
            ),
            BuildError::ReservedPluginName => write!(
                f,
                "umbra: the plugin name `app` is reserved for models registered via \
                 .model::<T>(); pick a different name"
            ),
            BuildError::PluginOnReady { plugin, source } => {
                write!(f, "umbra: plugin `{plugin}`'s on_ready failed: {source}")
            }
            BuildError::TemplatesInit(err) => {
                write!(f, "umbra: templates engine failed to initialise: {err}")
            }
            BuildError::PluginDatabaseAlias { plugin, alias } => write!(
                f,
                "umbra: plugin `{plugin}` requested database alias `{alias}`, which isn't \
                 registered; call .database(\"{alias}\", pool) on the builder before .build()"
            ),
            BuildError::DatabaseBackendMismatch {
                url_backend,
                pool_backend,
            } => write!(
                f,
                "umbra: settings.database_url names backend `{url_backend}`, but the \
                 default pool passed to .database(...) is a `{pool_backend}` pool. \
                 Either change UMBRA_DATABASE_URL to match the pool, or open the pool \
                 against a URL whose scheme matches umbra::db::connect."
            ),
        }
    }
}

impl std::error::Error for BuildError {}
