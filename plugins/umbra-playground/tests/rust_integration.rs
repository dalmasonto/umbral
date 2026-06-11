//! Integration tests for the PlaygroundPlugin after the migration onto
//! the framework's unified static pipeline.
//!
//! The plugin no longer serves its bundle out of `plugin.routes()`
//! (the old embedded `StaticPlugin::embedded` mount is gone). Instead:
//!
//! - `plugin.routes()` serves ONLY the HTML shell at `<base>/`.
//! - The vite bundle under `dist/assets/` is served by the framework's
//!   unified static handler at `<static_url>playground/assets/<file>`,
//!   keyed off the `playground` namespace the plugin registers via
//!   `Plugin::static_dirs()`.
//!
//! So the asset-serving tests now drive `umbra::static_files`'
//! `static_handler` with a registry built from the plugin (exactly how
//! `App::build` wires it), and the shell tests assert the rendered HTML
//! points its `<script>` / `<link>` at the `/static/playground/assets/`
//! pipeline prefix.

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbra::prelude::Plugin;
use umbra::static_files::{StaticHandlerState, StaticRegistry, static_handler};
use umbra_playground::PlaygroundPlugin;

/// Build the static-handler state the way `App::build` does for a dev
/// app: a registry collected from the plugin's `static_dirs()`, an
/// (unused-here) `static_root`, and `dev = true` so the pipeline serves
/// live off the plugin's `dist/` source dir.
fn dev_static_state(plugin: &PlaygroundPlugin) -> StaticHandlerState {
    let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(plugin.clone())];
    let registry = StaticRegistry::from_plugins(&plugins).expect("no namespace collision");
    StaticHandlerState {
        registry,
        static_root: PathBuf::from("staticfiles"),
        dev: true,
    }
}

/// The first index-*.css name vite emitted under `dist/assets/`, if the
/// bundle is built. `None` when build.rs fell back to placeholder (no
/// npm / no node_modules in CI).
fn built_css_name() -> Option<String> {
    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("dist")
        .join("assets");
    std::fs::read_dir(&assets_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .find_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            (n.starts_with("index-") && n.ends_with(".css")).then_some(n)
        })
}

