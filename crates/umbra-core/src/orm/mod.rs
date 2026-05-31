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

pub mod column;
pub mod model;
pub mod post;
pub mod queryset;
pub mod tsvector;
pub mod write;

use std::marker::PhantomData;
use std::ops::{BitAnd, BitOr};

pub use model::{ArrayElement, FieldSpec, Model, PrimaryKey, SqlType};
pub use post::Post;
pub use queryset::{Manager, QuerySet};
pub use tsvector::TsVector;

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
