//! `#[umbral(str = "{a} {b}")]` — the per-instance display template (the ORM's
//! `__str__`). A model declares how to concatenate its fields into a human
//! label; `ModelMeta::render_str` produces it from a row, and the admin uses it
//! wherever it labels a single object (FK/M2M chips, pickers, titles).

#![allow(dead_code)]

use serde_json::json;
use umbral::migrate::ModelMeta;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "mst_person", str = "{first_name} {last_name}")]
pub struct Person {
    pub id: i64,
    #[umbral(string)]
    pub first_name: String,
    pub last_name: String,
    pub age: i32,
}

// A model with NO template — render_str must return None so callers fall back.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "mst_plain")]
pub struct Plain {
    pub id: i64,
    pub label: String,
}

fn obj(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

/// The template flows from the struct attribute into `ModelMeta` (and, under
/// the hood, `Model::STR_TEMPLATE`).
#[test]
fn template_is_captured_in_meta() {
    let meta = ModelMeta::for_::<Person>();
    assert_eq!(
        meta.str_template.as_deref(),
        Some("{first_name} {last_name}")
    );

    // A model without `#[umbral(str = ...)]` carries none.
    assert!(ModelMeta::for_::<Plain>().str_template.is_none());
}

/// `render_str` concatenates the named fields for a row — the whole point.
#[test]
fn render_str_concatenates_multiple_fields() {
    let meta = ModelMeta::for_::<Person>();
    let row = obj(json!({ "id": 1, "first_name": "Ada", "last_name": "Lovelace", "age": 36 }));
    assert_eq!(meta.render_str(&row).as_deref(), Some("Ada Lovelace"));
}

/// A model with no template renders `None`, so the admin falls back to its
/// `#[umbral(string)]` column / PK.
#[test]
fn no_template_renders_none() {
    let meta = ModelMeta::for_::<Plain>();
    let row = obj(json!({ "id": 1, "label": "x" }));
    assert!(meta.render_str(&row).is_none());
}

/// Numbers stringify, null/absent fields become empty, and `{{`/`}}` are
/// literal braces — the substitution rules a `__str__` template needs.
#[test]
fn render_str_number_null_and_escape_rules() {
    // A throwaway meta whose template exercises every rule.
    let mut meta = ModelMeta::for_::<Person>();
    meta.str_template = Some("#{id}: {first_name} aged {age} {{literal}}".into());

    let row = obj(json!({ "id": 7, "first_name": "Grace", "last_name": null, "age": 45 }));
    assert_eq!(
        meta.render_str(&row).as_deref(),
        Some("#7: Grace aged 45 {literal}")
    );

    // A null / absent field substitutes to empty (not "null").
    meta.str_template = Some("{first_name} {last_name}".into());
    let row2 = obj(json!({ "first_name": "Katherine", "last_name": null }));
    assert_eq!(meta.render_str(&row2).as_deref(), Some("Katherine "));
}
