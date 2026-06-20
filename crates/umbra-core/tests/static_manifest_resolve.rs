//! `resolve_static_url` honours the hashed-asset manifest (gaps2 #82).
//!
//! The manifest lives in a process-global `OnceLock`, so this test owns
//! its own test binary (one integration-test file = one process) to set
//! the manifest exactly once and assert the resolver returns the hashed
//! URL for a mapped path and the plain URL for an unmapped one.

use std::collections::HashMap;

use umbra_core::static_files::{manifest_loaded, set_manifest_for_tests};
use umbra_core::templates::resolve_static_url;

#[test]
fn resolve_static_url_returns_hashed_url_when_manifest_loaded() {
    // Before any manifest is set, the resolver returns the plain URL.
    assert!(!manifest_loaded());
    assert_eq!(resolve_static_url("css/app.css"), "/static/css/app.css");

    // Install a manifest: css/app.css -> css/app.<hash>.css.
    let mut manifest = HashMap::new();
    manifest.insert(
        "css/app.css".to_string(),
        "css/app.abc123def456.css".to_string(),
    );
    set_manifest_for_tests(Some(manifest));
    assert!(manifest_loaded());

    // A mapped path resolves to its hashed URL.
    assert_eq!(
        resolve_static_url("css/app.css"),
        "/static/css/app.abc123def456.css"
    );
    // A leading slash hits the same entry (key normalisation).
    assert_eq!(
        resolve_static_url("/css/app.css"),
        "/static/css/app.abc123def456.css"
    );
    // An UNmapped path (not collected through --hashed) falls back to the
    // plain URL.
    assert_eq!(resolve_static_url("js/other.js"), "/static/js/other.js");
}
