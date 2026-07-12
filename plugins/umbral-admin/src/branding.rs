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
    /// Gap 107: admin base path (default `/admin`). Surfaced to
    /// templates as the `admin_base` Jinja global so cross-page
    /// links and HTMX targets resolve under whatever prefix
    /// `AdminPlugin::at()` configured.
    pub base_path: String,
    /// gaps2 #33 — whether the "restore last changelist" feature is
    /// active. When `true` (default), `/admin/` redirects the user to
    /// the last-visited changelist URL stored in
    /// `admin_user_pref.preferences.last_path`, and the "Home" breadcrumb
    /// link carries `?dashboard=1` so the dashboard is reachable in one
    /// click. When `false`, the index always renders the dashboard and
    /// the changelist handler stops writing `last_path` (no dead data).
    pub restore_last_path: bool,
    /// gaps3 #67 — the version string in the sidebar and on the login page.
    ///
    /// `None` hides it entirely (`AdminPlugin::show_version(false)`). The default is
    /// umbral's OWN version, read from `CARGO_PKG_VERSION` at compile time — the
    /// templates used to hardcode the literal `v0.0.1`, which had been wrong since
    /// 0.0.2 and would have gone on being wrong forever.
    ///
    /// An app that would rather advertise ITS version than the framework's sets its own
    /// string: `AdminPlugin::default().version(concat!("MyShop v", env!("CARGO_PKG_VERSION")))`.
    /// Whose version an admin should show is a product decision, not ours.
    pub version_label: Option<String>,
}

/// umbral's own version — the crate version of `umbral-admin`, which tracks the
/// workspace. Not a literal, so it cannot go stale.
pub fn umbral_version_label() -> String {
    format!("umbral v{}", env!("CARGO_PKG_VERSION"))
}

impl Default for AdminBranding {
    fn default() -> Self {
        Self {
            site_title: "umbral admin".to_string(),
            site_description: String::new(),
            brand_color: String::new(),
            base_path: "/admin".to_string(),
            restore_last_path: true,
            version_label: Some(umbral_version_label()),
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
