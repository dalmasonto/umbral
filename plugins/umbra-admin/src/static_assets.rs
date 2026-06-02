//! Compiled admin.css — embedded at compile time, served in prod, the
//! Tailwind CDN replaces it in dev (see `wrapper.html`).
//!
//! Goes through the framework's [`umbra::plugin::StaticFile`] hook:
//! `AdminPlugin::static_files()` returns one entry referencing
//! [`ADMIN_CSS_BYTES`], and `App::build` mounts the route. No
//! hand-written handler lives here. Build the CSS with:
//!
//! ```sh
//! cd plugins/umbra-admin/css && npm install && npm run build
//! ```

use umbra::plugin::StaticFile;

/// The compiled stylesheet bytes. Re-exported as a constant so the
/// `static_files()` registration can reach it.
pub(crate) const ADMIN_CSS_BYTES: &[u8] = include_bytes!("assets/admin.css");

/// The list of static files this plugin ships. One file today; if the
/// admin ever embeds a JS bundle or icon font, append to this list.
pub(crate) fn admin_static_files() -> Vec<StaticFile> {
    vec![StaticFile {
        url_path: "/admin/static/admin.css",
        content_type: "text/css; charset=utf-8",
        body: ADMIN_CSS_BYTES,
        cache_control: Some("public, max-age=86400"),
    }]
}
