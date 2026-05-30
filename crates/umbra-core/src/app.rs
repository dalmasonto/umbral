use axum::Router;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::db;
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
}

impl AppBuilder {
    /// Set the application settings.
    pub fn settings(mut self, settings: Settings) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Register a database pool under the given alias.
    ///
    /// The `"default"` pool is the one returned by `umbra::db::pool()`.
    /// If no pool is registered, `build()` will auto-connect one from
    /// `settings.database_url`.
    pub fn database(mut self, alias: &str, pool: SqlitePool) -> Self {
        self.databases.insert(alias.to_owned(), pool);
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
    ///    builder-local state.
    /// 2. **Publish ambient state.** Write settings and pools into their
    ///    `OnceLock`s so accessors like `umbra::db::pool()` work.
    /// 3. **Build router.** Merge every registered plugin's routes (M7+)
    ///    with the hand-written router. At M0, only the hand-written
    ///    router exists.
    ///
    /// Phases 4 (system check) and 5 (on_ready) are no-ops at M0; they
    /// land when the Plugin contract and backend abstraction exist.
    pub fn build(mut self) -> Result<App, BuildError> {
        // Phase 1 — collect settings
        let settings = self
            .settings
            .take()
            .unwrap_or_else(|| Settings::from_env().expect("umbra: failed to load settings"));

        // Phase 1 — ensure a default pool exists
        if !self.databases.contains_key("default") {
            let pool = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("umbra: failed to create temporary runtime for pool init")
                .block_on(db::connect(&settings.database_url))
                .map_err(|e| BuildError::Database(e.to_string()))?;
            self.databases.insert("default".to_owned(), pool);
        }

        // Phase 2 — publish ambient state
        crate::settings::init(&settings);
        db::init(self.databases);

        // Phase 3 — build the merged router
        let router = self.router.unwrap_or_else(|| {
            Router::new().fallback(|| async { "umbra is running, but no routes are registered." })
        });

        Ok(App { router })
    }
}

/// Errors that can occur during `AppBuilder::build()`.
#[derive(Debug)]
pub enum BuildError {
    /// Failed to connect to the database.
    Database(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Database(msg) => write!(f, "database error: {msg}"),
        }
    }
}

impl std::error::Error for BuildError {}
