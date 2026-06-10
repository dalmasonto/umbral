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

        // dir_b: user_greeting.html — exercises the `request.user`-style
        // ambient injection from the `CURRENT_USER` task-local.
        write_template(
            &dir_b,
            "user_greeting.html",
            // The `is defined` guard makes this template safe to render
            // whether or not the session layer (which scopes the
            // `CURRENT_USER` task-local) is installed. With the layer
            // active, `user` always has at least `is_authenticated:
            // false`; without it, the guard short-circuits to "anon".
            "{% if user is defined and user.is_authenticated %}hi {{ user.username }}{% else %}anon{% endif %}",
        );

        // gaps2 #19 follow-up — pin that `None` / `Undefined`
        // renders as empty string, not the literal "none" /
        // "undefined" tokens MiniJinja defaults to. Bug screenshot
        // 2026-06-10 01-08-30: an Option<String>::None on a fresh
        // form rendered `value="none"` in the HTML, breaking the
        // form's repopulation flow on visit.
        write_template(
            &dir_b,
            "none_renders_empty.html",
            r#"<input value="{{ phone }}" type="tel">"#,
        );

        // gaps2 #21 — `img` filter templates. One minimal, one with
        // every kwarg set, one with an alt-text that needs HTML
        // escaping. The filter output goes through `from_safe_string`
        // so autoescape doesn't double-escape the wrapping `<`/`>`,
        // but attribute values themselves DO get escaped — the
        // hostile-alt case pins that.
        write_template(
            &dir_b,
            "img_minimal.html",
            "{{ url | img }}",
        );
        write_template(
            &dir_b,
            "img_full.html",
            r#"{{ url | img(alt="A product", width=400, height=300, class="rounded-md") }}"#,
        );
        write_template(
            &dir_b,
            "img_hostile_alt.html",
            r#"{{ url | img(alt=alt_text) }}"#,
        );

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

// =========================================================================
// Test 3 — ambient user merge (Django's `request.user` shape)
//
// The session-aware layer in `umbra-sessions` calls `with_current_user`
// to scope the per-request user value. `render` reads the task-local
// and merges into ctx under key `user`, but only when:
//   a) the layer scope was entered (otherwise the task-local read errors
//      out and the merge is skipped — backwards compat for handlers
//      that don't opt in), and
//   b) the caller didn't already supply `user` themselves (explicit ctx
//      always wins over the ambient default).
// =========================================================================

#[tokio::test]
async fn ambient_user_renders_when_layer_scoped_value_is_set() {
    boot();

    let user = minijinja::Value::from_serialize(&serde_json::json!({
        "username": "alice",
        "is_authenticated": true,
    }));

    let output = templates::with_current_user(Some(user), async {
        templates::render("user_greeting.html", &minijinja::context! {})
            .expect("render should succeed")
    })
    .await;

    assert_eq!(output, "hi alice");
}

#[tokio::test]
async fn ambient_user_falls_back_to_anonymous_branch_outside_layer_scope() {
    boot();

    // No `with_current_user` wrapper — the task-local is unset, the
    // merge is skipped, and `user` resolves to undefined which minijinja
    // treats as falsy in `{% if user.is_authenticated %}`.
    let output = templates::render("user_greeting.html", &minijinja::context! {})
        .expect("render should succeed");

    assert_eq!(output, "anon");
}

#[tokio::test]
async fn explicit_ctx_user_wins_over_ambient_layer_value() {
    boot();

    let ambient = minijinja::Value::from_serialize(&serde_json::json!({
        "username": "alice",
        "is_authenticated": true,
    }));
    // Same template, but the handler hands its own `user` via ctx. The
    // ambient value must NOT clobber it — explicit ctx is authoritative.
    let explicit_ctx = minijinja::context! {
        user => serde_json::json!({ "username": "bob", "is_authenticated": true }),
    };

    let output = templates::with_current_user(Some(ambient), async {
        templates::render("user_greeting.html", &explicit_ctx).expect("render should succeed")
    })
    .await;

    assert_eq!(output, "hi bob");
}

// =========================================================================
// gaps2 #21 — `img` filter
//
// Pins the four contracts the filter promises:
//   1. minimal call emits loading/decoding + empty-alt default,
//   2. kwargs flow through (alt, width, height, class),
//   3. attribute values get escaped (hostile alt-text can't break out),
//   4. the wrapper `<img>` is marked safe (autoescape doesn't
//      double-escape its angle brackets).
// =========================================================================

