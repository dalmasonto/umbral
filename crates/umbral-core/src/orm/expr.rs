//! `F`-expressions and `Q`-objects — composable predicates and column references.
//!
//! # F-expressions
//!
//! [`F`] wraps a column name as a first-class value so it can appear on the
//! right-hand side of a comparison (column-vs-column WHERE) or inside an
//! atomic update expression.
//!
//! ## Column-vs-column WHERE
//!
//! ```rust,ignore
//! use umbral::orm::F;
//!
//! // WHERE author = editor
//! Post::objects()
//!     .filter(post::AUTHOR.eq_f(F::col("editor")))
//!     .fetch()
//!     .await?;
//! ```
//!
//! ## Atomic update arithmetic
//!
//! ```rust,ignore
//! use umbral::orm::{F, FExpr};
//!
//! // SET views = views + 1
//! Post::objects()
//!     .filter(post::ID.eq(42))
//!     .update_expr("views", F::col("views").add(1))
//!     .await?;
//! ```
//!
//! # Q-objects
//!
//! [`Q`] composes predicates with explicit AND, OR, and NOT so complex
//! boolean trees can be built before being handed to `.filter()`. The
//! existing `&` / `|` operators on `Predicate<T>` continue to work; `Q` adds
//! named constructors and `Q::not` for single-predicate negation.
//!
//! ```rust,ignore
//! use umbral::orm::Q;
//!
//! Post::objects()
//!     .filter(Q::or(post::PUBLISHED.eq(true), post::AUTHOR.eq(user_id)))
//!     .filter(Q::not(post::AUTHOR.eq(spam_user_id)))
//!     .fetch()
//!     .await?;
//! ```

use sea_query::{Alias, Expr as SqExpr};

use super::Predicate;

// ===========================================================================
// F-expressions
// ===========================================================================

/// A reference to a column on the current model's table.
///
/// Use this wherever you need to compare two columns on the same row or
/// reference a column on the right-hand side of an UPDATE `SET` clause.
///
/// # Construction
///
/// ```rust,ignore
/// use umbral::orm::F;
///
/// let col_ref = F::col("views");
/// ```
///
/// # Arithmetic
///
/// [`F::col`] returns an [`FExpr`] that supports `.add(n)`, `.sub(n)`,
/// `.mul(n)`, and `.div(n)` so you can express `SET views = views + 1`
/// without string formatting.
/// `F` is a namespace for the `col` factory method; it has no instance state.
pub struct F;

impl F {
    /// Create an F-expression referencing the column with the given name.
    ///
    /// The name must be a column that exists on the current model's table;
    /// unknown names produce a database-level error at runtime, not a
    /// compile-time one.
    pub fn col(name: impl Into<String>) -> FExpr {
        FExpr {
            inner: FExprInner::Column(name.into()),
        }
    }
}

/// The internal representation of an F-expression tree.
#[derive(Clone, Debug)]
enum FExprInner {
    /// A bare column reference: `col_name`.
    Column(String),
    /// `lhs + rhs`.
    Add(Box<FExpr>, Box<FExpr>),
    /// `lhs - rhs`.
    Sub(Box<FExpr>, Box<FExpr>),
    /// `lhs * rhs`.
    Mul(Box<FExpr>, Box<FExpr>),
    /// `lhs / rhs`.
    Div(Box<FExpr>, Box<FExpr>),
    /// A literal integer constant.
    LitI64(i64),
}

/// An expression that can appear in a `SET col = <expr>` clause.
///
/// Built via [`F::col`] and the arithmetic methods on this type. Passed to
/// [`QuerySet::update_expr`] to perform atomic column updates.
#[derive(Clone, Debug)]
pub struct FExpr {
    inner: FExprInner,
}

impl FExpr {
    /// Render this expression as a `sea_query::SimpleExpr` for use in an
    /// UPDATE's SET clause or a WHERE condition.
    pub(crate) fn to_simple_expr(&self) -> sea_query::SimpleExpr {
        match &self.inner {
            FExprInner::Column(name) => SqExpr::col(Alias::new(name.as_str())).into(),
            FExprInner::Add(lhs, rhs) => {
                let l = lhs.to_simple_expr();
                let r = rhs.to_simple_expr();
                l.add(r)
            }
            FExprInner::Sub(lhs, rhs) => {
                let l = lhs.to_simple_expr();
                let r = rhs.to_simple_expr();
                l.sub(r)
            }
            FExprInner::Mul(lhs, rhs) => {
                let l = lhs.to_simple_expr();
                let r = rhs.to_simple_expr();
                l.mul(r)
            }
            FExprInner::Div(lhs, rhs) => {
                let l = lhs.to_simple_expr();
                let r = rhs.to_simple_expr();
                l.div(r)
            }
            FExprInner::LitI64(n) => {
                sea_query::SimpleExpr::Value(sea_query::Value::BigInt(Some(*n)))
            }
        }
    }

    /// `self + n` — produces `col + n` in the generated SQL.
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, n: i64) -> FExpr {
        FExpr {
            inner: FExprInner::Add(Box::new(self), Box::new(FExpr::lit_i64(n))),
        }
    }

    /// `self - n` — produces `col - n` in the generated SQL.
    #[allow(clippy::should_implement_trait)]
    pub fn sub(self, n: i64) -> FExpr {
        FExpr {
            inner: FExprInner::Sub(Box::new(self), Box::new(FExpr::lit_i64(n))),
        }
    }

    /// `self * n` — produces `col * n` in the generated SQL.
    #[allow(clippy::should_implement_trait)]
    pub fn mul(self, n: i64) -> FExpr {
        FExpr {
            inner: FExprInner::Mul(Box::new(self), Box::new(FExpr::lit_i64(n))),
        }
    }

    /// `self / n` — produces `col / n` in the generated SQL.
    #[allow(clippy::should_implement_trait)]
    pub fn div(self, n: i64) -> FExpr {
        FExpr {
            inner: FExprInner::Div(Box::new(self), Box::new(FExpr::lit_i64(n))),
        }
    }

    fn lit_i64(n: i64) -> FExpr {
        FExpr {
            inner: FExprInner::LitI64(n),
        }
    }
}

