//! audit_2 core-templates-forms #3 — `templates::render_str` (the inline
//! template helper, `#[doc(hidden)]` but `pub`/reachable) built a fresh
//! `Environment` with the default `AutoEscape::None`, so `{{ x }}` emitted
//! user data verbatim → SSTI/XSS foot-gun. It must HTML-escape by default like
//! every `.html` template the framework renders.

#[test]
fn render_str_html_escapes_interpolated_values() {
    let out = umbral_core::templates::render_str(
        "{{ x }}",
        &serde_json::json!({ "x": "<script>alert(1)</script>" }),
    )
    .expect("render");

    assert!(
        !out.contains("<script>"),
        "render_str must autoescape HTML — a raw <script> leaked: {out}"
    );
    assert!(
        out.contains("&lt;script&gt;"),
        "expected HTML-escaped output, got: {out}"
    );
}

#[test]
fn render_str_still_renders_plain_values_unchanged() {
    // HTML-escaping must not disturb values with no special characters.
    let out =
        umbral_core::templates::render_str("hello {{ name }}", &serde_json::json!({"name": "ada"}))
            .expect("render");
    assert_eq!(out, "hello ada");
}
