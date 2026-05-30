use axum::Router;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::db;
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
}

/// The fluent entry point for constructing an [`App`].
///
/// Collects settings, database pools, and routes, then locks everything
/// into place at [`build`](AppBuilder::build).
#[derive(Default)]
pub struct AppBuilder {
    settings: Option<Settings>,
    databases: HashMap<String, SqlitePool>,
    router: Option<Router>,
    models: Vec<ModelMeta>,
    plugins: Vec<Box<dyn Plugin>>,
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
    pub fn database(mut self, alias: &str, pool: SqlitePool) -> Self {
        self.databases.insert(alias.to_owned(), pool);
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

    /// Finalize the application.
    ///
    /// Phases (see spec 01 §Mechanics and invariants):
    ///
    /// 1. **Collect.** Gather settings, databases, and router from
    ///    builder-local state. Settings must be set explicitly via
    ///    `.settings(...)`; the "default" database pool must be
    ///    registered via `.database("default", pool)`. The caller
    ///    opens the pool first (with `umbra::db::connect(...).await`)
    ///    and hands it to the builder. This matches the canonical
    ///    pattern in spec 01-app-and-settings.md.
    /// 2. **Detect backend.** `backend::detect(&settings.database_url)`
    ///    picks one of the shipped `DatabaseBackend` impls (M4
    ///    abstraction). An unknown URL scheme (mysql / oracle / etc.)
    ///    fails here, before any system check runs.
    /// 3. **Publish ambient state.** Write settings, pools, and the
    ///    active backend into their `OnceLock`s.
    /// 4. **System check.** Run framework-built-in checks against the
    ///    just-published context (active backend + settings). Errors
    ///    block boot; warnings log and continue.
    /// 5. **Build router.** Merge every registered plugin's routes (M7+)
    ///    with the hand-written router. At M4, only the hand-written
    ///    router exists.
    ///
    /// `Plugin::on_ready` (the doc-comment originally called this phase
    /// 5) lives in M7 with the rest of the Plugin contract; the M4
    /// build doesn't fire it.
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

        // Phase 2 — detect backend from the configured URL.
        let backend =
            crate::backend::detect(&settings.database_url).map_err(BuildError::BackendDetect)?;

        // Phase 3 — publish ambient state
        crate::settings::init(&settings);
        db::init(self.databases);
        crate::backend::init(backend);
        crate::migrate::init(self.models);

        // Phase 4 — system check. Build the context against ambient
        // state, run the framework checks, partition into errors vs
        // warnings, log the warnings, fail the build on any errors.
        let ctx = crate::check::CheckContext {
            backend,
            settings: crate::settings::get(),
        };
        let checks = crate::check::framework_checks();
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

        // Phase 5 — build the merged router
        let router = self.router.unwrap_or_else(|| {
            Router::new().fallback(|| async { "umbra is running, but no routes are registered." })
        });

        Ok(App { router })
    }
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
        }
    }
}

impl std::error::Error for BuildError {}
