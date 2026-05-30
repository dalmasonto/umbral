//! `QuerySet<T>` and `Manager<T>`: chainable lazy SQL builder + entry
//! point.
//!
//! `T::objects()` returns a `Manager<T>`; chaining `.filter` / `.order_by`
//! / `.limit` / etc. on it (or on a `QuerySet<T>` directly) yields a new
//! `QuerySet<T>`. Terminals (`.fetch`, `.first`, `.count`, `.exists`)
//! await an async DB roundtrip via the ambient or explicit pool.
//!
//! At M1 the surface is intentionally narrow per
//! `docs/specs/03-orm-querysets.md`: filter / order_by / limit / offset
//! for chaining, and fetch / first / count / exists for terminals. No
//! exclude / distinct / values / annotate / aggregate / update / delete
//! yet — those land as later milestones surface real need.
//!
//! The struct shapes are fixed (the sibling `post` module's `Post::
//! objects` returns `Manager<Post>`). Method implementations were filled
//! in by the M1 ORM fan-out subagent.

use std::marker::PhantomData;

use sea_query::{Alias, Expr, Func, Order, Query, SqliteQueryBuilder};
use sea_query_binder::SqlxBinder;

use crate::orm::{OrderExpr, Post, Predicate};

/// Entry point for queries on a model.
///
/// `Manager<T>` wraps a freshly-constructed `QuerySet<T>` and exposes
/// the same chainable surface. The user never constructs one directly;
/// `Post::objects()` is the only door.
pub struct Manager<T> {
    _phantom: PhantomData<T>,
}

impl<T> Manager<T> {
    pub(crate) fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<T> Default for Manager<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A lazy, chainable SQL query.
///
/// Carries a sea-query `SelectStatement` plus pool-resolution state.
/// Nothing is sent to the database until a terminal method is awaited.
/// Cloning is cheap (the `SelectStatement` clones in O(query size)).
pub struct QuerySet<T> {
    pub(crate) query: sea_query::SelectStatement,
    pub(crate) explicit_pool: Option<sqlx::SqlitePool>,
    _phantom: PhantomData<T>,
}

impl<T> QuerySet<T> {
    pub(crate) fn new(query: sea_query::SelectStatement) -> Self {
        Self {
            query,
            explicit_pool: None,
            _phantom: PhantomData,
        }
    }
}

/// Chainable methods on every `QuerySet<T>`.
///
/// These are model-agnostic: they only touch the sea-query
/// `SelectStatement` and the pool-resolution slot, neither of which
/// depends on `T`. Terminals (which need row mapping) live in a
/// concrete `impl QuerySet<Post>` below.
impl<T> QuerySet<T> {
    /// Add a WHERE condition. Multiple `.filter` calls AND together.
    pub fn filter(mut self, p: Predicate<T>) -> Self {
        self.query.and_where(p.cond);
        self
    }

    /// Add an ORDER BY clause. Multiple `.order_by` calls append.
    pub fn order_by(mut self, o: OrderExpr<T>) -> Self {
        let order = if o.descending {
            Order::Desc
        } else {
            Order::Asc
        };
        self.query.order_by(Alias::new(o.column), order);
        self
    }

    /// Set LIMIT.
    pub fn limit(mut self, n: u64) -> Self {
        self.query.limit(n);
        self
    }

    /// Set OFFSET.
    pub fn offset(mut self, n: u64) -> Self {
        self.query.offset(n);
        self
    }

    /// Override the pool resolved at terminal time.
    ///
    /// Wins over the ambient `umbra::db::pool()` default. Used by tests
    /// that drive the ORM without going through `App::build()`.
    pub fn on(mut self, pool: &sqlx::SqlitePool) -> Self {
        self.explicit_pool = Some(pool.clone());
        self
    }
}

/// Resolve the pool to run a terminal against.
///
/// Explicit pool wins; otherwise fall back to the ambient default
/// installed by `App::build()`. Tests that skip the App builder can
/// pass `.on(&pool)` instead.
fn resolve_pool(explicit: Option<sqlx::SqlitePool>) -> sqlx::SqlitePool {
    explicit.unwrap_or_else(crate::db::pool)
}

/// Terminal methods for `QuerySet<Post>`.
///
/// M1 specialises on the one hardcoded model so `sqlx::query_as_with`
/// can use a concrete `FromRow` impl. M2 lifts this onto a generic
/// `T: Model` once the trait exists.
impl QuerySet<Post> {
    /// Run the SELECT and return every matching row.
    pub async fn fetch(self) -> Result<Vec<Post>, sqlx::Error> {
        let pool = resolve_pool(self.explicit_pool);
        let (sql, values) = self.query.build_sqlx(SqliteQueryBuilder);
        sqlx::query_as_with::<_, Post, _>(&sql, values)
            .fetch_all(&pool)
            .await
    }

