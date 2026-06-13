//! Column types used in QuerySet predicates and ordering.
//!
//! Each column type carries inherent methods like `.eq`, `.ne`, `.lt`,
//! `.like`, `.is_null`, etc. that build `Predicate<T>` values, plus
//! `.asc()` and `.desc()` that build `OrderExpr<T>` values. The model
//! type parameter `T` ties the column to its parent model so a column
//! from `Post` can't be passed to a `QuerySet<Comment>`.
//!
//! M1 covers four column kinds (`IntCol`, `StrCol`, `DateTimeCol`,
//! `NullableDateTimeCol`). More land at M2 when the `Model` trait
//! abstraction goes in.
//!
//! The struct shapes and `::new` constructors are fixed (the sibling
//! `post` module references them). Inherent method implementations
//! were filled in by the M1 ORM fan-out subagent.

use std::marker::PhantomData;

use sea_query::{Alias, Expr, ExprTrait, Func};

use super::{OrderExpr, Predicate};

/// An i64-typed column.
pub struct IntCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> IntCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// // Build a predicate for `id = 2` and pass it to filter.
    /// let _ = Post::objects().filter(post::ID.eq(2));
    /// ```
    pub fn eq(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Mirrors the REST filter
    /// parser's `__lte` lookup name so handler-side code and URL
    /// filters spell the same operation the same way.
    pub fn lte(&self, val: i64) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: i64) -> Predicate<T> {
        self.ge(val)
    }

    /// SQL `IN (...)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// let _ = Post::objects().filter(post::ID.in_(&[1, 2, 3]));
    /// ```
    pub fn in_(&self, vals: &[i64]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_in(vals.iter().copied()))
    }

    /// SQL `<col> IN (SELECT ...)` against a [`super::Subquery`]
    /// built from another QuerySet via `.into_subquery("col")`
    /// (gap #26).
    pub fn in_subquery(&self, sub: super::Subquery) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).in_subquery(sub.into_statement()))
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// // Newest posts first, capped at 20 rows.
    /// let _ = Post::objects().order_by(post::ID.desc()).limit(20);
    /// ```
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A String-typed column.
pub struct StrCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> StrCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// let _ = Post::objects().filter(post::TITLE.eq("Hello world"));
    /// ```
    pub fn eq<S: Into<String>>(&self, val: S) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val.into()))
    }

    /// SQL `<>`.
    pub fn ne<S: Into<String>>(&self, val: S) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val.into()))
    }

    /// SQL `LIKE` (case-sensitive).
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// let _ = Post::objects().filter(post::TITLE.like("Hello%"));
    /// ```
    pub fn like<S: Into<String>>(&self, pattern: S) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).like(pattern.into()))
    }

    /// Case-insensitive `LIKE` via `UPPER(col) LIKE UPPER(pattern)` for
    /// portability across backends without a native `ILIKE`.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// let _ = Post::objects().filter(post::TITLE.ilike("hello%"));
    /// ```
    pub fn ilike<S: Into<String>>(&self, pattern: S) -> Predicate<T> {
        let pattern = pattern.into().to_uppercase();
        Predicate::new(Expr::expr(Func::upper(Expr::col(Alias::new(self.name)))).like(pattern))
    }

    /// SQL `LIKE '%val%'` substring containment.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// let _ = Post::objects().filter(post::TITLE.contains("rust"));
    /// ```
    pub fn contains<S: Into<String>>(&self, substring: S) -> Predicate<T> {
        let pattern = format!("%{}%", substring.into());
        Predicate::new(Expr::col(Alias::new(self.name)).like(pattern))
    }

    /// Case-insensitive substring containment via `UPPER(col) LIKE
    /// UPPER('%val%')`.
    ///
    /// SQLite's `LIKE` is already ASCII-case-insensitive, so `contains`
    /// and `icontains` may return the same rows there. The contract is
    /// "emit `LIKE`"; backend case-sensitivity differs.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// let _ = Post::objects().filter(post::TITLE.icontains("rust"));
    /// ```
    pub fn icontains<S: Into<String>>(&self, substring: S) -> Predicate<T> {
        let pattern = format!("%{}%", substring.into()).to_uppercase();
        Predicate::new(Expr::expr(Func::upper(Expr::col(Alias::new(self.name)))).like(pattern))
    }

    /// SQL `LIKE 'val%'` — prefix match. Mirrors the REST filter
    /// parser's `__startswith` lookup.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// let _ = Post::objects().filter(post::TITLE.startswith("intro"));
    /// ```
    pub fn startswith<S: Into<String>>(&self, prefix: S) -> Predicate<T> {
        let pattern = format!("{}%", prefix.into());
        Predicate::new(Expr::col(Alias::new(self.name)).like(pattern))
    }

    /// Case-insensitive prefix match via `UPPER(col) LIKE UPPER('val%')`.
    pub fn istartswith<S: Into<String>>(&self, prefix: S) -> Predicate<T> {
        let pattern = format!("{}%", prefix.into()).to_uppercase();
        Predicate::new(Expr::expr(Func::upper(Expr::col(Alias::new(self.name)))).like(pattern))
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A `chrono::DateTime<Utc>`-typed column.
pub struct DateTimeCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> DateTimeCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    pub fn eq(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        self.ge(val)
    }

    /// Alias for `.lt`, reading naturally for time.
    pub fn before(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        self.lt(val)
    }

    /// Alias for `.gt`, reading naturally for time.
    pub fn after(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        self.gt(val)
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable `chrono::DateTime<Utc>`-typed column.
pub struct NullableDateTimeCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableDateTimeCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's NULL semantics.
    pub fn eq(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`. NULL rows are excluded by SQL's NULL semantics.
    pub fn ne(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        self.ge(val)
    }

    /// Alias for `.lt`, reading naturally for time.
    ///
    /// # Examples
    ///
    /// ```
    /// use chrono::Utc;
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// let now = Utc::now();
    /// let _ = Post::objects().filter(post::PUBLISHED_AT.before(now));
    /// ```
    pub fn before(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        self.lt(val)
    }

    /// Alias for `.gt`, reading naturally for time.
    pub fn after(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        self.gt(val)
    }

    /// SQL `IS NULL`.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// // Drafts: rows where `published_at` has not been set.
    /// let _ = Post::objects().filter(post::PUBLISHED_AT.is_null());
    /// ```
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    ///
    /// # Examples
    ///
    /// ```
    /// use umbra_core::orm::Post;
    /// use umbra_core::orm::post::post;
    ///
    /// // Published posts only.
    /// let _ = Post::objects().filter(post::PUBLISHED_AT.is_not_null());
    ///
    /// // Compose with `&` for AND: published posts mentioning "rust".
    /// let _ = Post::objects()
    ///     .filter(post::PUBLISHED_AT.is_not_null() & post::TITLE.icontains("rust"));
    /// ```
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

// =========================================================================
//
// M3 type-catalogue refresh: stubs added by the scaffold commit; methods
// filled in by the M3 type-catalogue fan-out subagent A.
//
// Convention for the new types: a struct with `name: &'static str` plus
// `PhantomData<T>`, and a const `::new(&'static str)` constructor.
// Methods (.eq / .ne / .lt / .gt / .le / .ge / .is_null / .is_not_null /
// .asc / .desc / .before / .after / etc.) get added by subagent A so the
// stubs compile cleanly during the parallel phase.
//
// =========================================================================

/// A 64-bit float column (`f64`). Also serves `f32` field declarations
/// because `f32 -> f64` is lossless; the SqlType variant on FieldSpec
/// keeps the original precision distinction (`Real` vs `Double`) so
/// the migration engine renders the right SQL column type.
pub struct F64Col<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> F64Col<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    pub fn eq(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: f64) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: f64) -> Predicate<T> {
        self.ge(val)
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A boolean column.
pub struct BoolCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> BoolCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    pub fn eq(&self, val: bool) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: bool) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// Sugar for `.eq(true)`.
    pub fn is_true(&self) -> Predicate<T> {
        self.eq(true)
    }

    /// Sugar for `.eq(false)`.
    pub fn is_false(&self) -> Predicate<T> {
        self.eq(false)
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A `uuid::Uuid`-typed column.
pub struct UuidCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> UuidCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    pub fn eq(&self, val: uuid::Uuid) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: uuid::Uuid) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `IN (...)`.
    pub fn in_(&self, vals: &[uuid::Uuid]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_in(vals.iter().copied()))
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A `chrono::NaiveDate`-typed column (no time, no timezone).
pub struct DateCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> DateCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    pub fn eq(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: chrono::NaiveDate) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: chrono::NaiveDate) -> Predicate<T> {
        self.ge(val)
    }

    /// Alias for `.lt`, reading naturally for dates.
    pub fn before(&self, val: chrono::NaiveDate) -> Predicate<T> {
        self.lt(val)
    }

    /// Alias for `.gt`, reading naturally for dates.
    pub fn after(&self, val: chrono::NaiveDate) -> Predicate<T> {
        self.gt(val)
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A `chrono::NaiveTime`-typed column (no date, no timezone).
pub struct TimeCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> TimeCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    pub fn eq(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: chrono::NaiveTime) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: chrono::NaiveTime) -> Predicate<T> {
        self.ge(val)
    }

    /// Alias for `.lt`, reading naturally for times.
    pub fn before(&self, val: chrono::NaiveTime) -> Predicate<T> {
        self.lt(val)
    }

    /// Alias for `.gt`, reading naturally for times.
    pub fn after(&self, val: chrono::NaiveTime) -> Predicate<T> {
        self.gt(val)
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

// -------------------------------------------------------------------------
// Nullable variants. Each wraps a base type and adds `.is_null` /
// `.is_not_null`; otherwise the same predicates apply with the same
// signatures. The derive emits these for `Option<T>` fields across the
// catalogue.
// -------------------------------------------------------------------------

/// A nullable `i64`-typed column.
pub struct NullableIntCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableIntCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's NULL semantics.
    pub fn eq(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`. NULL rows are excluded by SQL's NULL semantics.
    pub fn ne(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: i64) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: i64) -> Predicate<T> {
        self.ge(val)
    }

    /// SQL `IN (...)`.
    pub fn in_(&self, vals: &[i64]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_in(vals.iter().copied()))
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable `String`-typed column.
pub struct NullableStrCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableStrCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's NULL semantics.
    pub fn eq<S: Into<String>>(&self, val: S) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val.into()))
    }

    /// SQL `<>`. NULL rows are excluded by SQL's NULL semantics.
    pub fn ne<S: Into<String>>(&self, val: S) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val.into()))
    }

    /// SQL `LIKE` (case-sensitive).
    pub fn like<S: Into<String>>(&self, pattern: S) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).like(pattern.into()))
    }

    /// Case-insensitive `LIKE` via `UPPER(col) LIKE UPPER(pattern)`.
    pub fn ilike<S: Into<String>>(&self, pattern: S) -> Predicate<T> {
        let pattern = pattern.into().to_uppercase();
        Predicate::new(Expr::expr(Func::upper(Expr::col(Alias::new(self.name)))).like(pattern))
    }

    /// SQL `LIKE '%val%'` substring containment.
    pub fn contains<S: Into<String>>(&self, substring: S) -> Predicate<T> {
        let pattern = format!("%{}%", substring.into());
        Predicate::new(Expr::col(Alias::new(self.name)).like(pattern))
    }

    /// Case-insensitive substring containment via `UPPER(col) LIKE
    /// UPPER('%val%')`.
    pub fn icontains<S: Into<String>>(&self, substring: S) -> Predicate<T> {
        let pattern = format!("%{}%", substring.into()).to_uppercase();
        Predicate::new(Expr::expr(Func::upper(Expr::col(Alias::new(self.name)))).like(pattern))
    }

    /// SQL `LIKE 'val%'` — prefix match. Mirrors the REST filter
    /// parser's `__startswith` lookup.
    pub fn startswith<S: Into<String>>(&self, prefix: S) -> Predicate<T> {
        let pattern = format!("{}%", prefix.into());
        Predicate::new(Expr::col(Alias::new(self.name)).like(pattern))
    }

    /// Case-insensitive prefix match via `UPPER(col) LIKE UPPER('val%')`.
    pub fn istartswith<S: Into<String>>(&self, prefix: S) -> Predicate<T> {
        let pattern = format!("{}%", prefix.into()).to_uppercase();
        Predicate::new(Expr::expr(Func::upper(Expr::col(Alias::new(self.name)))).like(pattern))
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable `f64`-typed column.
pub struct NullableF64Col<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableF64Col<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's NULL semantics.
    pub fn eq(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`. NULL rows are excluded by SQL's NULL semantics.
    pub fn ne(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: f64) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: f64) -> Predicate<T> {
        self.ge(val)
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable `bool`-typed column.
pub struct NullableBoolCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableBoolCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's NULL semantics.
    pub fn eq(&self, val: bool) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`. NULL rows are excluded by SQL's NULL semantics.
    pub fn ne(&self, val: bool) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// Sugar for `.eq(true)`.
    pub fn is_true(&self) -> Predicate<T> {
        self.eq(true)
    }

    /// Sugar for `.eq(false)`.
    pub fn is_false(&self) -> Predicate<T> {
        self.eq(false)
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable `uuid::Uuid`-typed column.
pub struct NullableUuidCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableUuidCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's NULL semantics.
    pub fn eq(&self, val: uuid::Uuid) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`. NULL rows are excluded by SQL's NULL semantics.
    pub fn ne(&self, val: uuid::Uuid) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `IN (...)`.
    pub fn in_(&self, vals: &[uuid::Uuid]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_in(vals.iter().copied()))
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable `chrono::NaiveDate`-typed column.
pub struct NullableDateCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableDateCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's NULL semantics.
    pub fn eq(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`. NULL rows are excluded by SQL's NULL semantics.
    pub fn ne(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: chrono::NaiveDate) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: chrono::NaiveDate) -> Predicate<T> {
        self.ge(val)
    }

    /// Alias for `.lt`, reading naturally for dates.
    pub fn before(&self, val: chrono::NaiveDate) -> Predicate<T> {
        self.lt(val)
    }

    /// Alias for `.gt`, reading naturally for dates.
    pub fn after(&self, val: chrono::NaiveDate) -> Predicate<T> {
        self.gt(val)
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable `chrono::NaiveTime`-typed column.
pub struct NullableTimeCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableTimeCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's NULL semantics.
    pub fn eq(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`. NULL rows are excluded by SQL's NULL semantics.
    pub fn ne(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: chrono::NaiveTime) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: chrono::NaiveTime) -> Predicate<T> {
        self.ge(val)
    }

    /// Alias for `.lt`, reading naturally for times.
    pub fn before(&self, val: chrono::NaiveTime) -> Predicate<T> {
        self.lt(val)
    }

    /// Alias for `.gt`, reading naturally for times.
    pub fn after(&self, val: chrono::NaiveTime) -> Predicate<T> {
        self.gt(val)
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

// =========================================================================
// Json columns (`serde_json::Value`).
//
// The first iteration of Phase 4. JSON value comparison is semantically
// non-trivial across backends — Postgres has `=` for jsonb (deep
// equality with key-order normalization), SQLite as TEXT compares
// strings literally and so depends on how the value was serialized.
// To avoid shipping a half-thought comparison story, the first
// iteration covers only `IS NULL` / `IS NOT NULL` predicates plus the
// usual ordering ops. Equality / containment / path-access operators
// land as a follow-on once the cross-backend semantics are pinned.
// =========================================================================

/// A `serde_json::Value`-typed column.
pub struct JsonCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> JsonCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `ORDER BY ... ASC`. Ordering on JSON values is well-defined
    /// per-backend (Postgres has a total order on jsonb; SQLite orders
    /// the underlying TEXT). Use sparingly — JSON ordering is rarely
    /// what the user means.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }

    /// Extract a JSON path as text. Postgres-only.
    ///
    /// ```ignore
    /// post::METADATA.path_text(&["author", "name"]).eq("alice")
    /// ```
    ///
    /// Renders as `"metadata" -> 'author' ->> 'name' = 'alice'` when
    /// the QuerySet is bound to a `PgPool`. The path must have at
    /// least one segment; an empty path panics at construction.
    ///
    /// See [`JsonPathText`] for the chainable surface.
    pub fn path_text(&self, keys: &[&str]) -> JsonPathText<T> {
        JsonPathText::new(self.name, keys)
    }

    /// Postgres `"col" ? key` — true when the JSON object has the
    /// given top-level key. Returns `Predicate<T>` directly (no
    /// chainable form yet — `has_key` is a complete boolean op).
    /// The key is single-quoted into the SQL fragment; standard SQL
    /// apostrophe escaping is applied.
    pub fn has_key(&self, key: &str) -> Predicate<T> {
        json_has_key_predicate(self.name, key)
    }
}

/// A nullable `serde_json::Value`-typed column.
pub struct NullableJsonCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableJsonCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }

    /// See [`JsonCol::path_text`]. NULL columns extract NULL through
    /// the operator — SQL's three-valued logic excludes them from
    /// equality predicates naturally.
    pub fn path_text(&self, keys: &[&str]) -> JsonPathText<T> {
        JsonPathText::new(self.name, keys)
    }

    /// See [`JsonCol::has_key`].
    pub fn has_key(&self, key: &str) -> Predicate<T> {
        json_has_key_predicate(self.name, key)
    }
}

// =========================================================================
// JSON operators — Phase 4.2, Postgres-only.
//
// `path_text(&["a", "b"])` returns a `JsonPathText<T>` builder that
// chains into a predicate via `.eq` / `.ne` / `.is_null` / `.is_not_null`.
// `has_key("k")` returns a Predicate<T> directly.
//
// The SQL templates use `$N` placeholders and resolve correctly only
// under PostgresQueryBuilder. `to_sql_pg()` is the right debug entry
// for these predicates; `to_sql()` (SQLite builder) leaves `$N` tokens
// literal. The user-facing docs and the Phase 4.0 Json field rustdoc
// both call out that operators are deferred for SQLite; Phase 4.2.1
// is the slot where the SQLite JSON1 fallback lands.
// =========================================================================

/// An expression that extracts a deeply-nested JSON value as text.
/// Produced by [`JsonCol::path_text`] / [`NullableJsonCol::path_text`]
/// and consumed by `.eq` / `.ne` / `.is_null` / `.is_not_null` to
/// produce a `Predicate<T>`.
///
/// The extraction renders to Postgres' chained `->` / `->>` operator
/// form: a path of length `n` produces `n-1` `->` steps and one final
/// `->>` step that returns text. Single-key paths use a single `->>`.
/// Empty paths would have nothing to extract — `path_text(&[])` panics
/// (constructor-level invariant; an empty path is a programmer bug,
/// not a runtime user input).
pub struct JsonPathText<T> {
    column: &'static str,
    /// Path segments, ordered root-to-leaf. Owned strings so the
    /// builder can be passed around without lifetime contortions.
    path: Vec<String>,
    _phantom: PhantomData<T>,
}

impl<T> JsonPathText<T> {
    fn new(column: &'static str, keys: &[&str]) -> Self {
        assert!(
            !keys.is_empty(),
            "umbra::orm::JsonPathText: path must have at least one segment"
        );
        Self {
            column,
            path: keys.iter().map(|s| s.to_string()).collect(),
            _phantom: PhantomData,
        }
    }

    /// Render the Postgres `"col" -> $1 -> $2 ->> $N` template for a
    /// path of length `n`. Returns the SQL string and the path-segment
    /// Values (in order). The caller appends comparison fragments and
    /// binds additional values.
    fn extract_template_pg(&self, base_placeholder: usize) -> (String, Vec<sea_query::Value>) {
        let col = self.column.replace('"', "\"\"");
        let n = self.path.len();
        let mut sql = format!("\"{col}\"");
        for i in 1..n {
            sql.push_str(&format!(" -> ${}", base_placeholder + i - 1));
        }
        sql.push_str(&format!(" ->> ${}", base_placeholder + n - 1));
        let values: Vec<sea_query::Value> = self
            .path
            .iter()
            .map(|k| sea_query::Value::String(Some(Box::new(k.clone()))))
            .collect();
        (sql, values)
    }

    /// Build the SQLite JSON1 path string `$.a.b.c` for the stored
    /// path. v1 uses dot-notation; users with quoted keys or array
    /// indexes hand-roll the path as the SQLite JSON1 bracket form.
    fn sqlite_json_path(&self) -> String {
        let mut s = String::from("$");
        for seg in &self.path {
            s.push('.');
            s.push_str(seg);
        }
        s
    }

    /// SQL `<extracted> = $val`. Backend-aware:
    /// - **Postgres**: `"col" -> 'a' ->> 'b' = $val`
    /// - **SQLite**: `json_extract("col", '$.a.b') = ?`
    pub fn eq(&self, val: &str) -> Predicate<T> {
        let (extract_pg, mut pg_values) = self.extract_template_pg(1);
        let pg_placeholder = pg_values.len() + 1;
        let pg_sql = format!("{extract_pg} = ${pg_placeholder}");
        pg_values.push(sea_query::Value::String(Some(Box::new(val.to_string()))));
        let pg_cond = Expr::cust_with_values(&pg_sql, pg_values);

        let col = self.column.replace('"', "\"\"");
        let sqlite_sql = format!("json_extract(\"{col}\", ?) = ?");
        let sqlite_values = vec![
            sea_query::Value::String(Some(Box::new(self.sqlite_json_path()))),
            sea_query::Value::String(Some(Box::new(val.to_string()))),
        ];
        let sqlite_cond = Expr::cust_with_values(&sqlite_sql, sqlite_values);

        Predicate::new_with_sqlite(pg_cond, sqlite_cond)
    }

    /// SQL `<extracted> <> $val`. Backend-aware (see [`Self::eq`]).
    pub fn ne(&self, val: &str) -> Predicate<T> {
        let (extract_pg, mut pg_values) = self.extract_template_pg(1);
        let pg_placeholder = pg_values.len() + 1;
        let pg_sql = format!("{extract_pg} <> ${pg_placeholder}");
        pg_values.push(sea_query::Value::String(Some(Box::new(val.to_string()))));
        let pg_cond = Expr::cust_with_values(&pg_sql, pg_values);

        let col = self.column.replace('"', "\"\"");
        let sqlite_sql = format!("json_extract(\"{col}\", ?) <> ?");
        let sqlite_values = vec![
            sea_query::Value::String(Some(Box::new(self.sqlite_json_path()))),
            sea_query::Value::String(Some(Box::new(val.to_string()))),
        ];
        let sqlite_cond = Expr::cust_with_values(&sqlite_sql, sqlite_values);

        Predicate::new_with_sqlite(pg_cond, sqlite_cond)
    }

    /// SQL `<extracted> IS NULL`. Backend-aware. Both renderings
    /// produce NULL when the column itself is NULL OR the path
    /// misses a key.
    pub fn is_null(&self) -> Predicate<T> {
        let (extract_pg, pg_values) = self.extract_template_pg(1);
        let pg_cond = Expr::cust_with_values(format!("{extract_pg} IS NULL"), pg_values);

        let col = self.column.replace('"', "\"\"");
        let sqlite_sql = format!("json_extract(\"{col}\", ?) IS NULL");
        let sqlite_values = vec![sea_query::Value::String(Some(Box::new(
            self.sqlite_json_path(),
        )))];
        let sqlite_cond = Expr::cust_with_values(&sqlite_sql, sqlite_values);

        Predicate::new_with_sqlite(pg_cond, sqlite_cond)
    }

    /// SQL `<extracted> IS NOT NULL`. Backend-aware (see
    /// [`Self::is_null`]).
    pub fn is_not_null(&self) -> Predicate<T> {
        let (extract_pg, pg_values) = self.extract_template_pg(1);
        let pg_cond = Expr::cust_with_values(format!("{extract_pg} IS NOT NULL"), pg_values);

        let col = self.column.replace('"', "\"\"");
        let sqlite_sql = format!("json_extract(\"{col}\", ?) IS NOT NULL");
        let sqlite_values = vec![sea_query::Value::String(Some(Box::new(
            self.sqlite_json_path(),
        )))];
        let sqlite_cond = Expr::cust_with_values(&sqlite_sql, sqlite_values);

        Predicate::new_with_sqlite(pg_cond, sqlite_cond)
    }
}

/// Build a `"col" ? $1` predicate — Postgres's "has top-level key"
/// operator. Shared between JsonCol and NullableJsonCol so both
/// expose the same surface. Postgres-only; the `?` token is sea-
/// query's positional placeholder for SQLite, so the template uses
/// the explicit `?` (which Postgres builder will leave alone, but
/// sea-query's `cust_with_values` interprets — that means we can't
/// use literal `?` here. We use the `\?` escape or build the SQL
/// directly).
fn json_has_key_predicate<T>(col: &'static str, key: &str) -> Predicate<T> {
    let col_escaped = col.replace('"', "\"\"");
    let key_escaped = key.replace('\'', "''");

    // Postgres: native `?` has-key operator. sea-query's
    // `cust_with_values` uses `?` and `$` as placeholder tokens, so
    // we double the `?` to emit a literal one. The key is inline
    // single-quoted (no binding).
    let pg_sql = format!("\"{col_escaped}\" ?? '{key_escaped}'");
    let pg_cond = Expr::cust(&pg_sql);

    // SQLite JSON1: there's no native has-key operator. The closest
    // semantic match is `json_extract(col, '$.key') IS NOT NULL` —
    // true when the key exists with a non-null value, false when
    // missing OR explicitly null. The Postgres `?` operator returns
    // true on `{"k": null}`; SQLite's fallback returns false. The
    // diverging-on-explicit-null case is documented; users with
    // strict "key present even if value is null" needs hand-roll the
    // SQLite SQL.
    let sqlite_sql = format!("json_extract(\"{col_escaped}\", ?) IS NOT NULL");
    let sqlite_values = vec![sea_query::Value::String(Some(Box::new(format!("$.{key}"))))];
    let sqlite_cond = Expr::cust_with_values(&sqlite_sql, sqlite_values);

    Predicate::new_with_sqlite(pg_cond, sqlite_cond)
}

// =========================================================================
// Array columns — Phase 4.1, Postgres-only.
//
// v1 surface: ordering ops (asc/desc) and IS NULL / IS NOT NULL for
// the nullable variant. Array-specific operators (`@>` contains,
// `<@` contained-by, `&&` overlaps, `array_length`, `unnest`) land
// as a follow-on. The element type is *not* a generic parameter on
// the column struct itself — the predicate methods we ship today
// don't need to know it, and adding a type parameter would force the
// derive macro to plumb it through every column-const declaration
// (each user struct's sibling module would gain an extra type arg).
// When the per-element operators land, the element type comes via a
// const associated value on the column or via the element ops as
// generics on a single method.
// =========================================================================

/// A `Vec<T>`-typed column (Postgres array).
pub struct ArrayCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> ArrayCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `ORDER BY ... ASC`. Postgres array ordering is element-wise
    /// lexicographic — rarely what the user wants, but well-defined.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }

    /// SQL `col @> ARRAY[elem]` (Postgres contains).
    ///
    /// Returns `true` if every element of `ARRAY[elem]` is present in
    /// the column's array — i.e. `elem` appears in the array. Use
    /// [`Self::contains_all`] when checking multiple elements at once.
    ///
    /// Postgres-only. ArrayCol is system-check-gated against SQLite, so
    /// the SQL fragment this emits only ever renders against a
    /// PostgresQueryBuilder.
    pub fn contains<V: Into<sea_query::Value>>(&self, elem: V) -> Predicate<T> {
        array_contains_predicate(self.name, std::iter::once(elem.into()))
    }

    /// SQL `col @> ARRAY[elems...]` (Postgres contains-all).
    ///
    /// Returns `true` if every element of `elems` is present in the
    /// column's array. An empty `elems` returns vacuously `true` (the
    /// empty set is contained by every set), which Postgres also
    /// reports — but the renderer requires at least one element to
    /// produce a typed `ARRAY[...]` literal; passing an empty iterator
    /// returns a tautology predicate (`1 = 1`).
    pub fn contains_all<I, V>(&self, elems: I) -> Predicate<T>
    where
        I: IntoIterator<Item = V>,
        V: Into<sea_query::Value>,
    {
        array_contains_predicate(self.name, elems.into_iter().map(Into::into))
    }

    /// SQL `col <@ ARRAY[elems...]` (Postgres contained-by).
    ///
    /// Returns `true` if every element of the column's array is in
    /// `elems` — i.e. the column is a subset of the supplied set.
    pub fn contained_by<I, V>(&self, elems: I) -> Predicate<T>
    where
        I: IntoIterator<Item = V>,
        V: Into<sea_query::Value>,
    {
        array_contained_by_predicate(self.name, elems.into_iter().map(Into::into))
    }

    /// SQL `col && ARRAY[elems...]` (Postgres overlaps).
    ///
    /// Returns `true` if the column's array and `elems` share at least
    /// one element.
    pub fn overlaps<I, V>(&self, elems: I) -> Predicate<T>
    where
        I: IntoIterator<Item = V>,
        V: Into<sea_query::Value>,
    {
        array_overlaps_predicate(self.name, elems.into_iter().map(Into::into))
    }
}

/// A nullable `Vec<T>`-typed column.
pub struct NullableArrayCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableArrayCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `IS NULL`. Note this is "the column is NULL", not "the
    /// array is empty" — Postgres distinguishes them. The empty-array
    /// predicate lands with the `array_length` op in a follow-on.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }

    /// See [`ArrayCol::contains`]. NULL columns are excluded by SQL's
    /// three-valued logic — same as every other column predicate.
    pub fn contains<V: Into<sea_query::Value>>(&self, elem: V) -> Predicate<T> {
        array_contains_predicate(self.name, std::iter::once(elem.into()))
    }

    /// See [`ArrayCol::contains_all`].
    pub fn contains_all<I, V>(&self, elems: I) -> Predicate<T>
    where
        I: IntoIterator<Item = V>,
        V: Into<sea_query::Value>,
    {
        array_contains_predicate(self.name, elems.into_iter().map(Into::into))
    }

    /// See [`ArrayCol::contained_by`].
    pub fn contained_by<I, V>(&self, elems: I) -> Predicate<T>
    where
        I: IntoIterator<Item = V>,
        V: Into<sea_query::Value>,
    {
        array_contained_by_predicate(self.name, elems.into_iter().map(Into::into))
    }

    /// See [`ArrayCol::overlaps`].
    pub fn overlaps<I, V>(&self, elems: I) -> Predicate<T>
    where
        I: IntoIterator<Item = V>,
        V: Into<sea_query::Value>,
    {
        array_overlaps_predicate(self.name, elems.into_iter().map(Into::into))
    }
}

// =========================================================================
// Internal helpers: array operator predicates.
//
// The three operators share the same shape — `"col" OP ARRAY[$1, $2,
// ...]` — and differ only by the operator string. Factored so the
// ArrayCol and NullableArrayCol impls stay short.
//
// Each helper builds a `sea_query::Expr::cust_with_values` SimpleExpr.
// The column identifier is quoted into the SQL template (Postgres
// double-quote escaping); the elements bind through sea-query's value
// list. Empty element lists return a tautology (`1 = 1`) or a
// guaranteed-false predicate as appropriate, so the caller doesn't
// have to special-case empty input.
//
// **Postgres-only.** ArrayCol is system-check-gated against SQLite, so
// these fragments only ever render against PostgresQueryBuilder.
// =========================================================================

fn array_op_predicate<T>(
    col: &'static str,
    op: &str,
    values: Vec<sea_query::Value>,
) -> Predicate<T> {
    if values.is_empty() {
        // Render as a constant boolean. `1 = 1` is true; `1 = 0` false.
        // Each operator picks the right tautology in the caller.
        return Predicate::new(Expr::cust("1 = 1"));
    }
    let placeholders: Vec<String> = (1..=values.len()).map(|i| format!("${i}")).collect();
    let sql = format!(
        "\"{}\" {op} ARRAY[{}]",
        col.replace('"', "\"\""),
        placeholders.join(", ")
    );
    Predicate::new(Expr::cust_with_values(&sql, values))
}

fn array_contains_predicate<T, I>(col: &'static str, elems: I) -> Predicate<T>
where
    I: IntoIterator<Item = sea_query::Value>,
{
    // `col @> ARRAY[]` is vacuously true on Postgres (empty set is
    // contained by every set). Render as 1 = 1 to keep the QuerySet
    // simple and predictable.
    array_op_predicate::<T>(col, "@>", elems.into_iter().collect())
}

fn array_contained_by_predicate<T, I>(col: &'static str, elems: I) -> Predicate<T>
where
    I: IntoIterator<Item = sea_query::Value>,
{
    let values: Vec<sea_query::Value> = elems.into_iter().collect();
    if values.is_empty() {
        // `col <@ ARRAY[]` is true only when `col` is empty or NULL;
        // 1 = 1 isn't right here. Use a guaranteed-false predicate
        // so the caller sees zero rows for "subset of nothing" — the
        // honest answer when the column has any rows at all. The
        // empty-array-equality check belongs in a future `len()`
        // op.
        return Predicate::new(Expr::cust("1 = 0"));
    }
    array_op_predicate::<T>(col, "<@", values)
}

fn array_overlaps_predicate<T, I>(col: &'static str, elems: I) -> Predicate<T>
where
    I: IntoIterator<Item = sea_query::Value>,
{
    let values: Vec<sea_query::Value> = elems.into_iter().collect();
    if values.is_empty() {
        // Empty set overlaps nothing; predicate is always false.
        return Predicate::new(Expr::cust("1 = 0"));
    }
    array_op_predicate::<T>(col, "&&", values)
}

// =========================================================================
// Network address columns — Phase 4.4, Postgres-only.
//
// Three pairs: `InetCol` / `NullableInetCol` for INET (`ipnetwork::
// IpNetwork`); `CidrCol` / `NullableCidrCol` for CIDR (same Rust type
// as Inet, just constrained to a network address); `MacAddrCol` /
// `NullableMacAddrCol` for MACADDR (`mac_address::MacAddress`).
//
// v1 surface: equality / inequality, `IS NULL` / `IS NOT NULL` on the
// nullable variants, plus the standard `asc()` / `desc()`. Network-
// specific operators (`<<`, `>>`, `&`, `|` on inet types; `<<=` /
// `>>=` for containment; `~` for MAC ranges) are deferred until a
// real consumer surfaces them.
//
// Each `Col::eq(val)` takes the Rust binding type by value. sea-query
// has built-in `Value::IpNetwork` and `Value::MacAddress` variants
// (gated behind sqlx feature flags we've enabled on sea-query-binder
// via the `with-ipnetwork` / `with-mac_address` route — sqlx pulls
// the same types through and they implement `Into<sea_query::Value>`).
// =========================================================================

/// An `ipnetwork::IpNetwork`-typed column (Postgres INET).
pub struct InetCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> InetCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    pub fn eq(&self, val: ipnetwork::IpNetwork) -> Predicate<T> {
        // sea-query doesn't expose `Into<Value>` for `IpNetwork` from
        // the `ipnetwork` crate directly; render the comparison via
        // `cust_with_values` with the value bound positionally.
        let sql = format!("\"{}\" = $1", self.name.replace('"', "\"\""));
        // sea_query::Value carries an IpNetwork variant when its
        // `with-ipnetwork` feature is enabled; cast through the
        // `Into` impl.
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: ipnetwork::IpNetwork) -> Predicate<T> {
        let sql = format!("\"{}\" <> $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable INET column.
pub struct NullableInetCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableInetCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's three-valued logic.
    pub fn eq(&self, val: ipnetwork::IpNetwork) -> Predicate<T> {
        let sql = format!("\"{}\" = $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: ipnetwork::IpNetwork) -> Predicate<T> {
        let sql = format!("\"{}\" <> $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// An `ipnetwork::IpNetwork`-typed column declared as a Postgres CIDR.
///
/// Same Rust binding type as [`InetCol`]; the DDL renders as `cidr`
/// (with the host-bits-zero constraint Postgres enforces). For
/// general host-address storage, use `InetCol`.
pub struct CidrCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> CidrCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    pub fn eq(&self, val: ipnetwork::IpNetwork) -> Predicate<T> {
        let sql = format!("\"{}\" = $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    pub fn ne(&self, val: ipnetwork::IpNetwork) -> Predicate<T> {
        let sql = format!("\"{}\" <> $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable CIDR column.
pub struct NullableCidrCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableCidrCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    pub fn eq(&self, val: ipnetwork::IpNetwork) -> Predicate<T> {
        let sql = format!("\"{}\" = $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    pub fn ne(&self, val: ipnetwork::IpNetwork) -> Predicate<T> {
        let sql = format!("\"{}\" <> $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A `mac_address::MacAddress`-typed column (Postgres MACADDR).
pub struct MacAddrCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> MacAddrCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    pub fn eq(&self, val: mac_address::MacAddress) -> Predicate<T> {
        let sql = format!("\"{}\" = $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    pub fn ne(&self, val: mac_address::MacAddress) -> Predicate<T> {
        let sql = format!("\"{}\" <> $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

// =========================================================================
// Full-text search columns — Phase 4.3, Postgres-only.
//
// `FullTextCol<T>` / `NullableFullTextCol<T>` wrap a Postgres
// `tsvector` column. v1 surface: `matches(query)` for plain
// `to_tsquery` matching, `matches_websearch(query)` for the more
// permissive `websearch_to_tsquery` form (handles user-typed search
// strings with quoted phrases, OR, etc.). Storage is a text vector;
// the column is typically populated via Postgres trigger or
// GENERATED ALWAYS clause — umbra's migration engine emits the bare
// `tsvector` declaration and leaves the population to the user.
// =========================================================================

/// A `umbra::orm::TsVector`-typed column (Postgres tsvector).
pub struct FullTextCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> FullTextCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `col @@ to_tsquery($1)`. The query string follows
    /// Postgres's `to_tsquery` syntax: `&` AND, `|` OR, `!` NOT,
    /// `:*` prefix match. Strict — malformed queries error at the
    /// server.
    pub fn matches(&self, query: &str) -> Predicate<T> {
        let col = self.name.replace('"', "\"\"");
        let sql = format!("\"{col}\" @@ to_tsquery($1)");
        let values = vec![sea_query::Value::String(Some(Box::new(query.to_string())))];
        Predicate::new(Expr::cust_with_values(&sql, values))
    }

    /// SQL `col @@ websearch_to_tsquery($1)`. The query string follows
    /// web-search conventions: space-separated terms (AND), `OR`,
    /// `-term` for negation, `"quoted phrase"` for adjacency. More
    /// forgiving than [`Self::matches`].
    pub fn matches_websearch(&self, query: &str) -> Predicate<T> {
        let col = self.name.replace('"', "\"\"");
        let sql = format!("\"{col}\" @@ websearch_to_tsquery($1)");
        let values = vec![sea_query::Value::String(Some(Box::new(query.to_string())))];
        Predicate::new(Expr::cust_with_values(&sql, values))
    }

    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable tsvector column.
pub struct NullableFullTextCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableFullTextCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    pub fn matches(&self, query: &str) -> Predicate<T> {
        let col = self.name.replace('"', "\"\"");
        let sql = format!("\"{col}\" @@ to_tsquery($1)");
        let values = vec![sea_query::Value::String(Some(Box::new(query.to_string())))];
        Predicate::new(Expr::cust_with_values(&sql, values))
    }

    pub fn matches_websearch(&self, query: &str) -> Predicate<T> {
        let col = self.name.replace('"', "\"\"");
        let sql = format!("\"{col}\" @@ websearch_to_tsquery($1)");
        let values = vec![sea_query::Value::String(Some(Box::new(query.to_string())))];
        Predicate::new(Expr::cust_with_values(&sql, values))
    }

    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable MACADDR column.
pub struct NullableMacAddrCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableMacAddrCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    pub fn eq(&self, val: mac_address::MacAddress) -> Predicate<T> {
        let sql = format!("\"{}\" = $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    pub fn ne(&self, val: mac_address::MacAddress) -> Predicate<T> {
        let sql = format!("\"{}\" <> $1", self.name.replace('"', "\"\""));
        Predicate::new(Expr::cust_with_values(&sql, vec![val]))
    }

    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

// =========================================================================
// Foreign-key columns — gap 14.
//
// `ForeignKeyCol<T>` is the column type emitted by `#[derive(Model)]` for
// fields of type `ForeignKey<U>`. Because `ForeignKey<U>` is stored as
// `i64` in SQL, the predicate surface is identical to `IntCol<T>`: equality,
// inequality, range comparisons, and `IN`. Ordering and `ASC` / `DESC` are
// also present.
//
// The `T` phantom parameter ties the column to its *owning* model (as every
// column type does); the referenced model type lives only in the Rust field
// declaration and is erased at the column-constant level.
// =========================================================================

/// A foreign-key column — stored as `i64`, referencing the primary key of
/// another model's table.
pub struct ForeignKeyCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> ForeignKeyCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    ///
    /// Accepts any value convertible to `sea_query::Value` — i64 for
    /// the common autoincrement-PK case, String for slug-keyed
    /// parents (`umbra-permissions::Permission.codename`), Uuid for
    /// UUID-keyed models. The type bound is permissive so reverse-FK
    /// accessors (gap #30) emitted by the derive macro can pass the
    /// parent's `Model::PrimaryKey` directly regardless of width.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// Post::objects().filter(post::AUTHOR.eq(1));
    /// UserGroup::objects().filter(usergroup::GROUP_ID.eq(group.id));
    /// ```
    pub fn eq<V: Into<sea_query::Value>>(&self, val: V) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val.into()))
    }

    /// SQL `<>`. See [`Self::eq`] for the type bound rationale.
    pub fn ne<V: Into<sea_query::Value>>(&self, val: V) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val.into()))
    }

    /// SQL `<`.
    pub fn lt(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`]. Same as `__lte` in URL
    /// filter strings.
    pub fn lte(&self, val: i64) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`]. Same as `__gte` in URL
    /// filter strings.
    pub fn gte(&self, val: i64) -> Predicate<T> {
        self.ge(val)
    }

    /// SQL `IN (...)`.
    pub fn in_(&self, vals: &[i64]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_in(vals.iter().copied()))
    }

    /// SQL `<col> IN (SELECT ...)` against a [`super::Subquery`]
    /// (gap #26). See [`IntCol::in_subquery`].
    pub fn in_subquery(&self, sub: super::Subquery) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).in_subquery(sub.into_statement()))
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// A nullable foreign-key column — the `Option<ForeignKey<U>>` shape.
pub struct NullableForeignKeyCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableForeignKeyCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. NULL rows are excluded by SQL's NULL semantics.
    pub fn eq(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `IN (...)`.
    pub fn in_(&self, vals: &[i64]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_in(vals.iter().copied()))
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

// =============================================================================
// BytesCol — Vec<u8> / BLOB / BYTEA columns.
// =============================================================================

/// A `BLOB` (SQLite) / `BYTEA` (Postgres) column carrying arbitrary bytes.
/// The Rust field type is `Vec<u8>`. v1 ships equality + null-checks + ordering;
/// the operator surface is intentionally small because byte columns rarely
/// appear in WHERE clauses (think file payloads, cache values, encrypted
/// envelopes).
pub struct BytesCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> BytesCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`. Borrows the byte slice into a sea_query Value.
    pub fn eq(&self, val: &[u8]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val.to_vec()))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: &[u8]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val.to_vec()))
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

/// `Option<Vec<u8>>` column. Same surface plus `is_null` / `is_not_null`.
pub struct NullableBytesCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> NullableBytesCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    pub fn eq(&self, val: &[u8]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val.to_vec()))
    }

    pub fn ne(&self, val: &[u8]) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val.to_vec()))
    }

    /// SQL `IS NULL`.
    pub fn is_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_null())
    }

    /// SQL `IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).is_not_null())
    }

    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

// =============================================================================
// DecimalCol — rust_decimal::Decimal / NUMERIC(19, 4) columns.
// =============================================================================

/// A fixed-point `NUMERIC(19, 4)` column carrying `rust_decimal::Decimal`.
/// Decimal is Postgres-only at v1, but the predicate surface follows the
/// numeric columns: comparisons, equality, and ordering.
pub struct DecimalCol<T> {
    pub(crate) name: &'static str,
    _phantom: PhantomData<T>,
}

impl<T> DecimalCol<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _phantom: PhantomData,
        }
    }

    /// SQL `=`.
    pub fn eq(&self, val: rust_decimal::Decimal) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).eq(val))
    }

    /// SQL `<>`.
    pub fn ne(&self, val: rust_decimal::Decimal) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).ne(val))
    }

    /// SQL `<`.
    pub fn lt(&self, val: rust_decimal::Decimal) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lt(val))
    }

    /// SQL `<=`.
    pub fn le(&self, val: rust_decimal::Decimal) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).lte(val))
    }

    /// Django-style alias for [`Self::le`].
    pub fn lte(&self, val: rust_decimal::Decimal) -> Predicate<T> {
        self.le(val)
    }

    /// SQL `>`.
    pub fn gt(&self, val: rust_decimal::Decimal) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: rust_decimal::Decimal) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
    }

    /// Django-style alias for [`Self::ge`].
    pub fn gte(&self, val: rust_decimal::Decimal) -> Predicate<T> {
        self.ge(val)
    }

    /// SQL `ORDER BY ... ASC`.
    pub fn asc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, false)
    }

    /// SQL `ORDER BY ... DESC`.
    pub fn desc(&self) -> OrderExpr<T> {
        OrderExpr::new(self.name, true)
    }
}

