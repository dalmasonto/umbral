//! Integration test: the PlaygroundPlugin serves a 200 HTML shell
//! at its base path and 404s on unknown asset paths.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbra::prelude::Plugin;
use umbra_playground::PlaygroundPlugin;

#[tokio::test]
async fn shell_returns_200_html() {
    let plugin = PlaygroundPlugin::new();
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

#[tokio::test]
async fn missing_asset_returns_404() {
    let plugin = PlaygroundPlugin::new();
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();
    let app = plugin.routes();

    let req = Request::builder()
        .uri(format!("/{base}/assets/does-not-exist.js"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// Regression test for the routes/static_serve mismatch the vite
/// migration introduced. Vite emits its entry chunks under
/// `dist/assets/`, but the asset handler used to strip the `assets/`
/// segment from the URL before resolving — so the request for
/// `/{base}/assets/index-<hash>.css` looked up `dist/index-<hash>.css`
/// (wrong directory) and 404ed. This test fetches the *actual*
/// CSS bundle that vite emitted and asserts a 200 with `text/css`.
///
/// Skips silently when the bundle hasn't been built (CI without npm).
/// The shell + 404 tests above already cover that mode.
#[tokio::test]
async fn vite_emitted_css_resolves_to_200() {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let assets_dir = manifest_dir.join("dist").join("assets");
    let Ok(read) = std::fs::read_dir(&assets_dir) else {
        eprintln!(
            "skipping: dist/assets not populated; build.rs likely fell back \
             to placeholder (no npm / no node_modules)"
        );
        return;
    };
    let css_name = read
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("index-") && n.ends_with(".css"));
    let Some(css_name) = css_name else {
        eprintln!("skipping: no index-*.css in dist/assets — vite output shape changed?");
        return;
    };

    let plugin = PlaygroundPlugin::new();
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();
    let app = plugin.routes();

    let req = Request::builder()
        .uri(format!("/{base}/assets/{css_name}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "expected the vite-built CSS at /{base}/assets/{css_name} to resolve; \
         this is the regression the dist/assets path mismatch introduced",
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
