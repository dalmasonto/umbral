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

/// gaps2 #4 — the admin runtime JS, extracted out of wrapper.html's
/// inline `<script>` blocks. Same `include_bytes!` shipping pattern
/// as the CSS: embedded at compile time, served as a static asset,
/// no runtime FS lookup.
pub(crate) const ADMIN_JS_BYTES: &[u8] = include_bytes!("assets/admin.js");

/// The list of static files this plugin ships. If the admin grows
/// more bundle output (font subset, separate widget bundles), append
/// here.
pub(crate) fn admin_static_files() -> Vec<StaticFile> {
    vec![
        StaticFile {
            url_path: "/admin/static/admin.css",
            content_type: "text/css; charset=utf-8",
            body: ADMIN_CSS_BYTES,
            cache_control: Some("public, max-age=86400"),
        },
        StaticFile {
            url_path: "/admin/static/admin.js",
            content_type: "application/javascript; charset=utf-8",
            body: ADMIN_JS_BYTES,
            cache_control: Some("public, max-age=86400"),
        },
    ]
}
