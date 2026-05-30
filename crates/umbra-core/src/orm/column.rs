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
