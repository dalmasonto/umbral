//! Two routes: the HTML shell and the bundled assets.
//!
//! The shell is one HTML template substituted with the hashed asset
//! filenames and lives here. The assets are served by `umbra-static`'s
//! `StaticPlugin` in its embedded variant — we hand it the
//! compile-time-baked [`crate::ASSETS`] tree and it gives us back a
//! Router that resolves `<base>/assets/{*path}` against the in-memory
//! `Dir`. No filesystem reads, no path-traversal surface, no risk of
//! a wiped `dist/` orphaning live browser tabs.
//!
//! Dogfooding the framework's static plugin means any improvement
//! `umbra-static` ships (cache headers, dev-mode max-age=0, future
//! ETag/range-request support against embedded sources) lands here
//! for free.
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use umbra::prelude::Plugin;
use umbra_static::StaticPlugin;

use crate::{ASSETS, CSS, JS, PLACEHOLDER_HTML};

/// Shared state carried through middleware: the base path (e.g.
/// `/api/playground`) and a flag for whether we're in placeholder mode.
#[derive(Clone, Debug)]
pub struct PlaygroundState {
    pub base_path: Arc<str>,
    /// Per-app scope used by the frontend to namespace browser-side
    /// storage. Closes gap #71. Injected into the rendered shell
    /// HTML as both a `<meta>` tag (for non-JS introspection) and
    /// the global `window.__UMBRA_PLAYGROUND_APP__` for the
    /// state-store helpers to read at boot.
    pub app_name: Arc<str>,
    pub degraded: bool,
}

impl PlaygroundState {
    pub fn new(
        base_path: impl Into<String>,
        app_name: impl Into<String>,
        degraded: bool,
    ) -> Self {
        Self {
            base_path: Arc::from(base_path.into()),
            app_name: Arc::from(app_name.into()),
            degraded,
        }
    }
}

const SHELL_HTML: &str = include_str!("shell.html");

/// Render the HTML shell, inserting the hashed asset paths and the
/// per-app scope. The app name is HTML-attribute-escaped (quotes +
/// `<>` + `&`) so an exotic project slug can't break out of the
/// meta-tag attribute or the inline script.
fn render_shell(state: &PlaygroundState) -> String {
    if state.degraded {
        return PLACEHOLDER_HTML.to_string();
    }
    let css = format!("{}/assets/{}", state.base_path, CSS);
    let js = format!("{}/assets/{}", state.base_path, JS);
    let app_meta = html_escape_attr(&state.app_name);
    let app_js = json_escape(&state.app_name);
    SHELL_HTML
        .replace("__CSS_PATH__", &css)
        .replace("__JS_PATH__", &js)
        .replace("__APP_NAME_ATTR__", &app_meta)
        .replace("__APP_NAME_JSON__", &app_js)
}

/// HTML attribute escape. Replaces `&`, `<`, `>`, `"`, `'` with
/// their entity forms so an `app_name` containing a `"` (or a `<`)
/// can't break out of the `content="..."` attribute on the meta tag.
fn html_escape_attr(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '&' => "&amp;".chars().collect::<Vec<_>>(),
            '<' => "&lt;".chars().collect(),
            '>' => "&gt;".chars().collect(),
            '"' => "&quot;".chars().collect(),
            '\'' => "&#39;".chars().collect(),
            _ => vec![c],
        })
        .collect()
}

/// JSON-string escape — quotes the value inline so the inline
/// `<script>` tag carries `window.__UMBRA_PLAYGROUND_APP__ =
/// "<value>"` with backslash-escaping for the dangerous chars
/// (`"`, `\`, `/`, control chars). Backed by `serde_json::to_string`
/// for correctness; the result includes the surrounding double
/// quotes.
fn json_escape(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// `GET {base_path}/` — HTML shell.
pub async fn shell(State(state): State<PlaygroundState>) -> Response<Body> {
    let html = render_shell(&state);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(html))
        .unwrap()
}

/// Compose both routes into a router.
///
/// - Shell: handled inline (template substitution, not a static
///   file) at `<base>/`.
/// - Assets: delegated to `StaticPlugin::embedded` at
///   `<base>/assets/*`. Routes are absolute because the plugin
///   contract merges plugin routers flat into the app router
///   without auto-prefixing.
///
/// Cache headers: one year + immutable. Safe with vite's content-
/// hashed filenames since a changed bundle gets a new hash, so a
/// cached `index-X.css` will always be exactly these bytes. In
/// `Environment::Dev` umbra-static forces max-age=0 automatically.
pub fn router(state: PlaygroundState) -> axum::Router {
    use axum::routing::get;
    use std::time::Duration;

    // Snapshot the base path before we consume `state` via
    // `.with_state(...)`. The asset mount has to be built afterwards
    // so we need an owned copy that survives the move.
    let base_trimmed = state.base_path.trim_end_matches('/').to_string();

    let shell_router = axum::Router::new()
        .route(&format!("{base_trimmed}/"), get(shell))
        .with_state(state);

    let assets_router = StaticPlugin::embedded(format!("{base_trimmed}/assets"), &ASSETS)
        .max_age(Duration::from_secs(31_536_000))
        .routes();

    shell_router.merge(assets_router)
}