// =========================================================================
// Gap #24 + #36 — DB-function helpers (`ColExpr<T>`)
//
// Column extension methods (`StrCol::lower`, `DateTimeCol::year`, ...)
// return a `ColExpr<T>` so the caller can pick the comparison
// operator: `post::TITLE.lower().eq(...)`,
// `post::CREATED_AT.year().lt(2026)`. `ColExpr<T>` carries a primary
// `SimpleExpr` plus an optional SQLite-specific override (same
// dual-rendering pattern `Predicate<T>` uses); date-extract needs
// this so it can emit `EXTRACT(YEAR FROM …)` on Postgres and
// `CAST(strftime('%Y', …) AS INTEGER)` on SQLite from a single
// `ColExpr`.
// =========================================================================

/// A backend-aware expression that hasn't been compared yet. Built by
/// the column extension methods (`.lower()`, `.year()`, etc.) and
/// finalised by calling a comparison operator (`.eq`, `.lt`, etc.) to
/// produce a `Predicate<T>`.
pub struct ColExpr<T> {
    expr: sea_query::SimpleExpr,
    expr_sqlite: Option<sea_query::SimpleExpr>,
    _phantom: PhantomData<T>,
}

impl<T> ColExpr<T> {
    /// Construct a single-form expression (same SQL on every backend).
    pub(crate) fn new(expr: sea_query::SimpleExpr) -> Self {
        Self {
            expr,
            expr_sqlite: None,
            _phantom: PhantomData,
        }
    }

