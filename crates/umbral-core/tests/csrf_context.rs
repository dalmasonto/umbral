//! The render merge for the ambient CSRF token: `{{ csrf_token }}`
//! (raw value) and `{{ csrf_input }}` (pre-built hidden input — the
//! Django `{% csrf_token %}` equivalent) appear in every template
//! rendered inside `with_current_csrf` scope; explicit ctx keys win;
//! outside the scope nothing is injected.
//!
//! Same OnceLock-guarded single `templates::init` boot as
//! `template_discovery.rs` — the engine is a process-global.

use std::fs;
use std::sync::OnceLock;

use tempfile::TempDir;
use umbral_core::templates;

static DIR: OnceLock<TempDir> = OnceLock::new();

fn boot() {
    DIR.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("token.html"),
            "tok=[{{ csrf_token }}] input=[{{ csrf_input }}]",
        )
        .unwrap();
        let _ = templates::init(&[dir.path().to_path_buf()]);
        dir
    });
}

#[tokio::test]
async fn csrf_merged_inside_scope() {
    boot();
    let out = templates::with_current_csrf(Some("abc123".to_string()), async {
        templates::render("token.html", &serde_json::json!({})).unwrap()
    })
    .await;
    assert!(out.contains("tok=[abc123]"), "raw token missing: {out}");
    assert!(
        out.contains(r#"<input type="hidden" name="csrf_token" value="abc123">"#),
        "csrf_input missing or escaped: {out}"
    );
}

#[tokio::test]
async fn explicit_ctx_key_wins_over_merge() {
    boot();
    let out = templates::with_current_csrf(Some("ambient".to_string()), async {
        templates::render("token.html", &serde_json::json!({"csrf_token": "explicit"})).unwrap()
    })
    .await;
    assert!(out.contains("tok=[explicit]"), "explicit ctx lost: {out}");
}

#[tokio::test]
async fn nothing_injected_outside_scope() {
    boot();
    let out = templates::render("token.html", &serde_json::json!({})).unwrap();
    assert!(out.contains("tok=[]"), "token leaked outside scope: {out}");
    assert!(
        out.contains("input=[]"),
        "input leaked outside scope: {out}"
    );
}

#[tokio::test]
async fn current_csrf_reads_the_scoped_value() {
    let got =
        templates::with_current_csrf(Some("xyz".to_string()), async { templates::current_csrf() })
            .await;
    assert_eq!(got.as_deref(), Some("xyz"));
    assert_eq!(templates::current_csrf(), None);
}

#[tokio::test]
async fn private_engines_can_merge_ambient_context() {
    let out = templates::with_current_csrf(Some("private-token".to_string()), async {
        let mut env = minijinja::Environment::new();
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
        env.add_template(
            "private.html",
            "name=[{{ name }}] tok=[{{ csrf_token }}] input=[{{ csrf_input }}]",
        )
        .unwrap();
        let tmpl = env.get_template("private.html").unwrap();
        tmpl.render(templates::merge_ambient_context(
            &serde_json::json!({"name": "plugin"}),
        ))
        .unwrap()
    })
    .await;

    assert!(out.contains("name=[plugin]"), "ctx value missing: {out}");
    assert!(
        out.contains("tok=[private-token]"),
        "ambient csrf token missing: {out}"
    );
    assert!(
        out.contains(r#"<input type="hidden" name="csrf_token" value="private-token">"#),
        "ambient csrf input missing or escaped: {out}"
    );
}

#[tokio::test]
async fn private_engines_can_merge_ambient_value() {
    let out = templates::with_current_csrf(Some("value-token".to_string()), async {
        let mut env = minijinja::Environment::new();
        env.add_template("private.html", "tok=[{{ csrf_token }}]")
            .unwrap();
        let tmpl = env.get_template("private.html").unwrap();
        tmpl.render(templates::merge_ambient_value(minijinja::context!()))
            .unwrap()
    })
    .await;

    assert!(
        out.contains("tok=[value-token]"),
        "ambient csrf token missing for Value ctx: {out}"
    );
}
