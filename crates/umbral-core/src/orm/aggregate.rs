//! Aggregate functions for `QuerySet::aggregate` / `annotate`.
//!
//! `Aggregate` is a closed enum covering the SQL standard set the
//! framework supports today — COUNT, SUM, AVG, MAX, MIN. Each variant
//! knows the column it operates on (or `*` for COUNT) and renders to a
//! sea-query `SimpleExpr` at terminal time.
//!
//! ```rust,ignore
//! use umbral::orm::Aggregate;
//!
//! // Single-row aggregate: "how many published posts, and what's the
//! // total view count?"
//! let summary = Post::objects()
//!     .filter(post::PUBLISHED.eq(true))
//!     .aggregate(&[
//!         ("count", Aggregate::count()),
//!         ("views", Aggregate::sum("view_count")),
//!     ])
//!     .await?;
//!
//! // Grouped: "post count per author."
//! let by_author = Post::objects()
//!     .annotate(&["author_id"], &[("count", Aggregate::count())])
//!     .await?;
//! ```
//!
//! StdDev / Variance / window-function aggregates are deferred. Add a
//! new variant when a real consumer surfaces the need.

use sea_query::{Alias, Expr, Func, SimpleExpr};

/// A SQL aggregate function over a single column (or `*` for COUNT).
///
/// Built via the named constructors on this type and passed to
/// [`crate::orm::QuerySet::aggregate`] or
/// [`crate::orm::QuerySet::annotate`] paired with an output name.
#[derive(Debug, Clone)]
pub enum Aggregate {
    /// `COUNT(*)` when `column` is `None`, `COUNT(col)` when set
    /// (which skips NULLs in the named column).
    Count(Option<String>),
    /// `SUM(col)` — NULL when no rows match.
    Sum(String),
    /// `AVG(col)` — always renders to a floating-point result type.
    Avg(String),
    /// `MAX(col)` — same return type as the column.
    Max(String),
    /// `MIN(col)` — same return type as the column.
    Min(String),
}

impl Aggregate {
    /// `COUNT(*)` — every row, including those with NULL columns.
    pub fn count() -> Self {
        Aggregate::Count(None)
    }

    /// `COUNT(col)` — skips rows where `col` is NULL.
    pub fn count_col(name: impl Into<String>) -> Self {
        Aggregate::Count(Some(name.into()))
    }

    /// `SUM(col)`.
    pub fn sum(name: impl Into<String>) -> Self {
        Aggregate::Sum(name.into())
    }

    /// `AVG(col)`.
    pub fn avg(name: impl Into<String>) -> Self {
        Aggregate::Avg(name.into())
    }

    /// `MAX(col)`.
    pub fn max(name: impl Into<String>) -> Self {
        Aggregate::Max(name.into())
    }

    /// `MIN(col)`.
    pub fn min(name: impl Into<String>) -> Self {
        Aggregate::Min(name.into())
    }

    /// Source column for this aggregate, or `None` for `COUNT(*)`.
    /// Used by the QuerySet terminals to validate against
    /// `Model::FIELDS` before running any SQL.
    pub fn source_column(&self) -> Option<&str> {
        match self {
            Aggregate::Count(c) => c.as_deref(),
            Aggregate::Sum(c) | Aggregate::Avg(c) | Aggregate::Max(c) | Aggregate::Min(c) => {
                Some(c.as_str())
            }
        }
    }

    /// Render to a `sea_query::SimpleExpr` for the SELECT list. Both
    /// backends accept the same function names for the supported set.
    pub fn to_simple_expr(&self) -> SimpleExpr {
        match self {
            Aggregate::Count(None) => Func::count(Expr::col(sea_query::Asterisk)).into(),
            Aggregate::Count(Some(col)) => Func::count(Expr::col(Alias::new(col.as_str()))).into(),
            Aggregate::Sum(col) => Func::sum(Expr::col(Alias::new(col.as_str()))).into(),
            Aggregate::Avg(col) => Func::avg(Expr::col(Alias::new(col.as_str()))).into(),
            Aggregate::Max(col) => Func::max(Expr::col(Alias::new(col.as_str()))).into(),
            Aggregate::Min(col) => Func::min(Expr::col(Alias::new(col.as_str()))).into(),
        }
    }

    /// One of `"count"`, `"sum"`, `"avg"`, `"max"`, `"min"` — used by
    /// the terminal to dispatch row-decoding (COUNT always returns
    /// i64, AVG always returns f64, SUM/MAX/MIN inherit the source
    /// column's type).
    pub fn kind(&self) -> AggregateKind {
        match self {
            Aggregate::Count(_) => AggregateKind::Count,
            Aggregate::Sum(_) => AggregateKind::Sum,
            Aggregate::Avg(_) => AggregateKind::Avg,
            Aggregate::Max(_) => AggregateKind::Max,
            Aggregate::Min(_) => AggregateKind::Min,
        }
    }
}

/// Discriminator for [`Aggregate`]. Carried separately so terminals
/// can pattern-match without pulling string allocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    Count,
    Sum,
    Avg,
    Max,
    Min,
}
