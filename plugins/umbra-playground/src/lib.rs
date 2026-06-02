//! umbra-playground — interactive API playground UI for umbra-rest.
//!
//! MVP: a 3-pane React UI mounted at `/api/playground/`, fetching the
//! existing `umbra-openapi` JSON spec at runtime. See the design spec
//! at `docs/superpowers/specs/2026-06-02-rest-playground-design.md`.

use umbra::prelude::*;

pub mod routes;
pub mod static_serve;

mod generated_assets {
    include!(concat!(env!("OUT_DIR"), "/generated_assets.rs"));
}

pub(crate) use generated_assets::{CSS, JS};

/// Placeholder HTML served when esbuild/tailwindcss were not available
/// at build time. Inline so the plugin always renders *something*.
pub(crate) const PLACEHOLDER_HTML: &str = include_str!("placeholder.html");

/// The playground plugin.
#[derive(Debug, Clone)]
pub struct PlaygroundPlugin {
    base_path: String,
}

impl Default for PlaygroundPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaygroundPlugin {
    pub fn new() -> Self {
        Self {
            base_path: "/api/playground".to_string(),
        }
    }

    /// Mount under a different path. Trailing slashes are normalised.
    pub fn at(mut self, path: impl Into<String>) -> Self {
        let trimmed = path.into().trim_end_matches('/').to_string();
        self.base_path = if trimmed.is_empty() { "/".to_string() } else { trimmed };
        self
    }
}

impl Plugin for PlaygroundPlugin {
    fn name(&self) -> &'static str {
        "umbra-playground"
    }

    fn routes(&self) -> axum::Router {
        let degraded = JS.starts_with("playground.placeholder");
        let state = routes::PlaygroundState::new(self.base_path.clone(), degraded);
        routes::router(state)
    }
}
