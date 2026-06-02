//! umbra-playground — interactive API playground UI for umbra-rest.
//!
//! MVP: a 3-pane React UI mounted at `/api/playground/`, fetching the
//! existing `umbra-openapi` JSON spec at runtime. See the design spec
//! at `docs/superpowers/specs/2026-06-02-rest-playground-design.md`.

mod generated_assets {
    include!(concat!(env!("OUT_DIR"), "/generated_assets.rs"));
}

pub(crate) use generated_assets::{CSS, JS};

/// Placeholder HTML served when esbuild/tailwindcss were not available
/// at build time. Inline so the plugin always renders *something*.
pub(crate) const PLACEHOLDER_HTML: &str = include_str!("placeholder.html");

/// Placeholder. The real plugin type lands in Task 6.
pub struct PlaygroundPlugin;
