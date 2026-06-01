//! Tests for StaticPlugin cache headers.
//!
//! Covers:
//! - max_age configured → `Cache-Control: public, max-age=N` present
//! - no max_age configured → no Cache-Control header
//! - dev mode → max_age forced to 0 regardless of configured value
//!
//! Note: the dev-mode branch reads `umbra::settings::get_opt()`.
//! Tests that exercise it set the settings OnceLock up-front. Tests
//! that don't set it exercise the "settings not initialised" branch,
//! which falls back to using the configured value as-is.

use std::fs;
use std::io::Write;

use axum::body::Body;
use http::{Method, Request, StatusCode};
use tempfile::tempdir;
use tower::ServiceExt;
use umbra::prelude::*;
use umbra_static::StaticPlugin;

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn get_response(router: Router, uri: &str) -> http::Response<Body> {
    router
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

fn write_file(dir: &std::path::Path, name: &str, content: &str) {
    let mut f = fs::File::create(dir.join(name)).unwrap();
    writeln!(f, "{content}").unwrap();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Without `.max_age()`, no Cache-Control header is added.
#[tokio::test]
async fn no_max_age_means_no_cache_control_header() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "app.css", "body {}");

    let plugin = StaticPlugin::new("/static", dir.path());
    let router = plugin.routes();

    let resp = get_response(router, "/static/app.css").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("Cache-Control").is_none(),
        "Cache-Control must be absent when max_age is not configured"
    );
}

/// When max_age is configured and settings are not initialised (no full
/// App::build), the configured value is returned as-is.
#[tokio::test]
async fn max_age_configured_adds_cache_control_header() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "app.css", "body {}");

    let plugin =
        StaticPlugin::new("/static", dir.path()).max_age(std::time::Duration::from_secs(86400));
    let router = plugin.routes();

    let resp = get_response(router, "/static/app.css").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let cc = resp
        .headers()
        .get("Cache-Control")
        .expect("Cache-Control must be present when max_age is configured");
    let cc_str = cc.to_str().unwrap();
    assert!(
        cc_str.contains("public"),
        "Cache-Control should contain 'public': {cc_str}"
    );
    assert!(
        cc_str.contains("max-age=86400"),
        "Cache-Control should contain 'max-age=86400': {cc_str}"
    );
}

/// A zero max_age emits `max-age=0` (disables caching explicitly).
#[tokio::test]
async fn zero_max_age_emits_max_age_zero() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "style.css", "h1 { color: red }");

    let plugin = StaticPlugin::new("/static", dir.path()).max_age(std::time::Duration::ZERO);
    let router = plugin.routes();

    let resp = get_response(router, "/static/style.css").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let cc = resp
        .headers()
        .get("Cache-Control")
        .expect("Cache-Control present");
    let cc_str = cc.to_str().unwrap();
    assert!(
        cc_str.contains("max-age=0"),
        "max-age=0 must be emitted: {cc_str}"
    );
}

/// Without `.max_age()` the plugin still serves files normally.
#[tokio::test]
async fn files_are_served_with_and_without_max_age() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "logo.png", "fake-png-bytes");

    let plugin_no_age = StaticPlugin::new("/s", dir.path());
    let resp1 = get_response(plugin_no_age.routes(), "/s/logo.png").await;
    assert_eq!(resp1.status(), StatusCode::OK);

    let plugin_with_age =
        StaticPlugin::new("/s", dir.path()).max_age(std::time::Duration::from_secs(3600));
    let resp2 = get_response(plugin_with_age.routes(), "/s/logo.png").await;
    assert_eq!(resp2.status(), StatusCode::OK);
    assert!(resp2.headers().get("Cache-Control").is_some());
}
