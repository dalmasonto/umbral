//! SecurityReportsPlugin — the `/security` security policy page.
//!
//! A static policy/disclosure page (editorial content, not records), with
//! a clear path to report a vulnerability via the existing `/report` flow.

pub mod models;

use std::path::PathBuf;

use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::templates::context;
use umbral::web::{Html, Router, StatusCode, get};

#[derive(Debug, Default, Clone)]
pub struct SecurityReportsPlugin;

impl Plugin for SecurityReportsPlugin {
    fn name(&self) -> &'static str {
        "security_reports"
    }

    fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
        Vec::new()
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn routes(&self) -> Router {
        Router::new().route("/security", get(security_page))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

async fn security_page() -> Result<Html<String>, (StatusCode, String)> {
    umbral::templates::render("security_reports/security.html", &context! {})
        .map(Html)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
