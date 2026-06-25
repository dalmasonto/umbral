//! `FormErrors::render` — the Django-bound-form ergonomic: a failed
//! submission renders the form template itself (422), binding `form`
//! to the user's raw keystrokes and `errors` to the flat per-field
//! view plus a default form-level summary banner. One line in the
//! handler's `Err` arm replaces the render-helper boilerplate every
//! form view used to carry.

use std::collections::HashMap;
use std::fs;
use std::sync::OnceLock;

use tempfile::TempDir;
use umbral_core::forms::FormErrors;
use umbral_core::orm::write::WriteError;
use umbral_core::templates;

static DIR: OnceLock<TempDir> = OnceLock::new();

fn boot() {
    DIR.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("form.html"),
            "name=[{{ form.name }}] err=[{{ errors.name }}] banner=[{{ errors.form }}] sent=[{% if sent %}yes{% else %}no{% endif %}]",
        )
        .unwrap();
        let _ = templates::init(&[dir.path().to_path_buf()]);
        dir
    });
}

fn errs_with_raw() -> FormErrors {
    let raw: HashMap<String, String> = [("name".to_string(), "A".to_string())]
        .into_iter()
        .collect();
    FormErrors::with_raw(
        WriteError::Validator {
            field: "name".to_string(),
            message: "Name is too short.".to_string(),
        },
        raw,
    )
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn render_binds_raw_values_errors_and_default_banner_with_422() {
    boot();
    let resp = errs_with_raw().render("form.html");
    assert_eq!(resp.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let html = body_text(resp).await;
    assert!(html.contains("name=[A]"), "raw keystroke lost: {html}");
    assert!(
        html.contains("err=[Name is too short.]"),
        "field error missing: {html}"
    );
    assert!(
        html.contains("banner=[Please fix the highlighted fields and try again.]"),
        "default banner missing: {html}"
    );
    // Keys the caller didn't pass stay undefined-falsy (lenient mode).
    assert!(html.contains("sent=[no]"), "stray ctx leaked: {html}");
}

#[tokio::test]
async fn render_keeps_an_explicit_non_field_error_as_the_banner() {
    boot();
    let errs = FormErrors::new(WriteError::Validator {
        field: String::new(),
        message: "Submissions are closed.".to_string(),
    });
    let html = body_text(errs.render("form.html")).await;
    assert!(
        html.contains("banner=[Submissions are closed.]"),
        "explicit non-field error must beat the default banner: {html}"
    );
}

#[tokio::test]
async fn render_with_merges_extra_context_keys() {
    boot();
    let mut extra = serde_json::Map::new();
    extra.insert("sent".to_string(), serde_json::Value::Bool(true));
    let html = body_text(errs_with_raw().render_with("form.html", extra)).await;
    assert!(html.contains("sent=[yes]"), "extra ctx not merged: {html}");
    assert!(
        html.contains("name=[A]"),
        "form binding lost with extra ctx: {html}"
    );
}

#[tokio::test]
async fn render_missing_template_is_a_500_not_a_panic() {
    boot();
    let resp = errs_with_raw().render("nope.html");
    assert_eq!(resp.status(), axum::http::StatusCode::INTERNAL_SERVER_ERROR);
}
