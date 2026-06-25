//! Pure-logic coverage for the column-selection helpers that back the
//! `Searchable` defaults. No DB — these read `Model::FIELDS` only.
//! Uses the crate-internal path (`umbral_core::orm::search`) since these
//! helpers are power-user surface, not necessarily on the facade.
use umbral_core::orm::search::{default_body, default_pk_column, default_title};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    umbral::orm::Choices,
)]
#[choices(rename_all = "lowercase")]
pub enum DocStatus {
    #[default]
    Draft,
    Live,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "srh_doc")]
pub struct Doc {
    pub id: i64,
    pub title: String,
    pub body: String,
    // `Slug` is a constrained-text wrapper: it lowers to `SqlType::Text`
    // with `text_format = Some("slug")`, so the body helper must exclude it.
    pub slug: umbral::orm::Slug,
    #[umbral(choices)]
    pub status: DocStatus,
}

impl umbral_core::orm::Searchable for Doc {}

#[test]
fn title_prefers_title_then_name_then_first_text() {
    assert_eq!(default_title::<Doc>(), "title");
}

#[test]
fn body_includes_text_columns_excludes_slug_and_choices() {
    let body = default_body::<Doc>();
    assert!(body.contains(&"title"), "title is a body column: {body:?}");
    assert!(body.contains(&"body"), "body is a body column: {body:?}");
    assert!(
        !body.contains(&"slug"),
        "slug (text_format) excluded: {body:?}"
    );
    assert!(
        !body.contains(&"status"),
        "choices column excluded: {body:?}"
    );
    assert!(!body.contains(&"id"), "non-text PK excluded: {body:?}");
}

#[test]
fn pk_column_is_the_primary_key() {
    assert_eq!(default_pk_column::<Doc>(), "id");
}

use umbral_core::orm::search::{Backend, branch_sql};

#[test]
fn postgres_branch_has_tsrank_setweight_and_union_shape() {
    let sql = branch_sql::<Doc>(Backend::Postgres);
    assert!(
        sql.contains("'srh_doc' AS kind") || sql.contains("'srh_doc'  AS kind"),
        "{sql}"
    );
    assert!(sql.contains("AS pk"), "{sql}");
    assert!(sql.contains("ts_rank("), "{sql}");
    assert!(
        sql.contains("setweight(to_tsvector('english'"),
        "title weighted: {sql}"
    );
    assert!(sql.contains("websearch_to_tsquery('english', $1)"), "{sql}");
    assert!(sql.contains("::float8 AS rank"), "rank cast to f64: {sql}");
}

#[test]
fn sqlite_branch_uses_weighted_like_case() {
    let sql = branch_sql::<Doc>(Backend::Sqlite);
    assert!(sql.contains("CASE WHEN"), "{sql}");
    assert!(sql.contains("LIKE ?1"), "substring param: {sql}");
    assert!(sql.contains("LIKE ?2"), "prefix param: {sql}");
    assert!(sql.contains("AS rank"), "{sql}");
    assert!(!sql.contains("to_tsvector"), "no tsvector on sqlite: {sql}");
}

#[test]
fn facade_paths_resolve() {
    // Compile-time proof the public path the docs promise exists.
    fn _assert<T: umbral::orm::Searchable>() {}
    let _ = umbral::orm::SearchHit {
        kind: String::new(),
        pk: String::new(),
        title: String::new(),
        snippet: String::new(),
        rank: 0.0,
    };
}
