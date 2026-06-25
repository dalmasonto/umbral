//! One route: the HTML shell.
//!
//! The shell is one HTML template substituted with the hashed asset
//! filenames and lives here. The assets themselves are served by the
//! framework's unified static pipeline (see [`umbral::static_files`] /
//! [`umbral::prelude::StaticDir`]): the plugin's
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

/// Shared state carried through middleware: the base path (e.g.
/// `/api/playground`) and a flag for whether we're in placeholder mode.
#[derive(Clone, Debug)]
pub struct PlaygroundState {
    pub base_path: Arc<str>,
    /// The resolved `<static_url>playground/assets` prefix the shell's
    /// `<script>` / `<link>` tags point at, snapshotted from the
    /// configured `settings.static_url` when the router is built (see
    /// [`PlaygroundPlugin::routes`](crate::PlaygroundPlugin)). Carrying
    /// the resolved value here — rather than reading the ambient setting
    /// inside [`render_shell`] — keeps rendering deterministic and lets a
    /// deploy's `STATIC_URL` override / CDN origin flow through. Closes
    /// gaps2 #53.
    pub asset_prefix: Arc<str>,
    /// Per-app scope used by the frontend to namespace browser-side
    /// storage. Closes gap #71. Injected into the rendered shell
    /// HTML as both a `<meta>` tag (for non-JS introspection) and
    /// the global `window.__UMBRAL_PLAYGROUND_APP__` for the
    /// state-store helpers to read at boot.
    pub app_name: Arc<str>,
    pub degraded: bool,
}

impl PlaygroundState {
    pub fn new(
        base_path: impl Into<String>,
        app_name: impl Into<String>,
        degraded: bool,
        asset_prefix: impl Into<String>,
    ) -> Self {
        Self {
            base_path: Arc::from(base_path.into()),
            app_name: Arc::from(app_name.into()),
            degraded,
            asset_prefix: Arc::from(asset_prefix.into()),
        }
    }
}

const SHELL_HTML: &str = include_str!("shell.html");

/// Render the HTML shell, inserting the hashed asset paths and the
/// per-app scope. The app name is HTML-attribute-escaped (quotes +
/// `<>` + `&`) so an exotic project slug can't break out of the
/// meta-tag attribute or the inline script.
pub fn render_shell(state: &PlaygroundState) -> String {
    if state.degraded {
        return PLACEHOLDER_HTML.to_string();
    }
    let css = format!("{}/{}", state.asset_prefix, CSS);
    let js = format!("{}/{}", state.asset_prefix, JS);
    let app_meta = html_escape_attr(&state.app_name);
    let app_js = json_escape(&state.app_name);
    // Read the OpenAPI spec URL the user actually configured —
    // falls back to the historical default when OpenApiPlugin
    // isn't installed OR the registry isn't populated yet
    // (boot-time race, shouldn't happen in practice since
    // Plugin::routes() runs in dependency order and the
    // playground depends on the rest plugin which depends on
    // openapi being mounted alongside).
    let spec_url = umbral::routes::registered_openapi_spec_url().unwrap_or("/openapi/openapi.json");
    let spec_url_json = json_escape(spec_url);
    single_pass_replace(
        SHELL_HTML,
        &[
            ("__CSS_PATH__", css.as_str()),
            ("__JS_PATH__", js.as_str()),
            ("__APP_NAME_ATTR__", app_meta.as_str()),
            ("__APP_NAME_JSON__", app_js.as_str()),
            ("__OPENAPI_URL_JSON__", spec_url_json.as_str()),
        ],
    )
}

/// Replace every `(token, value)` pair in `template` in a single left-to-right
/// scan. At each position the template is checked against every token; the
/// first matching token is consumed and its value is emitted. The emitted
/// value is **never** rescanned, so a value that contains a token literal
/// cannot trigger a second substitution — closing the sequential-replace
/// injection path.
fn single_pass_replace(template: &str, replacements: &[(&str, &str)]) -> String {
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    'outer: while i < len {
        for &(token, value) in replacements {
            let tb = token.as_bytes();
            if bytes[i..].starts_with(tb) {
                out.push_str(value);
                i += tb.len();
                continue 'outer;
            }
        }
        // No token matched at position i — emit one byte verbatim.
        // SAFETY: we walk byte-by-byte and reassemble into String only
        // after the loop. The original template is valid UTF-8, and we
        // only skip ahead by `token.len()` (which is also a &str, hence
        // valid UTF-8 boundaries). Single-byte advances may land mid-codepoint
        // for multi-byte chars, so push via char to stay correct.
        let ch = template[i..].chars().next().expect("non-empty slice");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
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
/// `<script>` tag carries `window.__UMBRAL_PLAYGROUND_APP__ =
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