#[tokio::test]
async fn shell_returns_200_html() {
    let plugin = PlaygroundPlugin::new("test-app");
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();
    let app = plugin.routes();

    let req = Request::builder()
        .uri(format!("/{base}/"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let s = String::from_utf8_lossy(&body);
    assert!(
        s.contains("<!doctype html>"),
        "expected HTML shell, got: {s}"
    );
}

/// `static_dirs()` registers the `playground` namespace mapped to the
/// crate's `dist/` directory — the contract `App::build` reads to wire
/// the unified static handler. Asserts both the namespace and that the
/// source dir is the compile-time-baked `<crate>/dist`.
#[tokio::test]
async fn static_dirs_maps_playground_to_dist() {
    let plugin = PlaygroundPlugin::new("test-app");
    let dirs = plugin.static_dirs();
    assert_eq!(dirs.len(), 1, "exactly one static dir");
    let dir = &dirs[0];
    assert_eq!(dir.namespace, "playground");
    let expected = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dist");
    assert_eq!(dir.source_dir, expected, "source dir must be <crate>/dist");

    // And the registry App::build builds from it resolves the namespace.
    let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(plugin)];
    let registry = StaticRegistry::from_plugins(&plugins).expect("no collision");
    assert_eq!(registry.source_dir("playground"), Some(expected.as_path()));
}

/// The rendered shell points its `<link>` / `<script>` at the static
/// pipeline prefix `/static/playground/assets/<hashed-name>`, where the
/// hashed names still come from build.rs's `generated_assets.rs`. This
/// is the user-visible payoff of the migration: the URL prefix moved
/// from the old bespoke `<base>/assets/` to the pipeline path.
///
/// Skips the hashed-name half when the bundle wasn't built (placeholder
/// mode renders a different HTML with no asset tags); the prefix
/// assertion below still runs for the built case.
#[tokio::test]
async fn shell_points_assets_at_static_pipeline() {
    let Some(css_name) = built_css_name() else {
        eprintln!("skipping: dist/assets not built (placeholder mode)");
        return;
    };

    let plugin = PlaygroundPlugin::new("test-app");
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();
    let app = plugin.routes();
    let req = Request::builder()
        .uri(format!("/{base}/"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let s = String::from_utf8_lossy(&body);

    // The CSS link points at the pipeline prefix + the hashed name.
    let css_url = format!("/static/playground/assets/{css_name}");
    assert!(
        s.contains(&css_url),
        "shell must link the CSS at the static pipeline URL {css_url}; got: {s}"
    );
    // The shell must NOT carry the old bespoke `<base>/assets/` prefix
    // any more — that route is gone.
    assert!(
        !s.contains(&format!("/{base}/assets/")),
        "shell must not reference the removed embedded asset route; got: {s}"
    );
    // A script tag pointing at the pipeline prefix exists too.
    assert!(
        s.contains("/static/playground/assets/") && s.contains("<script type=\"module\""),
        "shell must load the JS module from the static pipeline; got: {s}"
    );
}

/// The vite-built CSS bundle resolves to a 200 `text/css` THROUGH the
/// framework's unified static handler at
/// `/static/playground/assets/<hashed-css>` — proving the pipeline +
/// the plugin's `static_dirs()` wiring serve the real file. This is the
/// pipeline equivalent of the old embedded-mount regression test.
///
/// Skips silently when the bundle hasn't been built (CI without npm).
#[tokio::test]
async fn vite_css_resolves_through_static_pipeline() {
    let Some(css_name) = built_css_name() else {
        eprintln!("skipping: dist/assets not populated (no vite build)");
        return;
    };

    let plugin = PlaygroundPlugin::new("test-app");
    let state = dev_static_state(&plugin);

    // The handler is mounted at the static base; nest_service strips it,
    // so the handler sees the path relative to /static/ — i.e. starting
    // with the namespace.
    let req = Request::builder()
        .uri(format!("/playground/assets/{css_name}"))
        .body(Body::empty())
        .unwrap();
    let res = static_handler(State(state), req).await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "the vite CSS must resolve through the static pipeline at \
         /static/playground/assets/{css_name}",
    );
    let ctype = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ctype.starts_with("text/css"),
        "expected text/css content-type; got {ctype}",
    );
}

/// Every file in `dist/assets/` — entry chunks AND the Inter woff2
/// fonts the CSS references — resolves through the unified static
/// handler. A missing font would render the page bare-CSS-no-font, the
/// exact class of bug the user surfaced before. Skips when dist isn't
/// built.
#[tokio::test]
async fn every_dist_asset_resolves_through_static_pipeline() {
    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("dist")
        .join("assets");
    let Ok(read) = std::fs::read_dir(&assets_dir) else {
        eprintln!("skipping: dist/assets not populated");
        return;
    };
    let names: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if names.is_empty() {
        eprintln!("skipping: dist/assets is empty (placeholder build)");
        return;
    }

    let plugin = PlaygroundPlugin::new("test-app");
    let mut failures: Vec<String> = Vec::new();
    for name in &names {
        let state = dev_static_state(&plugin);
        let req = Request::builder()
            .uri(format!("/playground/assets/{name}"))
            .body(Body::empty())
            .unwrap();
        let res = static_handler(State(state), req).await;
        if res.status() != StatusCode::OK {
            failures.push(format!("{name}: {}", res.status()));
        }
    }
    assert!(
        failures.is_empty(),
        "every file in dist/assets/ must resolve through the static \
         pipeline at /static/playground/assets/; got {} failure(s): {:#?}",
        failures.len(),
        failures,
    );
}

/// A path that escapes the plugin's `dist/` source dir (or names a file
/// that doesn't exist) is a 404 through the pipeline — never a serve of
/// an unintended file.
#[tokio::test]
async fn missing_asset_returns_404_through_pipeline() {
    let plugin = PlaygroundPlugin::new("test-app");
    let state = dev_static_state(&plugin);
    let req = Request::builder()
        .uri("/playground/assets/does-not-exist.js")
        .body(Body::empty())
        .unwrap();
    let res = static_handler(State(state), req).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// Gap #71: the rendered shell HTML carries the configured app name in
/// two places — a `<meta name="umbra-playground-app">` tag and the
/// `window.__UMBRA_PLAYGROUND_APP__` global. Both must round-trip the
/// exact string the caller passed.
#[tokio::test]
async fn shell_injects_per_app_scope() {
    let plugin = PlaygroundPlugin::new("my-shop");
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();
    let app = plugin.routes();
    let req = Request::builder()
        .uri(format!("/{base}/"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let s = String::from_utf8_lossy(&body);
    assert!(
        s.contains(r#"<meta name="umbra-playground-app" content="my-shop" />"#),
        "shell must carry the app meta tag; got: {s}"
    );
    assert!(
        s.contains(r#"window.__UMBRA_PLAYGROUND_APP__ = "my-shop";"#),
        "shell must carry the app window global; got: {s}"
    );
}

/// Defensive: an app name with HTML/JS-dangerous chars must escape into
/// the attribute + the inline-script JSON string without breaking out.
#[tokio::test]
async fn shell_scope_escapes_dangerous_chars() {
    let plugin = PlaygroundPlugin::new(r#"my"shop & <test>"#);
    let app = plugin.routes();
    let req = Request::builder()
        .uri("/api/playground/")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let s = String::from_utf8_lossy(&body);
    assert!(
        s.contains("content=\"my&quot;shop &amp; &lt;test&gt;\""),
        "attribute must HTML-escape the unsafe chars; got: {s}"
    );
    assert!(
        s.contains(r#"window.__UMBRA_PLAYGROUND_APP__ = "my\"shop & <test>";"#),
        "window assignment must JSON-escape the unsafe chars; got: {s}"
    );
}
