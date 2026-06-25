//! The `| markdown` template filter: render CommonMark + GFM to HTML,
//! sanitize it (no script/event-handler XSS), and hand it to the
//! template as a safe string so autoescape doesn't double-escape the
//! generated tags. The reusable "render body/usage text" surface for
//! admin and end-user plugins alike.

use std::fs;
use std::sync::OnceLock;

use tempfile::TempDir;
use umbral_core::templates;

static DIR: OnceLock<TempDir> = OnceLock::new();

fn boot() {
    DIR.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("md.html"), "{{ body | markdown }}").unwrap();
        fs::write(dir.path().join("san.html"), "{{ body | sanitize }}").unwrap();
        let _ = templates::init(&[dir.path().to_path_buf()]);
        dir
    });
}

fn render(body: &str) -> String {
    templates::render("md.html", &serde_json::json!({ "body": body })).unwrap()
}

#[test]
fn renders_basic_commonmark_to_html() {
    boot();
    let out = render("# Title\n\nSome **bold** and a [link](https://umbral.dev).");
    assert!(out.contains("<h1>Title</h1>"), "heading missing: {out}");
    assert!(out.contains("<strong>bold</strong>"), "bold missing: {out}");
    // ammonia adds rel="noopener noreferrer" to links (a security
    // default we want), so assert the href + text, not the exact tag.
    assert!(
        out.contains(r#"<a href="https://umbral.dev""#) && out.contains(">link</a>"),
        "link missing: {out}"
    );
}

#[test]
fn renders_gfm_tables() {
    boot();
    let out = render("| a | b |\n|---|---|\n| 1 | 2 |");
    assert!(out.contains("<table>"), "GFM table not rendered: {out}");
    assert!(out.contains("<td>1</td>"), "table cell missing: {out}");
}

#[test]
fn sanitizes_script_and_event_handlers() {
    boot();
    // Raw HTML embedded in markdown must not survive as executable.
    let out = render("ok <script>alert(1)</script> <img src=x onerror=alert(1)>");
    assert!(!out.contains("<script"), "script tag survived: {out}");
    assert!(!out.contains("onerror"), "event handler survived: {out}");
    assert!(out.contains("ok"), "benign text dropped: {out}");
}

#[test]
fn output_is_not_double_escaped_by_autoescape() {
    boot();
    // The filter returns a safe string, so the <em> tags reach the
    // browser as markup, not as &lt;em&gt; entities.
    let out = render("_italic_");
    assert!(
        out.contains("<em>italic</em>"),
        "safe-string not honored: {out}"
    );
    assert!(!out.contains("&lt;em&gt;"), "output double-escaped: {out}");
}

#[test]
fn empty_input_renders_empty() {
    boot();
    assert_eq!(render("").trim(), "");
}

#[test]
fn sanitize_filter_keeps_safe_html_strips_scripts() {
    boot();
    // The RTE widget stores HTML; `| sanitize` is its safe-display
    // companion — keep formatting tags, drop script/event handlers.
    let out = templates::render(
        "san.html",
        &serde_json::json!({ "body": "<p>Hi <b>there</b></p><script>alert(1)</script><a href=\"javascript:alert(1)\">x</a>" }),
    )
    .unwrap();
    assert!(
        out.contains("<p>Hi <b>there</b></p>"),
        "safe html dropped: {out}"
    );
    assert!(!out.contains("<script"), "script survived: {out}");
    assert!(!out.contains("javascript:"), "js url survived: {out}");
}
