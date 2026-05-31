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

use sea_query::{Alias, Expr, Func};

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

    /// SQL `>`.
    pub fn gt(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// SQL `>`.
    pub fn gt(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// SQL `>`.
    pub fn gt(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::DateTime<chrono::Utc>) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// SQL `>`.
    pub fn gt(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// SQL `>`.
    pub fn gt(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// SQL `>`.
    pub fn gt(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// SQL `>`.
    pub fn gt(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: i64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// SQL `>`.
    pub fn gt(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: f64) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// SQL `>`.
    pub fn gt(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::NaiveDate) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// SQL `>`.
    pub fn gt(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gt(val))
    }

    /// SQL `>=`.
    pub fn ge(&self, val: chrono::NaiveTime) -> Predicate<T> {
        Predicate::new(Expr::col(Alias::new(self.name)).gte(val))
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

    /// Render the `"col" -> $1 -> $2 ->> $N` template for a path of
    /// length `n`. Returns the SQL string and the path-segment Values
    /// (in order). The caller appends comparison fragments and binds
    /// additional values.
    fn extract_template(&self, base_placeholder: usize) -> (String, Vec<sea_query::Value>) {
        let col = self.column.replace('"', "\"\"");
        let n = self.path.len();
        let mut sql = format!("\"{col}\"");
        // Path of length 1: "col" ->> $1
        // Path of length n: "col" -> $1 -> $2 ... -> $(n-1) ->> $n
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

    /// SQL `<extracted> = $val` (Postgres).
    pub fn eq(&self, val: &str) -> Predicate<T> {
        let (extract, mut values) = self.extract_template(1);
        let placeholder = values.len() + 1;
        let sql = format!("{extract} = ${placeholder}");
        values.push(sea_query::Value::String(Some(Box::new(val.to_string()))));
        Predicate::new(Expr::cust_with_values(&sql, values))
    }

    /// SQL `<extracted> <> $val` (Postgres).
    pub fn ne(&self, val: &str) -> Predicate<T> {
        let (extract, mut values) = self.extract_template(1);
        let placeholder = values.len() + 1;
        let sql = format!("{extract} <> ${placeholder}");
        values.push(sea_query::Value::String(Some(Box::new(val.to_string()))));
        Predicate::new(Expr::cust_with_values(&sql, values))
    }

    /// SQL `<extracted> IS NULL`. Distinguishes "the column itself is
    /// NULL" from "the path traversal misses a key" — both produce
    /// NULL from `->>`, which is the Postgres semantic.
    pub fn is_null(&self) -> Predicate<T> {
        let (extract, values) = self.extract_template(1);
        Predicate::new(Expr::cust_with_values(format!("{extract} IS NULL"), values))
    }

    /// SQL `<extracted> IS NOT NULL`.
    pub fn is_not_null(&self) -> Predicate<T> {
        let (extract, values) = self.extract_template(1);
        Predicate::new(Expr::cust_with_values(
            format!("{extract} IS NOT NULL"),
            values,
        ))
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
    // sea-query's cust_with_values uses `?` and `$N` as placeholder
    // tokens — a literal `?` in the template would be substituted
    // away. To emit a literal `?` operator we either escape it (the
    // sea-query token doubler `??` produces a literal `?`) or render
    // the whole fragment with `Expr::cust` (no value bindings) +
    // string-quote the key inline. The latter is safer against future
    // sea-query tokenizer changes; the key is single-quoted with
    // standard SQL escape (double the apostrophe).
    let col_escaped = col.replace('"', "\"\"");
    let key_escaped = key.replace('\'', "''");
    let sql = format!("\"{col_escaped}\" ?? '{key_escaped}'");
    // The double `?` becomes a literal `?` after sea-query
    // tokenizes the template — verified in sea-query's render
    // logic (`Punctuation(placeholder)` followed by another
    // `Punctuation(placeholder)` is treated as the escape).
    Predicate::new(Expr::cust(&sql))
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
