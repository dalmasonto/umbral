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
pub(crate) static ASSETS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/dist/assets");

/// Placeholder HTML served when the vite bundle couldn't be built
/// (no npm, no node_modules, vite failed). Inline so the plugin
/// always renders *something*.
pub(crate) const PLACEHOLDER_HTML: &str = include_str!("placeholder.html");

/// The playground plugin.
///
/// Scoped per app: every umbra app that wires the playground passes
/// a unique `app_name`, which the frontend reads (via a `<meta>` tag
/// + `window.__UMBRA_PLAYGROUND_APP__` global injected into the
/// shell) and uses to namespace every browser-side storage key:
/// the Dexie database, every `localStorage` key (theme, settings,
/// selected operation), and the legacy history key carried over from
/// the localStorage era. Closes gap #71 — two apps served from the
/// same browser (different ports) no longer share history or
/// settings.
#[derive(Debug, Clone)]
pub struct PlaygroundPlugin {
    base_path: String,
    app_name: String,
}

impl Default for PlaygroundPlugin {
    /// Fallback shape: `app_name = "default"` with a `tracing::warn`
    /// at construct time so developers see the missing scope. Two
    /// apps that both default-construct still collide; the warning
    /// nudges them to call `PlaygroundPlugin::new(app_name)` instead.
    fn default() -> Self {
        tracing::warn!(
            "umbra-playground: PlaygroundPlugin::default() falls back to app_name = \"default\"; \
             use PlaygroundPlugin::new(\"<your-app>\") so two apps on the same browser \
             don't share history / settings (gap #71)",
        );
        Self::with_defaults("default")
    }
}

impl PlaygroundPlugin {
    /// Construct the playground for an app named `app_name`. The
    /// name is opaque to the framework — it's only used to scope
    /// browser-side storage so two umbra apps served to the same
    /// browser (e.g. on `127.0.0.1:8000` and `127.0.0.1:8001`)
    /// don't share history, theme, or saved settings. A short
    /// project-style slug is the usual choice (`"shop"`, `"crm"`,
    /// `"my-blog"`). Closes gap #71.
    pub fn new(app_name: impl Into<String>) -> Self {
        Self::with_defaults(app_name)
    }

    fn with_defaults(app_name: impl Into<String>) -> Self {
        Self {
            base_path: "/api/playground".to_string(),
            app_name: app_name.into(),
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

    /// Test-only accessor for the configured app name.
    #[doc(hidden)]
    pub fn app_name_for_test(&self) -> &str {
        &self.app_name
    }
}

impl Plugin for PlaygroundPlugin {
    fn name(&self) -> &'static str {
        "umbra-playground"
    }

    fn routes(&self) -> axum::Router {
        let degraded = JS.starts_with("playground.placeholder");
        let state = routes::PlaygroundState::new(
            self.base_path.clone(),
            self.app_name.clone(),
            degraded,
        );
        routes::router(state)
    }
}