    /// Construct an expression that renders differently on SQLite vs
    /// Postgres. The default `expr` is the Postgres form; `sqlite` is
    /// substituted at terminal time when the resolved pool is SQLite.
    pub(crate) fn new_with_sqlite(
        expr: sea_query::SimpleExpr,
        sqlite: sea_query::SimpleExpr,
    ) -> Self {
        Self {
            expr,
            expr_sqlite: Some(sqlite),
            _phantom: PhantomData,
        }
    }

    /// Internal: build a `Predicate` by applying the supplied
    /// operator to both expression forms in parallel.
    fn into_predicate<F>(self, op: F) -> Predicate<T>
    where
        F: Fn(sea_query::SimpleExpr) -> sea_query::SimpleExpr,
    {
        let cond = op(self.expr);
        let cond_sqlite = self.expr_sqlite.map(&op);
        match cond_sqlite {
            Some(sql) => Predicate::new_with_sqlite(cond, sql),
            None => Predicate::new(cond),
        }
    }

    /// `<expr> = value`.
    pub fn eq<V: Into<sea_query::Value>>(self, val: V) -> Predicate<T> {
        let val = val.into();
        self.into_predicate(move |e| e.eq(val.clone()))
    }

    /// `<expr> <> value`.
    pub fn ne<V: Into<sea_query::Value>>(self, val: V) -> Predicate<T> {
        let val = val.into();
        self.into_predicate(move |e| e.ne(val.clone()))
    }