    /// Run the SELECT with LIMIT 1 and return the first row, if any.
    pub async fn first(mut self) -> Result<Option<Post>, sqlx::Error> {
        self.query.limit(1);
        let pool = resolve_pool(self.explicit_pool);
        let (sql, values) = self.query.build_sqlx(SqliteQueryBuilder);
        sqlx::query_as_with::<_, Post, _>(&sql, values)
            .fetch_optional(&pool)
            .await
    }

    /// Run `SELECT COUNT(*)` against the same FROM + WHERE.
    ///
    /// Rebuilds the query rather than wrapping the existing SELECT: the
    /// projection becomes `COUNT(*)` and LIMIT/OFFSET drop away. ORDER
    /// BY is harmless on a scalar aggregate and is left in place.
    pub async fn count(self) -> Result<i64, sqlx::Error> {
        let pool = resolve_pool(self.explicit_pool.clone());
        // Swap the projection for COUNT(*) and drop LIMIT / OFFSET, leaving
        // the FROM, WHERE, JOINs and GROUP BY intact. ORDER BY is harmless
        // on a scalar aggregate so it stays in place.
        let mut rebuilt = self.query;
        rebuilt.clear_selects();
        rebuilt.expr(Func::count(Expr::col(Alias::new("*"))));
        rebuilt.reset_limit();
        rebuilt.reset_offset();
        let (sql, values) = rebuilt.build_sqlx(SqliteQueryBuilder);
        let (n,): (i64,) = sqlx::query_as_with::<_, (i64,), _>(&sql, values)
            .fetch_one(&pool)
            .await?;
        Ok(n)
    }

    /// Return whether any row matches.
    ///
    /// M1 keeps the simple form: add LIMIT 1, fetch, check non-empty.
    /// A later milestone may swap the projection for `SELECT 1` to
    /// skip column materialisation.
    pub async fn exists(self) -> Result<bool, sqlx::Error> {
        let rows = self.limit(1).fetch().await?;
        Ok(!rows.is_empty())
    }
}

/// Delegating chainable + terminal surface on `Manager<Post>`.
///
/// Lets users write `Post::objects().filter(...).fetch().await` without
/// a separate `.query()` hop. Each method constructs the initial
/// `SelectStatement` against the `post` table, wraps it in a fresh
/// `QuerySet<Post>`, and forwards.
impl Manager<Post> {
    fn queryset(&self) -> QuerySet<Post> {
        let query = Query::select()
            .columns([
                Alias::new("id"),
                Alias::new("title"),
                Alias::new("body"),
                Alias::new("published_at"),
            ])
            .from(Alias::new(Post::TABLE))
            .take();
        QuerySet::new(query)
    }

    /// See `QuerySet::filter`.
    pub fn filter(&self, p: Predicate<Post>) -> QuerySet<Post> {
        self.queryset().filter(p)
    }

    /// See `QuerySet::order_by`.
    pub fn order_by(&self, o: OrderExpr<Post>) -> QuerySet<Post> {
        self.queryset().order_by(o)
    }

    /// See `QuerySet::limit`.
    pub fn limit(&self, n: u64) -> QuerySet<Post> {
        self.queryset().limit(n)
    }

    /// See `QuerySet::offset`.
    pub fn offset(&self, n: u64) -> QuerySet<Post> {
        self.queryset().offset(n)
    }

    /// See `QuerySet::on`.
    pub fn on(&self, pool: &sqlx::SqlitePool) -> QuerySet<Post> {
        self.queryset().on(pool)
    }

    /// See `QuerySet::fetch`.
    pub async fn fetch(&self) -> Result<Vec<Post>, sqlx::Error> {
        self.queryset().fetch().await
    }

    /// See `QuerySet::first`.
    pub async fn first(&self) -> Result<Option<Post>, sqlx::Error> {
        self.queryset().first().await
    }

    /// See `QuerySet::count`.
    pub async fn count(&self) -> Result<i64, sqlx::Error> {
        self.queryset().count().await
    }

    /// See `QuerySet::exists`.
    pub async fn exists(&self) -> Result<bool, sqlx::Error> {
        self.queryset().exists().await
    }
}
