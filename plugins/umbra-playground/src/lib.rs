//! umbra-playground — interactive API playground UI for umbra-rest.
//!
//! MVP: a 3-pane React UI mounted at `/api/playground/`, fetching the
//! existing `umbra-openapi` JSON spec at runtime. See the design spec
//! at `docs/superpowers/specs/2026-06-02-rest-playground-design.md`.

use include_dir::{Dir, include_dir};
use umbra::prelude::*;

pub mod routes;

mod generated_assets {
    include!(concat!(env!("OUT_DIR"), "/generated_assets.rs"));
}

pub(crate) use generated_assets::{CSS, JS};

/// The compile-time-embedded vite asset tree.
///
/// `include_dir!` walks `<crate>/dist/assets/` at macro-expansion
/// time and produces a `Dir` whose entries are baked into the
/// binary as `&'static [u8]`. The runtime serves files by name out
/// of this tree — no filesystem read, no CARGO_MANIFEST_DIR runtime
/// resolution, no risk of a wiped dist/ orphaning live requests
/// from a browser that's still holding the old shell HTML.
///
/// build.rs guarantees `dist/assets/` exists (with at least
/// placeholder files when vite isn't available) so this macro
/// always has *something* to embed.
pub(crate) static ASSETS: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/dist/assets");

/// Placeholder HTML served when the vite bundle couldn't be built
/// (no npm, no node_modules, vite failed). Inline so the plugin
/// always renders *something*.
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
        self.base_path = if trimmed.is_empty() {
            "/".to_string()
        } else {
            trimmed
        };
        self
    }

    /// Test-only accessor for the configured base path. Marked
    /// `#[doc(hidden)]` so it doesn't appear in the public API.
    #[doc(hidden)]
    pub fn base_path_for_test(&self) -> &str {
        &self.base_path
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