    /// `<expr> < value`.
    pub fn lt<V: Into<sea_query::Value>>(self, val: V) -> Predicate<T> {
        let val = val.into();
        self.into_predicate(move |e| e.lt(val.clone()))
    }

    /// `<expr> <= value`.
    pub fn le<V: Into<sea_query::Value>>(self, val: V) -> Predicate<T> {
        let val = val.into();
        self.into_predicate(move |e| e.lte(val.clone()))
    }

    /// `<expr> > value`.
    pub fn gt<V: Into<sea_query::Value>>(self, val: V) -> Predicate<T> {
        let val = val.into();
        self.into_predicate(move |e| e.gt(val.clone()))
    }

    /// `<expr> >= value`.
    pub fn ge<V: Into<sea_query::Value>>(self, val: V) -> Predicate<T> {
        let val = val.into();
        self.into_predicate(move |e| e.gte(val.clone()))
    }
}

/// String-function helpers — `lower()`, `upper()`, `length()`, `trim()`,
/// `coalesce()`, `concat()`. Implemented for both `StrCol<T>` and
/// `NullableStrCol<T>` so the extension methods work whether the column is
/// `String` or `Option<String>`.
///
/// Each returns a [`ColExpr`]; chain a comparison (`.eq` / `.ne` / `.lt`
/// …) to produce a `Predicate<T>` for `filter` / `exclude`. All six render
/// identically on SQLite and Postgres (`TRIM`, `COALESCE` are standard
/// SQL; `||` is the standard concatenation operator both backends accept).
pub trait StrColExt<T> {
    /// `LOWER(col)` — case-insensitive comparison primitive.
    fn lower(&self) -> ColExpr<T>;
    /// `UPPER(col)`.
    fn upper(&self) -> ColExpr<T>;
    /// `LENGTH(col)` — character count of the stored value.
    fn length(&self) -> ColExpr<T>;
    /// `TRIM(col)` — strip leading/trailing whitespace before comparing,
    /// so `name.trim().eq("ada")` matches a stored `" ada "`.
    fn trim(&self) -> ColExpr<T>;
    /// `COALESCE(col, default)` — substitute `default` when the column is
    /// NULL, so a nullable column compares as the fallback. Mostly paired
    /// with `NullableStrCol`.
    fn coalesce<V: Into<sea_query::Value>>(&self, default: V) -> ColExpr<T>;
    /// `col || suffix` — append `suffix` (the standard SQL concatenation
    /// operator, which both backends accept) before comparing.
    fn concat<V: Into<sea_query::Value>>(&self, suffix: V) -> ColExpr<T>;
}

