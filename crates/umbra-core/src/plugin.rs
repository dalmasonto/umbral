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

use axum::Router;
use sqlx::SqlitePool;

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
    /// hand-written one passed to `AppBuilder::router()`. Plugins
    /// choose their own path prefixes (spec 02 §"What a plugin can
    /// contribute": routes are flat, not auto-prefixed).
    fn routes(&self) -> Router {
        Router::new()
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

    /// Wire signals, start background work, seal admin registrations.
    /// Called after phase 4 (system checks) passes, in topological
    /// dependency order. Sync, on purpose; spawn async work via
    /// `ctx.runtime()` when the runtime handle lands.
    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        Ok(())
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
    /// The default SQLite pool, same as `umbra::db::pool()` returns.
    pub pool: SqlitePool,
    /// A clone of the active settings.
    pub settings: Settings,
}

/// Errors a plugin's `on_ready` can return. Boxed under
/// `BuildError::PluginOnReady` so the build phase surfaces them with
/// the plugin name attached.
pub type PluginError = Box<dyn std::error::Error + Send + Sync>;