// ===========================================================================
// F-expression predicates on column types.
//
// `IntCol` and `ForeignKeyCol` gain `.eq_f(FExpr)` / `.ne_f(FExpr)` so a
// column-vs-column WHERE is spelled naturally. The `_f` suffix avoids
// collision with the existing `.eq(i64)` methods. The impl lives here in
// expr.rs rather than in column.rs so the column module doesn't need to
// depend on FExpr and the dependency graph stays clean.
// ===========================================================================

/// Extension trait that adds `eq_f` / `ne_f` to column handles so a column
/// can be compared against an F-expression (another column or arithmetic).
///
/// Implemented for the integer and foreign-key column types. String and
/// datetime columns gain this too when a consumer surfaces the need.
pub trait FColExt<T> {
    /// `WHERE <col> = <expr>`.
    fn eq_f(&self, expr: FExpr) -> Predicate<T>;
    /// `WHERE <col> <> <expr>`.
    fn ne_f(&self, expr: FExpr) -> Predicate<T>;
}

impl<T> FColExt<T> for crate::orm::column::IntCol<T> {
    fn eq_f(&self, expr: FExpr) -> Predicate<T> {
        Predicate::new(SqExpr::col(Alias::new(self.name)).eq(expr.to_simple_expr()))
    }
    fn ne_f(&self, expr: FExpr) -> Predicate<T> {
        Predicate::new(SqExpr::col(Alias::new(self.name)).ne(expr.to_simple_expr()))
    }
}

impl<T> FColExt<T> for crate::orm::column::ForeignKeyCol<T> {
    fn eq_f(&self, expr: FExpr) -> Predicate<T> {
        Predicate::new(SqExpr::col(Alias::new(self.name)).eq(expr.to_simple_expr()))
    }
    fn ne_f(&self, expr: FExpr) -> Predicate<T> {
        Predicate::new(SqExpr::col(Alias::new(self.name)).ne(expr.to_simple_expr()))
    }
}

impl<T> FColExt<T> for crate::orm::column::StrCol<T> {
    fn eq_f(&self, expr: FExpr) -> Predicate<T> {
        Predicate::new(SqExpr::col(Alias::new(self.name)).eq(expr.to_simple_expr()))
    }
    fn ne_f(&self, expr: FExpr) -> Predicate<T> {
        Predicate::new(SqExpr::col(Alias::new(self.name)).ne(expr.to_simple_expr()))
    }
}

// ===========================================================================
// Q-objects
// ===========================================================================

/// A composable predicate builder.
///
/// `Q` provides named constructors for the three logical connectives — AND,
/// OR, and NOT — so complex WHERE trees can be expressed without reaching
/// for the `&` / `|` operator overloads. Both styles coexist: `Q::and(a, b)`
/// is the same as `a & b`; pick whichever reads better for your query.
///
/// ```rust,ignore
/// use umbral::orm::Q;
///
/// // OR: published OR authored by this user
/// Post::objects()
///     .filter(Q::or(
///         post::PUBLISHED.eq(true),
///         post::AUTHOR.eq(user_id),
///     ))
///     .fetch()
///     .await?;
///
/// // NOT: exclude spam author
/// Post::objects()
///     .filter(Q::not(post::AUTHOR.eq(spam_id)))
///     .fetch()
///     .await?;
///
/// // Nesting: (published AND title contains "rust") OR (author = me)
/// Post::objects()
///     .filter(Q::or(
///         Q::and(post::PUBLISHED.eq(true), post::TITLE.contains("rust")),
///         post::AUTHOR.eq(my_id),
///     ))
///     .fetch()
///     .await?;
/// ```
pub struct Q;

impl Q {
    /// Combine two predicates with logical AND.
    ///
    /// `Q::and(a, b)` is equivalent to `a & b`. Use this form when
    /// building the condition dynamically or when the infix notation
    /// reduces readability (e.g. deeply nested trees).
    pub fn and<T>(a: Predicate<T>, b: Predicate<T>) -> Predicate<T> {
        a & b
    }

    /// Combine two predicates with logical OR.
    ///
    /// `Q::or(a, b)` is equivalent to `a | b`.
    pub fn or<T>(a: Predicate<T>, b: Predicate<T>) -> Predicate<T> {
        a | b
    }

    /// Negate a predicate with logical NOT.
    ///
    /// Wraps the condition's `sea_query::SimpleExpr` in a NOT() wrapper.
    /// Both the default (`cond`) and optional SQLite override
    /// (`cond_sqlite`) are negated element-wise so backend routing stays
    /// correct.
    pub fn not<T>(p: Predicate<T>) -> Predicate<T> {
        use std::marker::PhantomData;
        let negated_cond = p.cond.not();
        let negated_sqlite = p.cond_sqlite.map(|c| c.not());
        Predicate {
            cond: negated_cond,
            cond_sqlite: negated_sqlite,
            _phantom: PhantomData,
        }
    }
}
