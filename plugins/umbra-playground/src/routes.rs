//! Two routes: the HTML shell and the bundled assets.
//!
//! The asset half is delegated to [`umbra_static::StaticPlugin`] so
//! we don't re-implement (and re-bug) path traversal handling, MIME
//! sniffing, range requests, ETag, etc. The shell half is one
//! template-substituted HTML response and lives here.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use umbra::prelude::Plugin;
use umbra_static::StaticPlugin;

use crate::{CSS, JS, PLACEHOLDER_HTML};

/// Compile-time path to the crate's `dist/` directory. Baked in
/// because `CARGO_MANIFEST_DIR` is only available during the build
/// phase; at runtime `std::env::var("CARGO_MANIFEST_DIR")` returns
/// `Err`, which is why the runtime asset serving used to 404 every
/// request before fix `e73d6db`.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

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

/// Compose both routes into a router. The shell route is registered
/// directly; the asset route is contributed by an internal
/// [`StaticPlugin`] instance mounted at `<base>/assets`. Routes are
/// absolute (e.g. `/api/playground/` and `/api/playground/assets/...`)
/// because the plugin contract merges plugin routers flat into the
/// app router without auto-prefixing.
pub fn router(state: PlaygroundState) -> axum::Router {
    use axum::routing::get;

    // Snapshot the base path before we consume `state` via
    // `.with_state(...)`. The asset mount has to be built afterwards
    // so we need an owned copy that survives the move.
    let base_trimmed = state.base_path.trim_end_matches('/').to_string();

    // The shell route — one HTML response with substituted asset paths.
    let shell_router = axum::Router::new()
        .route(&format!("{base_trimmed}/"), get(shell))
        .with_state(state);

    // The asset routes — every file under `<crate>/dist/assets/`
    // exposed at `<base>/assets/*`. StaticPlugin handles the
    // path-traversal-safe resolution, the MIME type, and the cache
    // headers; we just feed it the mount path and the directory.
    //
    // `max_age` is a year because vite emits hashed filenames — a
    // changed bundle gets a new hash, so the URL itself busts the
    // cache. In Environment::Dev StaticPlugin forces this to 0
    // automatically (see umbra_static::StaticPlugin docs).
    let assets_dir = PathBuf::from(MANIFEST_DIR).join("dist").join("assets");
    let assets_mount = format!("{base_trimmed}/assets");
    let assets_router = StaticPlugin::new(assets_mount, assets_dir)
        .max_age(std::time::Duration::from_secs(31_536_000))
        .routes();

    shell_router.merge(assets_router)
}
