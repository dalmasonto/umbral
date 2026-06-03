//! Two routes: the HTML shell and the bundled assets.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Response, StatusCode, header};

use crate::static_serve::{content_type, resolve};
use crate::{CSS, JS, PLACEHOLDER_HTML};

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

/// `GET {base_path}/assets/*` — bundled assets. Path-traversal safe.
pub async fn assets(State(state): State<PlaygroundState>, req: Request) -> Response<Body> {
    let path = req.uri().path();
    let prefix = format!("{}/assets/", state.base_path);
    let rel = match path.strip_prefix(&prefix) {
        Some(r) => r,
        None => return not_found(),
    };
    let resolved = match resolve(rel) {
        Some(p) => p,
        None => return not_found(),
    };
    let bytes = match std::fs::read(&resolved) {
        Ok(b) => b,
        Err(_) => return not_found(),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type(&resolved))
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(bytes))
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
    let base_trimmed = base.trim_end_matches('/');
    axum::Router::new()
        .route(&format!("{base_trimmed}/"), get(shell))
        .route(&format!("{base_trimmed}/assets/{{*path}}"), get(assets))
        .with_state(state)
}
