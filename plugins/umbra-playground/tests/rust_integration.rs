//! Integration test: the PlaygroundPlugin serves a 200 HTML shell
//! at its base path and 404s on unknown asset paths.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbra::prelude::Plugin;
use umbra_playground::PlaygroundPlugin;

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

#[tokio::test]
async fn missing_asset_returns_404() {
    let plugin = PlaygroundPlugin::new("test-app");
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
///
/// Note: this test runs under `cargo test`, which sets
/// `CARGO_MANIFEST_DIR` in the test binary's env. The runtime
/// equivalent (a deployed server binary) does NOT have that env var
/// set — `static_serve::resolve` now bakes the path in at compile
/// time via `env!()` for exactly that reason. See `runtime_cwd_does_not_affect_asset_resolution` below.
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

    let plugin = PlaygroundPlugin::new("test-app");
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

/// Regression test for the runtime CWD bug: the resolver used to read
/// `CARGO_MANIFEST_DIR` via `std::env::var` at request time. Cargo
/// only exports that env var during the build (and during `cargo test`),
/// so a deployed binary serving from any working directory other than
/// the crate root would 404 every asset. The bug only surfaced in
/// production because tests had the env var set for free.
///
/// We simulate the runtime by changing the test process's CWD to /tmp
/// AND unsetting `CARGO_MANIFEST_DIR` before the request. If the
/// resolver still works, it's because the manifest dir was baked at
/// compile time via `env!()`. If it 404s, it's reading runtime state
/// and we've regressed.
#[tokio::test]
async fn asset_resolves_even_when_cwd_and_env_dont_point_at_the_crate() {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let assets_dir = manifest_dir.join("dist").join("assets");
    let Ok(read) = std::fs::read_dir(&assets_dir) else {
        eprintln!("skipping: dist/assets not populated (no vite build)");
        return;
    };
    let css_name = read
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("index-") && n.ends_with(".css"));
    let Some(css_name) = css_name else {
        eprintln!("skipping: no index-*.css in dist/assets");
        return;
    };

    // Move CWD to /tmp and clear the env var. A correct resolver
    // should still find the asset because it baked the manifest dir
    // at compile time.
    //
    // SAFETY: tests run in the same process; this change leaks into
    // sibling tests if they assume a specific CWD. The other tests
    // in this file don't, but tokio::test spawns each test on the
    // same runtime — so we restore CWD before returning.
    let saved_cwd = std::env::current_dir().ok();
    std::env::set_current_dir("/tmp").expect("chdir /tmp");
    // SAFETY: setting env vars from a test thread can race with
    // anything reading env in another thread. There's no such reader
    // here (the resolver is the only consumer and it's compile-time
    // baked now), so this is the minimal surface that still exercises
    // the regression.
    unsafe {
        std::env::remove_var("CARGO_MANIFEST_DIR");
    }

    let plugin = PlaygroundPlugin::new("test-app");
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
    let status = res.status();

    // Restore CWD regardless of test outcome.
    if let Some(cwd) = saved_cwd {
        let _ = std::env::set_current_dir(cwd);
    }

    assert_eq!(
        status,
        StatusCode::OK,
        "asset resolution must not depend on runtime CWD or env vars; \
         the resolver should bake CARGO_MANIFEST_DIR at compile time",
    );
}

/// Regression test for "we need them all" — every file in
/// `dist/assets/`, not just the index JS + CSS entry chunks, must
/// resolve through the mount. Vite emits the woff2 fonts the Inter
/// CSS references into `dist/assets/inter-*.woff2`; if any of them
/// 404 the browser will load the page bare-CSS-no-font, which is
/// exactly the bug the user surfaced.
///
/// Iterates every file in `dist/assets/`, fetches each through the
/// router, and asserts a 200. Skips silently when dist isn't built.
#[tokio::test]
async fn every_file_in_dist_assets_resolves_through_the_mount() {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let assets_dir = manifest_dir.join("dist").join("assets");
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
    let base = plugin
        .base_path_for_test()
        .trim_start_matches('/')
        .to_string();

    let mut failures: Vec<String> = Vec::new();
    for name in &names {
        // Build a fresh router per request: axum's oneshot consumes
        // the service, so we can't reuse one across iterations.
        let app = plugin.routes();
        let req = Request::builder()
            .uri(format!("/{base}/assets/{name}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        if res.status() != StatusCode::OK {
            failures.push(format!("{name}: {}", res.status()));
        }
    }
    assert!(
        failures.is_empty(),
        "every file in dist/assets/ must resolve through {base}/assets/; got {} failure(s): {:#?}\nall files: {:#?}",
        failures.len(),
        failures,
        names,
    );
}

/// Gap #71: the rendered shell HTML carries the configured app
/// name in two places — a `<meta name="umbra-playground-app">`
/// tag (for non-JS introspection / scrapers) and the
/// `window.__UMBRA_PLAYGROUND_APP__` global the frontend reads at
/// boot to namespace storage keys. Both must round-trip the exact
/// string the caller passed, and both must escape correctly when
/// the name contains characters that are dangerous in HTML
/// attributes or JS strings.
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

/// Defensive: an app name containing a double quote, an
/// ampersand, or a `<` must escape into the attribute + the
/// inline-script JSON string without breaking out. Production code
/// will rarely hit this — a normal project slug doesn't have
/// these chars — but a careless `PlaygroundPlugin::new(&user_supplied_string)`
/// could.
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
    // Attribute: " -> &quot;, & -> &amp;, < -> &lt;, > -> &gt;
    assert!(
        s.contains("content=\"my&quot;shop &amp; &lt;test&gt;\""),
        "attribute must HTML-escape the unsafe chars; got: {s}"
    );
    // JS: " -> \", every other char preserved verbatim. serde_json
    // wraps the result in surrounding double-quotes.
    assert!(
        s.contains(r#"window.__UMBRA_PLAYGROUND_APP__ = "my\"shop & <test>";"#),
        "window assignment must JSON-escape the unsafe chars; got: {s}"
    );
}
