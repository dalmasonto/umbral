//! Pure-logic coverage for the column-selection helpers that back the
//! `Searchable` defaults. No DB — these read `Model::FIELDS` only.
//! Uses the crate-internal path (`umbra_core::orm::search`) since these
//! helpers are power-user surface, not necessarily on the facade.
use umbra_core::orm::search::{default_body, default_pk_column, default_title};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    umbra::orm::Choices,
)]
#[choices(rename_all = "lowercase")]
pub enum DocStatus {
    #[default]
    Draft,
    Live,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "srh_doc")]
pub struct Doc {
    pub id: i64,
    pub title: String,
    pub body: String,
    // `Slug` is a constrained-text wrapper: it lowers to `SqlType::Text`
    // with `text_format = Some("slug")`, so the body helper must exclude it.
    pub slug: umbra::orm::Slug,
    #[umbra(choices)]
    pub status: DocStatus,
}

#[test]
fn title_prefers_title_then_name_then_first_text() {
    assert_eq!(default_title::<Doc>(), "title");
}

#[test]
fn body_includes_text_columns_excludes_slug_and_choices() {
    let body = default_body::<Doc>();
    assert!(body.contains(&"title"), "title is a body column: {body:?}");
    assert!(body.contains(&"body"), "body is a body column: {body:?}");
    assert!(!body.contains(&"slug"), "slug (text_format) excluded: {body:?}");
    assert!(!body.contains(&"status"), "choices column excluded: {body:?}");
    assert!(!body.contains(&"id"), "non-text PK excluded: {body:?}");
}

#[test]
fn pk_column_is_the_primary_key() {
    assert_eq!(default_pk_column::<Doc>(), "id");
}
