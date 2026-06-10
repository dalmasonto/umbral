//! PluginDirectoryPlugin — owns the plugin directory for umbra.dev.
//!
//! Wire this into your App by adding to `src/main.rs`:
//!
//! ```ignore
//! .plugin(plugin_directory::PluginDirectoryPlugin::default())
//! ```
//!
//! Declare models, routes, and `on_ready` work in the impl below.
//! See `documentation/docs/v0.0.1/plugins/the-plugin-trait.mdx` for
//! what each method does.

pub mod models;
pub mod seed;

pub use models::{
    AuditStatus, CommentKind, CommentModeration, PluginCompatibility, PluginFeature,
    PluginMaturity, PluginModeration, PluginSource, PluginStatus, SecurityStatus,
};

use umbra::migrate::ModelMeta;
use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::web::Router;

#[derive(Debug, Default, Clone)]
pub struct PluginDirectoryPlugin;

impl Plugin for PluginDirectoryPlugin {
    fn name(&self) -> &'static str {
        "plugin_directory"
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![
            ModelMeta::for_::<models::Plugin>(),
            ModelMeta::for_::<models::PluginFeature>(),
            ModelMeta::for_::<models::PluginCompatibility>(),
            ModelMeta::for_::<models::PluginComment>(),
        ]
    }

    fn routes(&self) -> Router {
        // Add your routes here. The base path is up to you — convention
        // is `/<name>/...` for HTML and `/api/<name>/...` for JSON.
        Router::new()
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        // Seed the first-party plugin rows the first time the
        // server starts. The seed is idempotent (short-circuits
        // when the table is non-empty), so this is safe on every
        // boot. Failures log a warning but do not crash startup —
        // the home page falls back to its static table when the
        // DB is empty.
        let plugin_name = self.name();
        tokio::spawn(async move {
            match seed::seed_official_plugins().await {
                Ok(0) => tracing::debug!(
                    "{}: official plugin table already populated, seed skipped",
                    plugin_name
                ),
                Ok(n) => tracing::info!(
                    "{}: seeded {} official plugin rows",
                    plugin_name,
                    n
                ),
                Err(e) => tracing::warn!(
                    "{}: official plugin seed failed: {e}. \
                     Home page will fall back to the static plugin table.",
                    plugin_name
                ),
            }
        });
        Ok(())
    }
}
