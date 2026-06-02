//! Admin chrome the developer can rebrand at plugin-build time —
//! site title, site description, and brand color.
//!
//! Stored on `AdminPlugin` during construction (chainable builders),
//! sealed into the global [`BRANDING`] cell at `Plugin::routes()`
//! time, and exposed to every template as the globals `site_title`,
//! `site_description`, and `brand_color`. The wrapper template
//! injects a `<style>` overriding `--primary` when a brand color is
//! set so the entire theme tints accordingly.

use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct AdminBranding {
    pub site_title: String,
    pub site_description: String,
    pub brand_color: String,
}

impl Default for AdminBranding {
    fn default() -> Self {
        Self {
            site_title: "umbra admin".to_string(),
            site_description: String::new(),
            brand_color: String::new(),
        }
    }
}

/// Per-process branding. Sealed once at `Plugin::routes()` time;
/// subsequent attempts to set it are silent no-ops, matching
/// `App::build`'s "build once" expectation.
pub(crate) static BRANDING: OnceLock<AdminBranding> = OnceLock::new();

/// Read the active branding. Falls back to defaults if `routes()`
/// hasn't sealed the value (test harnesses, ad-hoc renders).
pub(crate) fn current() -> AdminBranding {
    BRANDING.get().cloned().unwrap_or_default()
}