/// `TRIM("col")`. No bound values; same SQL on both backends.
fn str_trim_expr(name: &'static str) -> sea_query::SimpleExpr {
    Expr::cust(format!("TRIM(\"{}\")", name.replace('"', "\"\"")))
}

/// `COALESCE("col", default)` built as a native sea-query function so the
/// bound `default` is ordered alongside any later comparison value by
/// sea-query itself (mixing `cust_with_values`' embedded params with a
/// builder-added `.eq` value swaps their bind order).
fn str_coalesce_expr(name: &'static str, default: sea_query::Value) -> sea_query::SimpleExpr {
    let col: sea_query::SimpleExpr = Expr::col(Alias::new(name)).into();
    let def: sea_query::SimpleExpr = Expr::val(default).into();
    Func::coalesce([col, def]).into()
}

/// `"col" || suffix` via the standard concatenation operator `||` (which
/// both backends accept) as a native binary expr, so the bound `suffix`
/// orders correctly with a later comparison value.
fn str_concat_expr(name: &'static str, suffix: sea_query::Value) -> sea_query::SimpleExpr {
    Expr::col(Alias::new(name)).binary(sea_query::BinOper::Custom("||"), Expr::val(suffix))
}

impl<T> StrColExt<T> for StrCol<T> {
    fn lower(&self) -> ColExpr<T> {
        ColExpr::new(Func::lower(Expr::col(Alias::new(self.name))).into())
    }
    fn upper(&self) -> ColExpr<T> {
        ColExpr::new(Func::upper(Expr::col(Alias::new(self.name))).into())
    }
    fn length(&self) -> ColExpr<T> {
        ColExpr::new(Func::char_length(Expr::col(Alias::new(self.name))).into())
    }
    fn trim(&self) -> ColExpr<T> {
        ColExpr::new(str_trim_expr(self.name))
    }
    fn coalesce<V: Into<sea_query::Value>>(&self, default: V) -> ColExpr<T> {
        ColExpr::new(str_coalesce_expr(self.name, default.into()))
    }
    fn concat<V: Into<sea_query::Value>>(&self, suffix: V) -> ColExpr<T> {
        ColExpr::new(str_concat_expr(self.name, suffix.into()))
    }
}

