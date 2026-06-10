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
        Ok(())
    }
}
