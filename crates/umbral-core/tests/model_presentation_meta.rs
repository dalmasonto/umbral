//! gaps4 #95 — model-level presentation/query metadata. A model declares its
//! display/search/filter/inline-edit/readonly intent ONCE via per-field
//! `#[umbral(...)]` markers; `#[derive(Model)]` aggregates them into
//! struct-level column-name consts, and `ModelMeta::for_` mirrors them, so any
//! plugin (admin, rest) reads the same source of truth.

use serde::{Deserialize, Serialize};
use umbral::migrate::ModelMeta;
use umbral::orm::Model;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pm_software")]
pub struct Software {
    pub id: i64,

    #[umbral(string, list_display, search)]
    pub name: String,

    #[umbral(list_display, search)]
    pub tagline: String,

    #[umbral(list_display, list_filter, inline_edit)]
    pub category: String,

    // Not shown in list, but searchable.
    #[umbral(search)]
    pub description: String,

    #[umbral(readonly)]
    pub slug: String,
}

#[test]
fn presentation_consts_aggregate_marked_fields_in_declaration_order() {
    assert_eq!(Software::LIST_DISPLAY, &["name", "tagline", "category"]);
    assert_eq!(Software::SEARCH_FIELDS, &["name", "tagline", "description"]);
    assert_eq!(Software::LIST_FILTER, &["category"]);
    assert_eq!(Software::INLINE_EDIT_FIELDS, &["category"]);
    assert_eq!(Software::READONLY_FIELDS, &["slug"]);
}

#[test]
fn model_meta_mirrors_the_presentation_consts() {
    let meta = ModelMeta::for_::<Software>();
    assert_eq!(meta.list_display, vec!["name", "tagline", "category"]);
    assert_eq!(meta.search_fields, vec!["name", "tagline", "description"]);
    assert_eq!(meta.list_filter, vec!["category"]);
    assert_eq!(meta.inline_edit_fields, vec!["category"]);
    assert_eq!(meta.readonly_fields, vec!["slug"]);
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pm_plain")]
pub struct Plain {
    pub id: i64,
    pub title: String,
}

#[test]
fn a_model_with_no_markers_declares_empty_metadata() {
    // Empty = "not declared"; consumers fall back to their own defaults.
    assert!(Plain::LIST_DISPLAY.is_empty());
    assert!(Plain::SEARCH_FIELDS.is_empty());
    let meta = ModelMeta::for_::<Plain>();
    assert!(meta.search_fields.is_empty());
    assert!(meta.list_display.is_empty());
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pm_renamed")]
pub struct Renamed {
    pub id: i64,
    // The metadata must carry the COLUMN name, not the Rust field name, so a
    // plugin querying/rendering by column matches.
    #[sqlx(rename = "display_name")]
    #[umbral(search, list_display)]
    pub name: String,
}

#[test]
fn presentation_metadata_uses_the_column_name_not_the_field_name() {
    assert_eq!(Renamed::SEARCH_FIELDS, &["display_name"]);
    assert_eq!(Renamed::LIST_DISPLAY, &["display_name"]);
}