impl<T> StrColExt<T> for NullableStrCol<T> {
    fn lower(&self) -> ColExpr<T> {
        ColExpr::new(Func::lower(Expr::col(Alias::new(self.name))).into())
    }
    fn upper(&self) -> ColExpr<T> {
        ColExpr::new(Func::upper(Expr::col(Alias::new(self.name))).into())
    }
    fn length(&self) -> ColExpr<T> {
        ColExpr::new(Func::char_length(Expr::col(Alias::new(self.name))).into())
    }
    fn trim(&self) -> ColExpr<T> {
        ColExpr::new(str_trim_expr(self.name))
    }
    fn coalesce<V: Into<sea_query::Value>>(&self, default: V) -> ColExpr<T> {
        ColExpr::new(str_coalesce_expr(self.name, default.into()))
    }
    fn concat<V: Into<sea_query::Value>>(&self, suffix: V) -> ColExpr<T> {
        ColExpr::new(str_concat_expr(self.name, suffix.into()))
    }
}

/// Date-extract helpers — `year()`, `month()`, `day()`.
///
/// Backend dispatch is hidden inside the returned [`ColExpr`]: the
/// Postgres form uses `CAST(EXTRACT(<part> FROM col) AS INTEGER)`;
/// the SQLite form uses `CAST(strftime('<fmt>', col) AS INTEGER)`.
/// Both forms land in the same `ColExpr`; `Predicate` picks the
/// right one at terminal time based on the resolved pool.
pub trait DateTimeColExt<T> {
    /// Year as an integer (e.g. 2026).
    fn year(&self) -> ColExpr<T>;
    /// Month of year, 1..=12.
    fn month(&self) -> ColExpr<T>;
    /// Day of month, 1..=31.
    fn day(&self) -> ColExpr<T>;
    /// Hour of day, 0..=23.
    fn hour(&self) -> ColExpr<T>;
    /// Minute of hour, 0..=59.
    fn minute(&self) -> ColExpr<T>;
    /// Second of minute, 0..=59 (whole seconds; subsecond fragments
    /// are truncated by the cast).
    fn second(&self) -> ColExpr<T>;
    /// Day of week. **Numbering differs by backend** to keep each
    /// dialect's native form: Postgres `EXTRACT(DOW ...)` returns
    /// 0=Sunday..6=Saturday; SQLite `strftime('%w', ...)` matches
    /// that numbering too, so both backends agree. Use this for
    /// "rows posted on weekends" / "rows posted on a Friday" style
    /// queries — compare against the integer (`week_day().eq(5)`
    /// for Friday).
    fn week_day(&self) -> ColExpr<T>;
}

