//! Two routes: the HTML shell and the bundled assets.
//!
//! Both are served entirely from memory. The shell is one HTML
//! template substituted with the hashed asset filenames; the assets
//! come from the compile-time-embedded `ASSETS` Dir
//! (see [`crate::ASSETS`]). No filesystem reads happen at request
//! time — the binary is the asset store. That's both a fix for the
//! "rebuild wipes dist/, browser 404s on old hash names" footgun
//! and the architectural shape an embeddable plugin should have:
//! its UI ships *with it*.
//!
//! Path-traversal is structurally impossible — the lookup is a
//! `Dir::get_file(rel_path)` against an in-memory tree, not a path
//! join. A `..` segment in the URL becomes a no-match.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Response, StatusCode, header};

use crate::{ASSETS, CSS, JS, PLACEHOLDER_HTML};

/// Shared state carried through middleware: the base path (e.g.
/// `/api/playground`) and a flag for whether we're in placeholder mode.
#[derive(Clone, Debug)]
pub struct PlaygroundState {
    pub base_path: Arc<str>,
    pub degraded: bool,
}

impl PlaygroundState {
    pub fn new(base_path: impl Into<String>, degraded: bool) -> Self {
        Self {
            base_path: Arc::from(base_path.into()),
            degraded,
        }
    }
}

const SHELL_HTML: &str = include_str!("shell.html");

/// Render the HTML shell, inserting the hashed asset paths.
fn render_shell(state: &PlaygroundState) -> String {
    if state.degraded {
        return PLACEHOLDER_HTML.to_string();
    }
    let css = format!("{}/assets/{}", state.base_path, CSS);
    let js = format!("{}/assets/{}", state.base_path, JS);
    SHELL_HTML
        .replace("__CSS_PATH__", &css)
        .replace("__JS_PATH__", &js)
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

/// `GET {base_path}/assets/{*path}` — every entry in the embedded
/// asset tree, looked up by name.
///
/// Vite emits its entry chunks (and the woff2 fonts the CSS
/// references) under `dist/assets/`, which is exactly what
/// `include_dir!` baked into [`ASSETS`]. The handler strips
/// `{base}/assets/` from the URL and passes the remainder to
/// `Dir::get_file`. MIME type comes from `mime_guess` against the
/// extension; fall back to `application/octet-stream`.
///
/// Cache headers: a year + `immutable`. Safe because vite's
/// content-hashed filenames change whenever the underlying bytes
/// change — a cached `index-X.css` will never refer to anything
/// other than these exact bytes. Browsers can hold it forever.
pub async fn assets(State(state): State<PlaygroundState>, req: Request) -> Response<Body> {
    let path = req.uri().path();
    let prefix = format!("{}/assets/", state.base_path);
    let rel = match path.strip_prefix(&prefix) {
        Some(r) => r,
        None => return not_found(),
    };
    let file = match ASSETS.get_file(rel) {
        Some(f) => f,
        None => return not_found(),
    };

    let content_type = mime_guess::from_path(rel)
        .first_or_octet_stream()
        .to_string();

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(file.contents()))
        .unwrap()
}

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("not found"))
        .unwrap()
}

/// Compose both routes into a router. Routes are absolute
/// (e.g. `/api/playground/` and `/api/playground/assets/{*path}`)
/// because the plugin contract merges plugin routers flat into
/// the app router without auto-prefixing.
pub fn router(state: PlaygroundState) -> axum::Router {
    use axum::routing::get;

    let base = state.base_path.as_ref();
    let base_trimmed = base.trim_end_matches('/').to_string();

    axum::Router::new()
        .route(&format!("{base_trimmed}/"), get(shell))
        .route(&format!("{base_trimmed}/assets/{{*path}}"), get(assets))
        .with_state(state)
}
