//! The ORM: declarative models, typed queries, and SQL generation.
//!
//! At M1 the design is intentionally narrow: one hardcoded model (`Post`),
//! a single QuerySet type backed by sea-query, and basic predicates. No
//! `Model` trait abstraction yet (that's M2), no derive macro (that's M3),
//! no joins / aggregates / relations (later milestones). See
//! `docs/specs/03-orm-querysets.md` for the target shape and the
//! M1→M2→M3 progression.
//!
//! Module layout:
//!
//! - `post` — the hardcoded `Post` struct and its sibling column module.
//! - `column` — column types (`StrCol`, `IntCol`, `NullableDateTimeCol`,
//!   etc.) carrying inherent methods that build `Predicate`s.
//! - `queryset` — `QuerySet<T>` and `Manager<T>`, the chainable / lazy
//!   SQL builder plus its terminal methods.
//!
//! The shared types — `Predicate<T>` and `OrderExpr<T>` — live here in
//! `mod.rs` so both `column` and `queryset` can reach them without
//! crossing each other.

pub mod aggregate;
pub mod choices;
pub mod column;
pub mod dynamic;
pub mod expr;
pub mod file_field;
pub mod foreign_key;
pub mod forms_runtime;
pub mod m2m;
pub mod masked;
pub mod model;
pub mod multichoice;
pub mod one_to_one;
pub mod post;
pub mod queryset;
pub mod reverse_accessor;
pub mod reverse_set;
pub mod tsvector;
pub mod validation;
pub mod validators;
pub mod write;

use std::marker::PhantomData;
use std::ops::{BitAnd, BitOr};

pub use aggregate::{Aggregate, AggregateKind};

/// Canonical string key for a primary-key (or FK) value, for bucketing
/// relation children by their parent's PK in a `HashMap` / `HashSet`.
///
/// `serde_json::Value` is not `Hash`, and the relation-hydration paths
/// need to group children by parent PK whatever the PK type — `i64`,
/// `String`, `uuid::Uuid`. This is the **PK-agnostic** replacement for the
/// historical `i64` keys: the value is namespaced by shape (`n:` number,
/// `s:` string, `o:` other) so a numeric `42` and the string `"42"` never
/// collide in the same bucket. Pairs with
/// [`Model::pk_as_json`](crate::orm::Model::pk_as_json) and
/// [`HydrateRelated::fk_id_for`](crate::orm::HydrateRelated::fk_id_for),
/// both of which return a `serde_json::Value`.
pub fn pk_key(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Number(n) => format!("n:{n}"),
        serde_json::Value::String(s) => format!("s:{s}"),
        other => format!("o:{other}"),
    }
}

/// Escape SQL `LIKE` wildcards in a user-supplied **literal** substring.
///
/// `contains` / `startswith` / `icontains` / the REST `__contains`
/// family treat their argument as a literal to find, then wrap it in
/// structural `%`. Without escaping, a user typing `%`, `_` or `\` would
/// inject wildcards into the pattern — a search for `"100%"` matches
/// every row starting with `100`, and `"a_b"` matches `axb` (ORM-1).
/// This backslash-escapes the three LIKE metacharacters; the caller then
/// adds its own structural `%` and pairs the predicate with
/// `LikeExpr::escape('\\')` so the database honours the escape. Not SQL
/// injection (the pattern is still a bound parameter) — a match-semantics
/// correctness fix. The user-facing `.like()` / `.ilike()` builders take
/// a raw pattern on purpose and must NOT call this.
pub fn escape_like_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// A typed wrapper around a `sea_query::SelectStatement` for use in
/// `col IN (SELECT col FROM ...)` predicates (gap #26).
///
/// Built by [`QuerySet::into_subquery`] or
/// [`Manager::into_subquery`]; consumed by `IntCol::in_subquery` /
/// `ForeignKeyCol::in_subquery` to produce a `Predicate`. The inner
/// SelectStatement only knows the projected column the caller
/// requested.
pub struct Subquery {
    inner: sea_query::SelectStatement,
}

impl Subquery {
    /// Construct from a `SelectStatement` (internal — the
    /// QuerySet/Manager helpers are the supported entry points).
    pub(crate) fn from_select(inner: sea_query::SelectStatement) -> Self {
        Self { inner }
    }

    /// Consume the wrapper and hand back the inner SelectStatement
    /// — sea-query's `in_subquery` builder takes ownership.
    pub(crate) fn into_statement(self) -> sea_query::SelectStatement {
        self.inner
    }
}
pub use choices::ChoiceField;
pub use dynamic::{CsvImportReport, DynError, DynQuerySet, decode_to_string, import_table_rows};
pub use expr::{F, FColExt, FExpr, Q};
pub use file_field::{FileField, ImageField};
pub use foreign_key::ForeignKey;
pub use m2m::{M2M, load_junction_selection, set_junction_dynamic};
pub use masked::{MaskError, MaskKeyring, Masked, set_mask_keyring};
pub use model::{
    ArrayElement, FieldSpec, FkAction, HydrateRelated, M2MRelationSpec, Model,
    OneToOneRelationSpec, PrimaryKey, ReverseFkRelationSpec, SqlType,
};
pub use multichoice::MultiChoice;
pub use one_to_one::OneToOne;
pub use post::Post;
pub use queryset::{GetError, JoinKind, Manager, QuerySet, QuerySetTx, TryForEachError};
pub use reverse_accessor::{ReverseError, ReverseRelations};
pub use reverse_set::ReverseSet;
pub use tsvector::TsVector;
pub use validators::{Email, Slug, Url, ValidatorError, validate_text_format};
pub use write::SaveError;