fn date_part_exprs(
    col_name: &str,
    part_pg: &'static str,
    fmt_sqlite: &'static str,
) -> (sea_query::SimpleExpr, sea_query::SimpleExpr) {
    let pg = sea_query::SimpleExpr::Custom(format!(
        "CAST(EXTRACT({part_pg} FROM \"{col_name}\") AS INTEGER)"
    ));
    let sqlite = sea_query::SimpleExpr::Custom(format!(
        "CAST(strftime('{fmt_sqlite}', \"{col_name}\") AS INTEGER)"
    ));
    (pg, sqlite)
}

impl<T> DateTimeColExt<T> for DateTimeCol<T> {
    fn year(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "YEAR", "%Y");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn month(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "MONTH", "%m");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn day(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "DAY", "%d");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn hour(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "HOUR", "%H");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn minute(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "MINUTE", "%M");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn second(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "SECOND", "%S");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn week_day(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "DOW", "%w");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
}

impl<T> DateTimeColExt<T> for NullableDateTimeCol<T> {
    fn year(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "YEAR", "%Y");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn month(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "MONTH", "%m");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn day(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "DAY", "%d");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn hour(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "HOUR", "%H");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn minute(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "MINUTE", "%M");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn second(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "SECOND", "%S");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
    fn week_day(&self) -> ColExpr<T> {
        let (pg, sqlite) = date_part_exprs(self.name, "DOW", "%w");
        ColExpr::new_with_sqlite(pg, sqlite)
    }
}
