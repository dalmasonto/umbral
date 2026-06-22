//! Tests for StoragePlugin (static side) cache headers. Moved from
//! umbra-static.

use std::fs;
use std::io::Write;

use axum::body::Body;
use http::{Method, Request, StatusCode};
use tempfile::tempdir;
use tower::ServiceExt;
use umbra::prelude::*;
use umbra_storage::StoragePlugin;

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

#[tokio::test]
async fn no_max_age_means_no_cache_control_header() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "app.css", "body {}");

    let plugin = StoragePlugin::new().static_files("/static", dir.path());
    let router = plugin.routes();

    let resp = get_response(router, "/static/app.css").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("Cache-Control").is_none(),
        "Cache-Control must be absent when max_age is not configured"
    );
}

#[tokio::test]
async fn max_age_configured_adds_cache_control_header() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "app.css", "body {}");

    let plugin = StoragePlugin::new()
        .static_files("/static", dir.path())
        .max_age(std::time::Duration::from_secs(86400));
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

#[tokio::test]
async fn zero_max_age_emits_max_age_zero() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "style.css", "h1 { color: red }");

    let plugin = StoragePlugin::new()
        .static_files("/static", dir.path())
        .max_age(std::time::Duration::ZERO);
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

#[tokio::test]
async fn files_are_served_with_and_without_max_age() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "logo.png", "fake-png-bytes");

    let plugin_no_age = StoragePlugin::new().static_files("/s", dir.path());
    let resp1 = get_response(plugin_no_age.routes(), "/s/logo.png").await;
    assert_eq!(resp1.status(), StatusCode::OK);

    let plugin_with_age = StoragePlugin::new()
        .static_files("/s", dir.path())
        .max_age(std::time::Duration::from_secs(3600));
    let resp2 = get_response(plugin_with_age.routes(), "/s/logo.png").await;
    assert_eq!(resp2.status(), StatusCode::OK);
    assert!(resp2.headers().get("Cache-Control").is_some());
}
