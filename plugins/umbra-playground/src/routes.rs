//! One route: the HTML shell.
//!
//! The shell is one HTML template substituted with the hashed asset
//! filenames and lives here. The assets themselves are served by the
//! framework's unified static pipeline (see [`umbra::static_files`] /
//! [`umbra::prelude::StaticDir`]): the plugin's
//! [`PlaygroundPlugin::static_dirs`](crate::PlaygroundPlugin::static_dirs)
//! registers `dist/` under the `playground` namespace, so a request for
//! `<static_url>playground/assets/<hashed-file>` resolves to
//! `dist/assets/<hashed-file>` — live off disk in dev, from the
//! collected `static_root` in prod. The shell's `<script>` / `<link>`
//! URLs are built to point at that pipeline path.
//!
//! Dogfooding the pipeline means dropping a freshly-built bundle into
//! `dist/` is served on the next request in dev with NO Rust recompile —
//! the property the user asked for.
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};

use crate::{CSS, JS, PLACEHOLDER_HTML};

/// URL prefix the shell points its `<script>` / `<link>` tags at.
///
/// This is `<static_url>playground/assets/` with the framework's
/// default `static_url` (`/static/`) hardcoded. The static pipeline
/// resolves `/static/playground/assets/<name>` → `dist/assets/<name>`
/// via the `playground` namespace registered in `static_dirs()`.
///
/// TODO(gaps2 #53): this hardcodes `/static/` instead of reading the
/// configured `settings.static_url`. `Plugin::routes()` (where the
/// shell state is built) gets no `Settings` handle, so the configured
/// prefix isn't reachable here. If a deploy overrides `static_url`, the
/// shell's asset URLs won't follow. Proper fix lives in
/// `plugins/umbra-playground/src/{lib.rs,routes.rs}`: thread the
/// resolved `static_url` into `PlaygroundState` (e.g. via an
/// `on_ready` snapshot or a builder field) and build this prefix from
/// it.
const STATIC_ASSET_PREFIX: &str = "/static/playground/assets";

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
    pub fn new(base_path: impl Into<String>, app_name: impl Into<String>, degraded: bool) -> Self {
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
    let css = format!("{STATIC_ASSET_PREFIX}/{CSS}");
    let js = format!("{STATIC_ASSET_PREFIX}/{JS}");
    let app_meta = html_escape_attr(&state.app_name);
    let app_js = json_escape(&state.app_name);
    // Read the OpenAPI spec URL the user actually configured —
    // falls back to the historical default when OpenApiPlugin
    // isn't installed OR the registry isn't populated yet
    // (boot-time race, shouldn't happen in practice since
    // Plugin::routes() runs in dependency order and the
    // playground depends on the rest plugin which depends on
    // openapi being mounted alongside).
    let spec_url = umbra::routes::registered_openapi_spec_url().unwrap_or("/openapi/openapi.json");
    let spec_url_json = json_escape(spec_url);
    SHELL_HTML
        .replace("__CSS_PATH__", &css)
        .replace("__JS_PATH__", &js)
        .replace("__APP_NAME_ATTR__", &app_meta)
        .replace("__APP_NAME_JSON__", &app_js)
        .replace("__OPENAPI_URL_JSON__", &spec_url_json)
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

/// Compose the plugin's routes into a router.
///
/// Only the shell route lives here now: handled inline (template
/// substitution, not a static file) at `<base>/`. The asset files are
/// served by the framework's unified static pipeline at
/// `<static_url>playground/assets/*` (registered via
/// [`PlaygroundPlugin::static_dirs`](crate::PlaygroundPlugin::static_dirs)),
/// NOT by a route mounted here. The route path is absolute because the
/// plugin contract merges plugin routers flat into the app router
/// without auto-prefixing.
pub fn router(state: PlaygroundState) -> axum::Router {
    use axum::routing::get;

    let base_trimmed = state.base_path.trim_end_matches('/').to_string();

    axum::Router::new()
        .route(&format!("{base_trimmed}/"), get(shell))
        .with_state(state)
}
