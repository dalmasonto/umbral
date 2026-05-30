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
//! M2 lifted the terminals and the `Manager` delegation onto a generic
//! `T: Model` bound. The table name comes from `T::TABLE`, the SELECT
//! column list from `T::FIELDS`, and row materialisation from the
//! `for<'r> FromRow<'r, SqliteRow>` supertrait on `Model`. M3 will
//! generate the `Model` impl for any user struct via `#[derive(Model)]`,
//! at which point `Manager<MyModel>` works without any per-model code.

use std::marker::PhantomData;

use sea_query::{Alias, Expr, Func, Order, Query, SqliteQueryBuilder};
use sea_query_binder::SqlxBinder;

use crate::orm::{Model, OrderExpr, Predicate};

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
/// depends on `T`. Terminals (which need row mapping) live in the
/// `impl<T: Model> QuerySet<T>` block below.
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
/// Precedence: explicit `.on(&pool)` override wins; then the per-
/// model database alias the Plugin contract published via
/// `Plugin::database()` (FEATURES.md #6); then the `"default"`
/// pool. Tests that skip the App builder pass `.on(&pool)`
/// directly and bypass the alias lookup entirely.
fn resolve_pool<T: Model>(explicit: Option<sqlx::SqlitePool>) -> sqlx::SqlitePool {
    if let Some(pool) = explicit {
        return pool;
    }
    if let Some(alias) = crate::migrate::model_alias(T::NAME) {
        return crate::db::pool_for(&alias);
    }
    crate::db::pool()
}

/// Terminal methods for every `QuerySet<T>` where `T: Model`.
///
/// `sqlx::query_as_with::<_, T, _>` works for any `T: Model` because the
/// `Model` trait carries `for<'r> FromRow<'r, SqliteRow>` as a
/// supertrait, so the row mapping is available without naming a
/// concrete type here.
impl<T: Model> QuerySet<T> {
    /// Render the SQL the QuerySet would execute, without running it.
    ///
    /// Returns the prepared statement with `?` placeholders for the
    /// bound values, exactly the string sqlx would send. Useful for
    /// `eprintln!`-style debugging and for tests that want to pin
    /// the rendered query without round-tripping through a pool.
    ///
    /// The bound values are intentionally not surfaced (sqlx's binder
    /// types aren't part of umbra's public surface); a `(sql, values)`
    /// accessor lands when EXPLAIN-style integration needs it.
    pub fn to_sql(&self) -> String {
        let (sql, _values) = self.query.build_sqlx(SqliteQueryBuilder);
        sql
    }

    /// Run the SELECT and return every matching row.
    pub async fn fetch(self) -> Result<Vec<T>, sqlx::Error> {
        let pool = resolve_pool::<T>(self.explicit_pool);
        let (sql, values) = self.query.build_sqlx(SqliteQueryBuilder);
        sqlx::query_as_with::<_, T, _>(&sql, values)
            .fetch_all(&pool)
            .await
    }

    /// Run the SELECT with LIMIT 1 and return the first row, if any.
    pub async fn first(mut self) -> Result<Option<T>, sqlx::Error> {
        self.query.limit(1);
        let pool = resolve_pool::<T>(self.explicit_pool);
        let (sql, values) = self.query.build_sqlx(SqliteQueryBuilder);
        sqlx::query_as_with::<_, T, _>(&sql, values)
            .fetch_optional(&pool)
            .await
    }

    /// Run `SELECT COUNT(*)` against the same FROM + WHERE.
    ///
    /// Reshapes the query rather than wrapping the existing SELECT: the
    /// projection becomes `COUNT(*)` and LIMIT/OFFSET drop away. ORDER
    /// BY is harmless on a scalar aggregate and is left in place. The
    /// `query_as_with` row type stays `(i64,)` because the result is an
    /// aggregate scalar, not a row of `T`.
    pub async fn count(self) -> Result<i64, sqlx::Error> {
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
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

/// Delegating chainable + terminal surface on `Manager<T>`.
///
/// Lets users write `Post::objects().filter(...).fetch().await` without
/// a separate `.query()` hop. Each method constructs the initial
/// `SelectStatement` against `T::TABLE` with one column per
/// `T::FIELDS` entry, wraps it in a fresh `QuerySet<T>`, and forwards.
impl<T: Model> Manager<T> {
    fn queryset(&self) -> QuerySet<T> {
        let columns: Vec<Alias> = T::FIELDS.iter().map(|f| Alias::new(f.name)).collect();
        let query = Query::select()
            .columns(columns)
            .from(Alias::new(T::TABLE))
            .take();
        QuerySet::new(query)
    }

    /// See `QuerySet::filter`.
    pub fn filter(&self, p: Predicate<T>) -> QuerySet<T> {
        self.queryset().filter(p)
    }

    /// See `QuerySet::order_by`.
    pub fn order_by(&self, o: OrderExpr<T>) -> QuerySet<T> {
        self.queryset().order_by(o)
    }

    /// See `QuerySet::limit`.
    pub fn limit(&self, n: u64) -> QuerySet<T> {
        self.queryset().limit(n)
    }

    /// See `QuerySet::offset`.
    pub fn offset(&self, n: u64) -> QuerySet<T> {
        self.queryset().offset(n)
    }

    /// See `QuerySet::on`.
    pub fn on(&self, pool: &sqlx::SqlitePool) -> QuerySet<T> {
        self.queryset().on(pool)
    }

    /// See `QuerySet::fetch`.
    pub async fn fetch(&self) -> Result<Vec<T>, sqlx::Error> {
        self.queryset().fetch().await
    }

    /// See `QuerySet::first`.
    pub async fn first(&self) -> Result<Option<T>, sqlx::Error> {
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