// =========================================================================
// gaps2 #19 follow-up — None / Undefined formatter
//
// MiniJinja's default formatter renders `Value::None` as the literal
// "none" and `Undefined` as "undefined". The framework overrides the
// formatter so both render as empty string, which is the right
// default for HTML form values (`value=""` vs `value="none"`) and
// for missing context keys in general.
// =========================================================================

#[test]
fn none_value_renders_as_empty_string_in_attribute_context() {
    boot();
    // Optional field set to None — pre-fix this rendered `value="none"`,
    // which the user then had to manually clear before typing.
    let ctx = minijinja::context! { phone => Option::<String>::None };
    let html = templates::render("none_renders_empty.html", &ctx).expect("render");
    assert_eq!(
        html, r#"<input value="" type="tel">"#,
        "None should render as empty string, not the literal `none`: {html}",
    );
}

#[test]
fn undefined_value_renders_as_empty_string_in_attribute_context() {
    boot();
    // No `phone` key in the context at all — pre-fix this rendered
    // `value="undefined"` (or crashed depending on strict-undefined).
    let html =
        templates::render("none_renders_empty.html", &minijinja::context! {}).expect("render");
    assert_eq!(
        html, r#"<input value="" type="tel">"#,
        "Undefined should render as empty, not the literal `undefined`: {html}",
    );
}

#[test]
fn explicit_string_values_still_render_unchanged() {
    boot();
    // Make sure the formatter override didn't break the happy path:
    // an actual `Some("555-1234")` still renders verbatim.
    let ctx = minijinja::context! { phone => "555-1234" };
    let html = templates::render("none_renders_empty.html", &ctx).expect("render");
    assert_eq!(html, r#"<input value="555-1234" type="tel">"#);
}

#[test]
fn img_filter_minimal_call_emits_perf_attributes_with_empty_alt() {
    boot();
    let ctx = minijinja::context! { url => "/static/products/cup.jpg" };
    let html = templates::render("img_minimal.html", &ctx).expect("render");

    // No double-escape of the wrapper tag — the filter's output is
    // marked safe.
    assert!(
        html.starts_with("<img "),
        "filter output must be raw HTML, not autoescaped; got: {html:?}"
    );
    assert!(html.contains(r#"src="/static/products/cup.jpg""#));
    // Empty alt is intentional — screen readers treat it as
    // decorative-image; better than the markup not having alt at all
    // (which would cause some readers to read the URL).
    assert!(
        html.contains(r#"alt="""#),
        "minimal call sets alt=\"\": {html}"
    );
    assert!(
        html.contains(r#"loading="lazy""#),
        "loading=lazy must be on every img: {html}"
    );
    assert!(
        html.contains(r#"decoding="async""#),
        "decoding=async must be on every img: {html}"
    );
    // No width/height/class on the minimal call.
    assert!(!html.contains("width="), "minimal has no width: {html}");
    assert!(!html.contains("height="), "minimal has no height: {html}");
    assert!(!html.contains("class="), "minimal has no class: {html}");
}

#[test]
fn img_filter_full_kwargs_flow_through() {
    boot();
    let ctx = minijinja::context! { url => "/static/p/hero.jpg" };
    let html = templates::render("img_full.html", &ctx).expect("render");

    assert!(html.contains(r#"src="/static/p/hero.jpg""#));
    assert!(html.contains(r#"alt="A product""#));
    assert!(
        html.contains(r#"width="400""#),
        "width passed through: {html}"
    );
    assert!(
        html.contains(r#"height="300""#),
        "height passed through: {html}"
    );
    assert!(
        html.contains(r#"class="rounded-md""#),
        "class passed through: {html}"
    );
    assert!(html.contains(r#"loading="lazy""#));
    assert!(html.contains(r#"decoding="async""#));
}

#[test]
fn img_filter_escapes_attribute_values_against_quote_breakout() {
    boot();
    // A hostile alt-text trying to break out of the attribute quote
    // and inject an event handler. The filter MUST escape the quote
    // and angle brackets so the rendered tag stays well-formed and
    // the payload lands as visible text instead of executing.
    let ctx = minijinja::context! {
        url => "/static/x.jpg",
        alt_text => r#"" onerror="alert(1)"#,
    };
    let html = templates::render("img_hostile_alt.html", &ctx).expect("render");

    assert!(
        !html.contains(r#""alert(1)""#),
        "raw quote must not appear unescaped in attribute value: {html}"
    );
    assert!(
        html.contains("&quot;"),
        "double quote must escape to &quot;: {html}"
    );
    // Tag well-formed: `<img ...>` with no extra `>` from a hostile
    // payload (an unescaped `>` would close the tag early).
    let close_count = html.matches('>').count();
    assert_eq!(
        close_count, 1,
        "exactly one `>` (the tag's own close): {html}"
    );
}
