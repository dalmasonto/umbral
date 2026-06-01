//! Tests for cross-plugin template discovery.
//!
//! The template engine searches an ordered list of directories (app-level
//! first, then plugins in registration order). `templates::init` publishes
//! to a process-global `OnceLock`, so this file calls it exactly once and
//! then runs all assertions against the resulting engine state.
//!
//! ## Test scenarios
//!
//! 1. Cross-plugin extends: plugin A has a child template that `{% extends %}`
//!    a base template from plugin B. Rendering A's template must resolve B's
//!    base automatically because both directories are in the search list.
//!
//! 2. Collision detection: when two directories ship a template with the same
//!    name, the first-registered copy wins and `init` returns the colliding
//!    name in its returned `Vec<String>`.
//!
//! Both use `tempfile::TempDir` for isolated, reproducible directory layouts.
//! A `std::sync::OnceLock<()>` guards the single `templates::init` call so
//! all `#[test]` functions can run in any order.

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use tempfile::TempDir;
use umbra_core::templates;

// =========================================================================
// Shared boot: call templates::init exactly once for this test binary.
//
// Layout:
//   dir_a/  extends.html          — extends base.html (cross-plugin)
//   dir_b/  base.html             — the base (resolved from dir_b by dir_a's extends)
//   dir_b/  conflict.html         — "from_dir_b" (first registration)
//   dir_c/  conflict.html         — "from_dir_c" (duplicate, should be skipped)
//
// Search order: [dir_a, dir_b, dir_c]
// =========================================================================

/// Temp dirs that must stay alive for the duration of all tests.
/// Wrapped in a `OnceLock` so they're created once and never dropped.
static DIRS: OnceLock<(TempDir, TempDir, TempDir)> = OnceLock::new();

/// Collision list returned by `templates::init`. Populated once.
static COLLISIONS: OnceLock<Vec<String>> = OnceLock::new();

fn write_template(base: &TempDir, relative_path: &str, content: &str) {
    let full = base.path().join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(&full, content).expect("write template file");
}

fn boot() {
    let dirs = DIRS.get_or_init(|| {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let dir_c = TempDir::new().unwrap();

        // dir_b: base.html (the parent template) + conflict.html (first)
        write_template(
            &dir_b,
            "base.html",
            "<!doctype html><html><body>{% block content %}{% endblock %}</body></html>",
        );
        write_template(&dir_b, "conflict.html", "from_dir_b");

        // dir_a: extends.html which cross-extends base.html from dir_b
        write_template(
            &dir_a,
            "extends.html",
            r#"{% extends "base.html" %}{% block content %}hello from dir_a{% endblock %}"#,
        );

        // dir_c: conflict.html — duplicate, should be shadowed by dir_b's copy
        write_template(&dir_c, "conflict.html", "from_dir_c");

        (dir_a, dir_b, dir_c)
    });

    COLLISIONS.get_or_init(|| {
        let dirs_vec: Vec<PathBuf> = vec![
            dirs.0.path().to_path_buf(), // dir_a first
            dirs.1.path().to_path_buf(), // dir_b second
            dirs.2.path().to_path_buf(), // dir_c third
        ];
        templates::init(&dirs_vec).expect("templates::init should succeed")
    });
}

// =========================================================================
// Test 1 — cross-plugin extends
//
// extends.html in dir_a extends base.html which lives in dir_b.
// Because both dirs are in the search list, the extends lookup finds
// base.html and the render produces the merged output.
// =========================================================================

#[test]
fn cross_plugin_extends_resolves_base_from_sibling_plugin_dir() {
    boot();

    let output =
        templates::render("extends.html", &minijinja::context! {}).expect("render should succeed");

    assert!(
        output.contains("hello from dir_a"),
        "rendered output should contain dir_a block content; got: {output:?}"
    );
    assert!(
        output.contains("<body>"),
        "rendered output should include dir_b base layout; got: {output:?}"
    );
}

// =========================================================================
// Test 2 — collision detection
//
// conflict.html appears in both dir_b (registration index 1) and dir_c
// (registration index 2). The first-registered copy (dir_b = "from_dir_b")
// must win, and init must return "conflict.html" in its collision list.
// =========================================================================

#[test]
fn collision_first_registered_wins_and_is_reported() {
    boot();

    // Assert the collision was detected.
    let collisions = COLLISIONS.get().expect("boot() sets COLLISIONS");
    assert!(
        collisions.contains(&"conflict.html".to_string()),
        "conflict.html should appear in the collision list; got: {collisions:?}"
    );

    // Assert the first-registered copy wins.
    let output =
        templates::render("conflict.html", &minijinja::context! {}).expect("render should succeed");
    assert_eq!(
        output.trim(),
        "from_dir_b",
        "dir_b (first registered) should win over dir_c (duplicate)"
    );
}
