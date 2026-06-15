//! Cross-model relevance search. See
//! `docs/superpowers/specs/2026-06-15-cross-model-search-design.md`.

use crate::db::DbPool;
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
    texts
        .first()
        .copied()
        .unwrap_or_else(default_pk_column::<T>)
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

/// One normalized search result. Column aliases are fixed so every model's
/// branch unions cleanly and `sqlx::FromRow` decodes identically per backend.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SearchHit {
    pub kind: String,
    pub pk: String,
    pub title: String,
    pub snippet: String,
    pub rank: f64,
}

/// Which dialect to emit. Resolved from the ambient pool at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Postgres,
    Sqlite,
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `coalesce(c1,'')||' '||coalesce(c2,'')||…` over the given columns.
fn concat_coalesce(cols: &[&str]) -> String {
    cols.iter()
        .map(|c| format!("coalesce({}, '')", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(" || ' ' || ")
}

/// The single normalized `SELECT` for one model. The query parameter is
/// referenced positionally: `$1` (Postgres, the websearch string) /
/// `?1` substring + `?2` prefix (SQLite). Every branch reuses the same
/// numbers so `Search::across` binds each value once regardless of arity.
pub fn branch_sql<T: Searchable>(backend: Backend) -> String {
    let table = quote_ident(T::TABLE);
    let kind = T::kind().replace('\'', "''");
    let ident = quote_ident(T::ident());
    let title = quote_ident(T::title());
    let body = T::body();
    let body_cols: Vec<&str> = if body.is_empty() {
        vec![T::title()]
    } else {
        body
    };
    let body_concat = concat_coalesce(&body_cols);
    // Body minus the title column, for the un-weighted part of the rank vector.
    let rest: Vec<&str> = body_cols
        .iter()
        .copied()
        .filter(|c| *c != T::title())
        .collect();

    match backend {
        Backend::Postgres => {
            let title_vec =
                format!("setweight(to_tsvector('english', coalesce({title}, '')), 'A')");
            let rest_vec = if rest.is_empty() {
                String::new()
            } else {
                format!(" || to_tsvector('english', {})", concat_coalesce(&rest))
            };
            format!(
                "SELECT '{kind}' AS kind, \
                 CAST({ident} AS text) AS pk, \
                 {title} AS title, \
                 left({body_concat}, 200) AS snippet, \
                 ts_rank({title_vec}{rest_vec}, websearch_to_tsquery('english', $1))::float8 AS rank \
                 FROM {table} \
                 WHERE to_tsvector('english', {body_concat}) @@ websearch_to_tsquery('english', $1)"
            )
        }
        Backend::Sqlite => {
            // ?1 = '%q%' (substring), ?2 = 'q%' (prefix bonus).
            let where_like = body_cols
                .iter()
                .map(|c| format!("{} LIKE ?1", quote_ident(c)))
                .collect::<Vec<_>>()
                .join(" OR ");
            let title_q = quote_ident(T::title());
            let body_substr_terms = body_cols
                .iter()
                .map(|c| format!("(CASE WHEN {} LIKE ?1 THEN 1.0 ELSE 0 END)", quote_ident(c)))
                .collect::<Vec<_>>()
                .join(" + ");
            format!(
                "SELECT '{kind}' AS kind, \
                 CAST({ident} AS TEXT) AS pk, \
                 {title} AS title, \
                 substr({body_concat}, 1, 200) AS snippet, \
                 ( (CASE WHEN {title_q} LIKE ?1 THEN 2.0 ELSE 0 END) \
                 + {body_substr_terms} \
                 + (CASE WHEN {title_q} LIKE ?2 THEN 1.0 ELSE 0 END) ) AS rank \
                 FROM {table} \
                 WHERE {where_like}"
            )
        }
    }
}

/// A tuple of `Searchable` models. Produces one normalized branch per member
/// for a given backend. Implemented for tuples of arity 1..=6 via the macro
/// below.
pub trait SearchSources {
    fn branches(backend: Backend) -> Vec<String>;
}

macro_rules! impl_search_sources {
    ($($T:ident),+) => {
        impl<$($T: Searchable),+> SearchSources for ($($T,)+) {
            fn branches(backend: Backend) -> Vec<String> {
                vec![$( branch_sql::<$T>(backend) ),+]
            }
        }
    };
}
impl_search_sources!(A);
impl_search_sources!(A, B);
impl_search_sources!(A, B, C);
impl_search_sources!(A, B, C, D);
impl_search_sources!(A, B, C, D, E);
impl_search_sources!(A, B, C, D, E, F);

/// Escape SQL `LIKE` metacharacters in a user query (for the SQLite path).
fn escape_like(q: &str) -> String {
    q.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Cross-model relevance search. See the module docs.
pub struct Search;

impl Search {
    /// Search every model in `S` for `query`, returning up to `limit` hits
    /// ordered by descending relevance. A blank query yields an empty Vec
    /// without touching the database.
    pub async fn across<S: SearchSources>(
        query: &str,
        limit: u64,
    ) -> Result<Vec<SearchHit>, sqlx::Error> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        match crate::db::pool_dispatched() {
            DbPool::Postgres(pool) => {
                let sql = format!(
                    "{} ORDER BY rank DESC LIMIT $2",
                    S::branches(Backend::Postgres).join("\nUNION ALL\n")
                );
                sqlx::query_as::<_, SearchHit>(&sql)
                    .bind(q)
                    .bind(limit as i64)
                    .fetch_all(pool)
                    .await
            }
            DbPool::Sqlite(pool) => {
                let sql = format!(
                    "{} ORDER BY rank DESC LIMIT ?3",
                    S::branches(Backend::Sqlite).join("\nUNION ALL\n")
                );
                let like = format!("%{}%", escape_like(q));
                let prefix = format!("{}%", escape_like(q));
                sqlx::query_as::<_, SearchHit>(&sql)
                    .bind(like) // ?1
                    .bind(prefix) // ?2
                    .bind(limit as i64) // ?3
                    .fetch_all(pool)
                    .await
            }
        }
    }
}
