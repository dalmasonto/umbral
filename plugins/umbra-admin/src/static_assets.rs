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
    // These URLs are the unified static pipeline paths: `/static/admin/…`.
    // Phase 5.4 of `App::build` mounts each as a *specific* GET route that
    // serves the embedded bytes, taking precedence over the nested pipeline
    // fallback (Phase 5.45) at `static_url`. That keeps the admin zero-config
    // (single-binary, dev AND prod, no `collect_static` required) while moving
    // it onto the same `/static/admin/…` URL every other static asset uses.
    //
    // `url_path` is a `&'static str`, so these are compile-time consts hard-
    // coded against the DEFAULT `static_url` (`/static/`). Trade-off: if a
    // deployment customises `static_url` away from `/static/`, the embedded
    // route won't match the rewritten template URLs — that deployment must
    // serve the admin assets through the pipeline / `collect_static` path
    // (which `static_dirs()` below feeds) instead of the in-binary route.
    vec![
        StaticFile {
            url_path: "/static/admin/admin.css",
            content_type: "text/css; charset=utf-8",
            body: ADMIN_CSS_BYTES,
            cache_control: Some("public, max-age=86400"),
        },
        StaticFile {
            url_path: "/static/admin/admin.js",
            content_type: "application/javascript; charset=utf-8",
            body: ADMIN_JS_BYTES,
            cache_control: Some("public, max-age=86400"),
        },
    ]
}