/// A typed boolean condition on rows of `T`.
///
/// Built by inherent methods on the column types in `column` and passed
/// to `QuerySet::filter` / `QuerySet::exclude` to constrain a query. The
/// type parameter `T` ties the predicate to its model so a `Predicate<Post>`
/// can't accidentally be applied to a `QuerySet<Comment>`.
pub struct Predicate<T> {
    /// The default condition. Renders correctly on Postgres and on
    /// any backend whose operators match sea-query's defaults.
    pub(crate) cond: sea_query::SimpleExpr,
    /// Optional SQLite-specific override. Set by predicates that need
    /// dialect-specific rendering — Phase 4.2.2 JSON operators are the
    /// first consumer (`json_extract` instead of Postgres's `->` /
    /// `->>`). When `None`, `cond` is used for both backends. The
    /// QuerySet picks at terminal time based on the resolved pool
    /// variant.
    pub(crate) cond_sqlite: Option<sea_query::SimpleExpr>,
    _phantom: PhantomData<T>,
}

impl<T> Predicate<T> {
    /// Build a `col = value` predicate by column name. Use when the
    /// column constant isn't reachable at the call site — typically
    /// generic-over-`T` helper functions in plugin code (e.g.
    /// `authenticate<U: UserModel>` filtering on `"username"` without
    /// knowing `U`'s column module).
    ///
    /// The typed sibling-module path (`my_model::USERNAME.eq(...)`) is
    /// preferred when you have a concrete `T`, because it catches typos
    /// at compile time. This constructor is the escape hatch for
    /// genuinely-generic code.
    pub fn col_eq(col: &'static str, value: impl Into<sea_query::Value>) -> Self {
        let expr = sea_query::Expr::col(sea_query::Alias::new(col)).eq(value);
        Self::new(expr)
    }

    pub(crate) fn new(cond: sea_query::SimpleExpr) -> Self {
        Self {
            cond,
            cond_sqlite: None,
            _phantom: PhantomData,
        }
    }

    /// Construct a predicate that renders differently on each backend.
    /// Phase 4.2.2's JSON-operator path uses this to ship one
    /// predicate that resolves to `col -> 'a' ->> 'b'` under Postgres
    /// and `json_extract(col, '$.a.b')` under SQLite.
    pub(crate) fn new_with_sqlite(
        cond: sea_query::SimpleExpr,
        cond_sqlite: sea_query::SimpleExpr,
    ) -> Self {
        Self {
            cond,
            cond_sqlite: Some(cond_sqlite),
            _phantom: PhantomData,
        }
    }

    /// Pick the SimpleExpr appropriate for `backend_name` (`"sqlite"`
    /// or `"postgres"`). Falls back to the default `cond` when no
    /// SQLite override is set or the backend isn't SQLite. Cloning
    /// the SimpleExpr is cheap (it's a tree of small enum values).
    pub(crate) fn cond_for(&self, backend_name: &str) -> sea_query::SimpleExpr {
        match backend_name {
            "sqlite" => self
                .cond_sqlite
                .clone()
                .unwrap_or_else(|| self.cond.clone()),
            _ => self.cond.clone(),
        }
    }
}

/// Compose two predicates with logical AND. Both per-backend variants
/// combine element-wise — if either side has a SQLite override, the
/// combined predicate carries the AND of (lhs's sqlite-or-default)
/// with (rhs's sqlite-or-default). When neither side overrides, the
/// combined predicate keeps `cond_sqlite = None` so the default render
/// path stays uniform.
impl<T> BitAnd for Predicate<T> {
    type Output = Predicate<T>;
    fn bitand(self, rhs: Predicate<T>) -> Predicate<T> {
        let any_sqlite_override = self.cond_sqlite.is_some() || rhs.cond_sqlite.is_some();
        let combined_sqlite = if any_sqlite_override {
            let lhs_sql = self
                .cond_sqlite
                .clone()
                .unwrap_or_else(|| self.cond.clone());
            let rhs_sql = rhs.cond_sqlite.clone().unwrap_or_else(|| rhs.cond.clone());
            Some(lhs_sql.and(rhs_sql))
        } else {
            None
        };
        Predicate {
            cond: self.cond.and(rhs.cond),
            cond_sqlite: combined_sqlite,
            _phantom: PhantomData,
        }
    }
}

/// Compose two predicates with logical OR. Same backend-variant story
/// as [`BitAnd`].
impl<T> BitOr for Predicate<T> {
    type Output = Predicate<T>;
    fn bitor(self, rhs: Predicate<T>) -> Predicate<T> {
        let any_sqlite_override = self.cond_sqlite.is_some() || rhs.cond_sqlite.is_some();
        let combined_sqlite = if any_sqlite_override {
            let lhs_sql = self
                .cond_sqlite
                .clone()
                .unwrap_or_else(|| self.cond.clone());
            let rhs_sql = rhs.cond_sqlite.clone().unwrap_or_else(|| rhs.cond.clone());
            Some(lhs_sql.or(rhs_sql))
        } else {
            None
        };
        Predicate {
            cond: self.cond.or(rhs.cond),
            cond_sqlite: combined_sqlite,
            _phantom: PhantomData,
        }
    }
}

/// An ordering directive for one column.
///
/// Built by `.asc()` / `.desc()` on a column constant and passed to
/// `QuerySet::order_by`. The type parameter `T` ties the directive to its
/// model the same way `Predicate<T>` does.
pub struct OrderExpr<T> {
    pub(crate) column: &'static str,
    pub(crate) descending: bool,
    _phantom: PhantomData<T>,
}

impl<T> OrderExpr<T> {
    pub(crate) fn new(column: &'static str, descending: bool) -> Self {
        Self {
            column,
            descending,
            _phantom: PhantomData,
        }
    }
}
