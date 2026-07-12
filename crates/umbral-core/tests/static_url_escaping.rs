//! `{{ static(...) }}` must render a real URL, not `&#x2f;static&#x2f;css&#x2f;app.css`
//! (gaps3 #66).
//!
//! `static()` and `media_url()` returned a plain `String`, which minijinja autoescapes in
//! HTML context. So every page in every umbral app emitted
//!
//!     <link rel="stylesheet" href="&#x2f;static&#x2f;css&#x2f;app.css">
//!
//! Browsers decode `&#x2f;` back to `/`, so the stylesheet loaded and the page looked
//! right — which is precisely why nobody caught it. It was working by accident, and
//! anyone reading the page source would reasonably conclude static serving was broken.
//!
//! The fix has to hold BOTH ends: the URL must come out clean, and a hostile
//! upload filename must keep its armour. `media_url(key)` takes a key that came from a
//! user; a key containing `"` would close the `href` attribute and inject markup. So a
//! URL carrying any HTML-special character stays escaped.

use minijinja::Environment;
use umbral::templates::safe_url;

/// Render one expression in a template named `*.html`, which is what turns minijinja's
/// HTML autoescaping ON. (`minijinja::render!` uses an unnamed template, so autoescape is
/// OFF and the test would pass no matter what `safe_url` did — which is exactly the trap
/// this test exists to avoid falling into.)
fn render_html(src: &str, key: &str) -> String {
    let mut env = Environment::new();
    env.add_function("static", |p: String| safe_url(format!("/static/{p}")));
    env.add_function("media_url", |k: String| safe_url(format!("/media/{k}")));
    env.add_template_owned("t.html".to_string(), src.to_string())
        .unwrap();
    env.get_template("t.html")
        .unwrap()
        .render(minijinja::context! { k => key })
        .unwrap()
}

#[test]
fn a_normal_url_renders_clean() {
    let out = render_html(r#"<link href="{{ static('css/app.css') }}">"#, "");
    assert_eq!(out, r#"<link href="/static/css/app.css">"#);
    assert!(
        !out.contains("&#x2f;"),
        "the slashes were HTML-escaped: {out}"
    );
}

#[test]
fn a_hostile_filename_is_still_escaped() {
    // The key an attacker would upload: it closes the href and opens a tag.
    let evil = r#"a" onerror="alert(1)"#;
    let out = render_html(r#"<img src="{{ media_url(k) }}">"#, evil);
    assert!(
        !out.contains(r#"onerror="alert(1)""#),
        "a user-controlled filename broke out of the attribute — this is XSS: {out}"
    );
    assert!(
        out.contains("&#x22;") || out.contains("&quot;"),
        "the quote must stay escaped: {out}"
    );
}

/// The exact characters that decide it. Anything that could terminate an attribute or
/// open a tag keeps the escaping; everything else — which is every URL a template author
/// actually writes — comes out clean.
#[test]
fn only_html_special_characters_keep_the_escaping() {
    for clean in [
        "/static/css/app.css",
        "/static/css/app.a1b2c3.css", // hashed by `collectstatic --hashed`
        "/media/photo (1).jpg",
        "https://cdn.example.com/x.css",
    ] {
        assert!(
            safe_url(clean.to_string()).is_safe(),
            "`{clean}` should render unescaped"
        );
    }
    for hostile in [
        r#"/media/a".jpg"#,
        "/media/<script>.jpg",
        "/media/a&b.jpg",
        "/media/a'b.jpg",
    ] {
        assert!(
            !safe_url(hostile.to_string()).is_safe(),
            "`{hostile}` MUST stay escaped"
        );
    }
}
