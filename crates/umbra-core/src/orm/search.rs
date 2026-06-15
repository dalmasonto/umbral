//! Cross-model relevance search. See
//! `docs/superpowers/specs/2026-06-15-cross-model-search-design.md`.

use crate::orm::{FieldSpec, Model, SqlType};

/// A model that can take part in [`Search::across`]. Opt in with a marker
/// `impl Searchable for T {}`; every default is read from `T::FIELDS`.
pub trait Searchable: Model {
    /// Result tag, e.g. `"plugin"`. Default: the table name.
    fn kind() -> &'static str {
        Self::TABLE
    }
    /// Column shown as the result title. Default: the text column named
    /// (case-insensitively) `title` or `name`, else the first text column.
    fn title() -> &'static str {
        default_title::<Self>()
    }
    /// Text columns forming the searchable body. Default: every text column
    /// except metadata-flagged non-content ones (slug/url/email/choices).
    fn body() -> Vec<&'static str> {
        default_body::<Self>()
    }
    /// Column whose value becomes `SearchHit.pk` (the routing key). Default:
    /// the primary-key column. Override to a natural key (e.g. a slug).
    fn ident() -> &'static str {
        default_pk_column::<Self>()
    }
}

/// True when a `FieldSpec` is plain searchable prose: a `Text` column that
/// is not a constrained-text wrapper (slug/url/email) and not a choices set.
fn is_content_text(f: &FieldSpec) -> bool {
    matches!(f.ty, SqlType::Text) && f.text_format.is_none() && f.choices.is_empty()
}

/// The text columns of `T`, in declaration order, minus non-content ones.
pub fn default_body<T: Model>() -> Vec<&'static str> {
    T::FIELDS
        .iter()
        .filter(|f| is_content_text(f))
        .map(|f| f.name)
        .collect()
}

/// Title column: a content-text column named `title` or `name`
/// (case-insensitive), else the first content-text column, else the PK.
pub fn default_title<T: Model>() -> &'static str {
    let texts: Vec<&'static str> = default_body::<T>();
    for want in ["title", "name"] {
        if let Some(c) = texts.iter().find(|c| c.eq_ignore_ascii_case(want)) {
            return c;
        }
    }
    texts.first().copied().unwrap_or_else(default_pk_column::<T>)
}

/// The primary-key column name (first `primary_key` field; falls back to
/// the conventional `id`).
pub fn default_pk_column<T: Model>() -> &'static str {
    T::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.name)
        .unwrap_or("id")
}
