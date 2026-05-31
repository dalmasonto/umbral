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

use std::marker::PhantomData;
use std::ops::{BitAnd, BitOr};

pub use model::{ArrayElement, FieldSpec, Model, PrimaryKey, SqlType};
pub use post::Post;
pub use queryset::{Manager, QuerySet};

/// A typed boolean condition on rows of `T`.
///
/// Built by inherent methods on the column types in `column` and passed
/// to `QuerySet::filter` / `QuerySet::exclude` to constrain a query. The
/// type parameter `T` ties the predicate to its model so a `Predicate<Post>`
/// can't accidentally be applied to a `QuerySet<Comment>`.
pub struct Predicate<T> {
    pub(crate) cond: sea_query::SimpleExpr,
    _phantom: PhantomData<T>,
}

impl<T> Predicate<T> {
    pub(crate) fn new(cond: sea_query::SimpleExpr) -> Self {
        Self {
            cond,
            _phantom: PhantomData,
        }
    }
}

/// Compose two predicates with logical AND.
impl<T> BitAnd for Predicate<T> {
    type Output = Predicate<T>;
    fn bitand(self, rhs: Predicate<T>) -> Predicate<T> {
        Predicate::new(self.cond.and(rhs.cond))
    }
}

/// Compose two predicates with logical OR.
impl<T> BitOr for Predicate<T> {
    type Output = Predicate<T>;
    fn bitor(self, rhs: Predicate<T>) -> Predicate<T> {
        Predicate::new(self.cond.or(rhs.cond))
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
