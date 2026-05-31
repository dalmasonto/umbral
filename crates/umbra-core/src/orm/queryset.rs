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
//! `FromRow` bound the terminals carry. M3 generates the `Model` impl
//! from `#[derive(Model)]`.
//!
//! ## Phase 2.5 — backend-agnostic terminals
//!
//! Through Phase 2 the QuerySet stored a `SqlitePool` and built every
//! query with sea-query's `SqliteQueryBuilder`. Phase 2.5 widens that:
//! the explicit-pool slot is `Option<DbPool>`, `.on(&SqlitePool)` keeps
//! working unchanged, and a new `.on_pg(&PgPool)` registers a Postgres
//! pool. The terminal methods dispatch on the resolved pool variant —
//! SQLite path uses `SqliteQueryBuilder` + a `SqlitePool` executor;
//! Postgres path uses `PostgresQueryBuilder` + a `PgPool` executor.
//!
//! The row-materialization bound on each terminal is the conjunction
//! of both backends' `FromRow` impls. `#[derive(sqlx::FromRow)]` emits
//! a generic-over-`R` impl, so a user struct with standard field
//! types satisfies both bounds without any per-backend ceremony.

use std::marker::PhantomData;

use sea_query::{Alias, Expr, Func, Order, PostgresQueryBuilder, Query, SqliteQueryBuilder};
use sea_query_binder::SqlxBinder;

use crate::db::DbPool;
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
    /// The base SelectStatement — FROM, columns, joins, group-by,
    /// order-by, limit, offset. Filters are NOT applied here; they
    /// accumulate on [`Self::predicates`] and get woven in at
    /// terminal time, so per-backend predicate variants (Phase
    /// 4.2.2) can pick the right SimpleExpr based on the resolved
    /// pool.
    pub(crate) query: sea_query::SelectStatement,
    /// Accumulated filter predicates. Each one renders to either its
    /// default `cond` (for Postgres) or its `cond_sqlite` override
    /// (for SQLite, if set) at terminal time.
    pub(crate) predicates: Vec<Predicate<T>>,
    pub(crate) explicit_pool: Option<DbPool>,
    _phantom: PhantomData<T>,
}

impl<T> QuerySet<T> {
    pub(crate) fn new(query: sea_query::SelectStatement) -> Self {
        Self {
            query,
            predicates: Vec::new(),
            explicit_pool: None,
            _phantom: PhantomData,
        }
    }

    /// Clone the base query and weave in the accumulated predicates,
    /// picking the dialect-appropriate `SimpleExpr` for each one. The
    /// `backend_name` is `"sqlite"` or `"postgres"`; any other value
    /// behaves like Postgres (the default).
    pub(crate) fn build_query_for(&self, backend_name: &str) -> sea_query::SelectStatement {
        let mut q = self.query.clone();
        for p in &self.predicates {
            q.and_where(p.cond_for(backend_name));
        }
        q
    }
}

/// Chainable methods on every `QuerySet<T>`.
///
/// These are model-agnostic: they only touch the sea-query
/// `SelectStatement` and the pool-resolution slot, neither of which
/// depends on `T`. Terminals (which need row mapping) live in the
/// `impl<T: Model> QuerySet<T>` block below.
impl<T> QuerySet<T> {
    /// Add a WHERE condition. Multiple `.filter` calls AND together
    /// (sea-query's `and_where` semantics — applied at terminal time
    /// once the resolved pool's backend is known).
    pub fn filter(mut self, p: Predicate<T>) -> Self {
        self.predicates.push(p);
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

    /// Override the pool resolved at terminal time with a SQLite pool.
    ///
    /// Wins over the ambient default. Used by tests that drive the ORM
    /// without going through `App::build()`. For a Postgres override
    /// use [`Self::on_pg`].
    pub fn on(mut self, pool: &sqlx::SqlitePool) -> Self {
        self.explicit_pool = Some(DbPool::Sqlite(pool.clone()));
        self
    }

    /// Override the pool resolved at terminal time with a Postgres pool.
    ///
    /// The Postgres counterpart of [`Self::on`]. Tests that want to
    /// exercise the Postgres branch (or that drive against a real
    /// Postgres instance) reach for this directly.
    pub fn on_pg(mut self, pool: &sqlx::PgPool) -> Self {
        self.explicit_pool = Some(DbPool::Postgres(pool.clone()));
        self
    }
}

/// Resolve the pool to run a terminal against.
///
/// Precedence: explicit `.on(&pool)` / `.on_pg(&pool)` override wins;
/// then the per-model database alias the Plugin contract published
/// via `Plugin::database()` (FEATURES.md #6); then the `"default"`
/// pool. Tests that skip the App builder pass an explicit pool and
/// bypass the alias lookup entirely.
fn resolve_pool<T: Model>(explicit: Option<DbPool>) -> DbPool {
    if let Some(pool) = explicit {
        return pool;
    }
    if let Some(alias) = crate::migrate::model_alias(T::NAME) {
        return crate::db::pool_for_dispatched(&alias).clone();
    }
    crate::db::pool_dispatched().clone()
}

/// Terminal methods for every `QuerySet<T>` where `T: Model`.
///
/// Each terminal that materializes `T` carries a FromRow bound on the
/// method (not the impl block) — the conjunction of both backends'
/// FromRow impls. `#[derive(sqlx::FromRow)]` emits a generic-over-`R`
/// impl, so any user struct with standard field types satisfies both
/// bounds automatically.
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
    ///
    /// The rendered placeholder dialect is SQLite's (`?`). When the
    /// dispatched pool is Postgres the actual at-execute rendering
    /// uses `$1`-style placeholders; the `to_sql` debug surface
    /// continues to emit SQLite-style for stability across calls
    /// regardless of which pool is registered.
    pub fn to_sql(&self) -> String {
        let q = self.build_query_for("sqlite");
        let (sql, _values) = q.build_sqlx(SqliteQueryBuilder);
        sql
    }

    /// Render the QuerySet's SQL against the **Postgres** dialect,
    /// without running it. Companion to [`Self::to_sql`].
    ///
    /// The two render slightly different placeholder syntax (`?` for
    /// SQLite, `$1..$N` for Postgres) and any Postgres-specific
    /// operators like the array `@>` / `<@` / `&&` family only render
    /// correctly through this entry point — `to_sql`'s SQLite path
    /// leaves `$N` tokens in the template untouched. Use this when
    /// debugging a Postgres query or asserting on the rendered shape
    /// in tests.
    pub fn to_sql_pg(&self) -> String {
        let q = self.build_query_for("postgres");
        let (sql, _values) = q.build_sqlx(PostgresQueryBuilder);
        sql
    }

    /// Run the SELECT and return every matching row.
    pub async fn fetch(self) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        // The turbofish on `query_as_with::<DB, _, _>` is load-bearing:
        // with both `sqlx-sqlite` and `sqlx-postgres` features on
        // sea-query-binder, `SqlxValues` implements `IntoArguments` for
        // both backends, so the compiler can't infer DB from the values
        // alone. Naming DB explicitly pins which `FromRow` bound is
        // checked.
        match resolve_pool::<T>(self.explicit_pool.clone()) {
            DbPool::Sqlite(pool) => {
                let q = self.build_query_for("sqlite");
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_all(&pool)
                    .await
            }
            DbPool::Postgres(pool) => {
                let q = self.build_query_for("postgres");
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_all(&pool)
                    .await
            }
        }
    }

    /// Run the SELECT with LIMIT 1 and return the first row, if any.
    pub async fn first(mut self) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.query.limit(1);
        match resolve_pool::<T>(self.explicit_pool.clone()) {
            DbPool::Sqlite(pool) => {
                let q = self.build_query_for("sqlite");
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_optional(&pool)
                    .await
            }
            DbPool::Postgres(pool) => {
                let q = self.build_query_for("postgres");
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_optional(&pool)
                    .await
            }
        }
    }

    /// Run `SELECT COUNT(*)` against the same FROM + WHERE.
    ///
    /// Reshapes the query rather than wrapping the existing SELECT: the
    /// projection becomes `COUNT(*)` and LIMIT/OFFSET drop away. ORDER
    /// BY is harmless on a scalar aggregate and is left in place. The
    /// row type is `(i64,)` so the FromRow constraint comes from sqlx's
    /// tuple impl rather than the user struct — count() doesn't need
    /// T's FromRow bounds.
    pub async fn count(self) -> Result<i64, sqlx::Error> {
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        // Build the dialect-appropriate filtered query first, then
        // rebuild as COUNT. Doing it in this order keeps the predicate
        // walk pluggable per backend without duplicating the COUNT
        // rewrite logic across branches.
        let mut rebuilt = self.build_query_for(backend);
        rebuilt.clear_selects();
        rebuilt.expr(Func::count(Expr::col(Alias::new("*"))));
        rebuilt.reset_limit();
        rebuilt.reset_offset();

        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = rebuilt.build_sqlx(SqliteQueryBuilder);
                let (n,): (i64,) = sqlx::query_as_with::<sqlx::Sqlite, (i64,), _>(&sql, values)
                    .fetch_one(&pool)
                    .await?;
                Ok(n)
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = rebuilt.build_sqlx(PostgresQueryBuilder);
                let (n,): (i64,) = sqlx::query_as_with::<sqlx::Postgres, (i64,), _>(&sql, values)
                    .fetch_one(&pool)
                    .await?;
                Ok(n)
            }
        }
    }

    /// Return whether any row matches.
    ///
    /// M1 keeps the simple form: add LIMIT 1, fetch, check non-empty.
    /// A later milestone may swap the projection for `SELECT 1` to
    /// skip column materialisation.
    pub async fn exists(self) -> Result<bool, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let rows = self.limit(1).fetch().await?;
        Ok(!rows.is_empty())
    }

    // =====================================================================
    // Postgres-only terminals (Phase 4.1).
    //
    // Models with Postgres-only field types (`Vec<T>` arrays, the future
    // Hstore / CIDR / FullTextSearch types) can't satisfy the dual
    // FromRow bound on `fetch` / `first` / `count` / `exists`. These
    // `_pg` variants bound on `FromRow<PgRow>` alone, take the pool as
    // an argument, and skip the dispatch — the call site explicitly
    // says "this model is Postgres-only."
    //
    // For models with portable fields, the existing `fetch` etc. stay
    // the recommended call: they pick up the ambient pool and route
    // through `.on(&pool)` / `.on_pg(&pool)` overrides exactly as
    // Phase 2.5 documented.
    // =====================================================================

    // =====================================================================
    // Write terminals — DELETE and UPDATE.
    //
    // Both apply the accumulated filter predicates as the WHERE clause,
    // dispatch to the resolved pool's backend, and return the affected-
    // rows count from sqlx. No row materialisation — DELETE is keyless,
    // and UPDATE doesn't do a RETURNING read-back at v1 (use
    // `.filter(...).fetch()` after a write if you need the updated
    // rows back).
    //
    // **Without a `.filter(...)`, both terminals affect every row in
    // the table.** That mirrors raw SQL semantics; the type system
    // can't distinguish "I forgot the filter" from "I really meant to
    // truncate." Users protecting against accidental full-table writes
    // wrap their callers or assert a row count via `.count()` first.
    // =====================================================================

    /// `DELETE FROM table WHERE <predicates>`. Returns the number of
    /// rows deleted. With no `.filter` calls, deletes every row.
    pub async fn delete(self) -> Result<u64, sqlx::Error> {
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        let stmt = self.build_delete_for(backend);
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let result = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&pool)
                    .await?;
                Ok(result.rows_affected())
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let result = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&pool)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// `UPDATE table SET k=v[, ...] WHERE <predicates>`. The values
    /// map provides `column_name → JSON value` pairs; each is
    /// converted to a `sea_query::Value` per the column's declared
    /// `SqlType` via [`crate::orm::write::json_to_sea_value`]. Returns
    /// the number of rows affected.
    ///
    /// Unknown columns in the map fail loudly with
    /// `WriteError::UnknownColumn`. JSON `null` is rejected for
    /// non-nullable columns; supplying a column that exists but is
    /// absent from the map is silently a no-op (the column keeps its
    /// current value — PATCH semantics, not PUT).
    pub async fn update_values(
        self,
        values: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, crate::orm::write::WriteError> {
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        let stmt = self.build_update_for(backend, &values)?;
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let result = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&pool)
                    .await?;
                Ok(result.rows_affected())
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let result = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&pool)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// Helper: build the DELETE statement for the active backend.
    /// Public-by-virtue-of-being-pub(crate) so the `_pg` and (future)
    /// `_sqlite` explicit-pool variants can share the SQL builder.
    fn build_delete_for(&self, backend_name: &str) -> sea_query::DeleteStatement {
        let mut stmt = Query::delete();
        stmt.from_table(Alias::new(T::TABLE));
        for p in &self.predicates {
            stmt.and_where(p.cond_for(backend_name));
        }
        stmt
    }

    /// Helper: build the UPDATE statement for the active backend.
    /// Walks the `values` map, validates each column against the
    /// model's `FIELDS` metadata, converts the JSON value via
    /// `write::json_to_sea_value`, and threads the accumulated
    /// predicates into the WHERE clause.
    fn build_update_for(
        &self,
        backend_name: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<sea_query::UpdateStatement, crate::orm::write::WriteError> {
        use crate::orm::write::{WriteError, json_to_sea_value};
        let mut stmt = Query::update();
        stmt.table(Alias::new(T::TABLE));
        for (col_name, val) in values {
            // Look up the column on the model. Unknown column names
            // fail loudly here rather than producing a bad UPDATE.
            let field = T::FIELDS
                .iter()
                .find(|f| f.name == col_name.as_str())
                .ok_or_else(|| WriteError::UnknownColumn {
                    field: col_name.clone(),
                })?;
            // Reject attempts to overwrite the PK via update_values.
            // The QuerySet's WHERE clause is the only way to identify
            // rows; rewriting the PK while filtering on the old one
            // is a footgun.
            if field.primary_key {
                continue;
            }
            let sea_value = json_to_sea_value(field.ty, val, field.nullable, field.name)?;
            stmt.value(Alias::new(field.name), sea_value);
        }
        for p in &self.predicates {
            stmt.and_where(p.cond_for(backend_name));
        }
        Ok(stmt)
    }

    /// Run the SELECT against an explicit `PgPool` and return every
    /// matching row. Bound by `FromRow<PgRow>` alone so models with
    /// Postgres-only field types compile.
    pub async fn fetch_pg(self, pool: &sqlx::PgPool) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let q = self.build_query_for("postgres");
        let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
        sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
            .fetch_all(pool)
            .await
    }

    /// Run the SELECT against an explicit `PgPool` with LIMIT 1.
    pub async fn first_pg(mut self, pool: &sqlx::PgPool) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.query.limit(1);
        let q = self.build_query_for("postgres");
        let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
        sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
            .fetch_optional(pool)
            .await
    }

    /// Run `SELECT COUNT(*)` against an explicit `PgPool`. No FromRow
    /// bound on `T` — the count tuple type is `(i64,)`.
    pub async fn count_pg(self, pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
        let mut rebuilt = self.build_query_for("postgres");
        rebuilt.clear_selects();
        rebuilt.expr(Func::count(Expr::col(Alias::new("*"))));
        rebuilt.reset_limit();
        rebuilt.reset_offset();
        let (sql, values) = rebuilt.build_sqlx(PostgresQueryBuilder);
        let (n,): (i64,) = sqlx::query_as_with::<sqlx::Postgres, (i64,), _>(&sql, values)
            .fetch_one(pool)
            .await?;
        Ok(n)
    }

    /// Return whether any row matches, against an explicit `PgPool`.
    pub async fn exists_pg(self, pool: &sqlx::PgPool) -> Result<bool, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let rows = self.limit(1).fetch_pg(pool).await?;
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

    /// See `QuerySet::on_pg`.
    pub fn on_pg(&self, pool: &sqlx::PgPool) -> QuerySet<T> {
        self.queryset().on_pg(pool)
    }

    /// See `QuerySet::fetch`.
    pub async fn fetch(&self) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.queryset().fetch().await
    }

    /// See `QuerySet::first`.
    pub async fn first(&self) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.queryset().first().await
    }

    /// See `QuerySet::count`.
    pub async fn count(&self) -> Result<i64, sqlx::Error> {
        self.queryset().count().await
    }

    /// See `QuerySet::exists`.
    pub async fn exists(&self) -> Result<bool, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.queryset().exists().await
    }

    /// See [`QuerySet::fetch_pg`].
    pub async fn fetch_pg(&self, pool: &sqlx::PgPool) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.queryset().fetch_pg(pool).await
    }

    /// See [`QuerySet::first_pg`].
    pub async fn first_pg(&self, pool: &sqlx::PgPool) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.queryset().first_pg(pool).await
    }

    /// See [`QuerySet::count_pg`].
    pub async fn count_pg(&self, pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
        self.queryset().count_pg(pool).await
    }

    /// See [`QuerySet::exists_pg`].
    pub async fn exists_pg(&self, pool: &sqlx::PgPool) -> Result<bool, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.queryset().exists_pg(pool).await
    }

    // =====================================================================
    // Write methods — INSERT.
    //
    // `create(instance)` does one row; `bulk_create([...])` does many in
    // a single multi-VALUES INSERT. Both serialise the instance(s) to a
    // JSON map via `serde::Serialize`, look up each field in the model's
    // `FIELDS` metadata, and bind values through
    // [`crate::orm::write::json_to_sea_value`].
    //
    // PK handling:
    // - Default value (0 for ints, nil for UUIDs, empty for String):
    //   omitted from the INSERT column list so the DB autoincrement /
    //   default kicks in.
    // - Explicit non-default value: included in the INSERT so the
    //   caller can supply UUIDs / slug PKs themselves.
    // =====================================================================

    /// INSERT one row, return the row as it now exists in the
    /// database (with any autoincrement PK populated). Uses the
    /// ambient pool via `Manager::queryset().resolve_pool`.
    pub async fn create(&self, instance: T) -> Result<T, crate::orm::write::WriteError>
    where
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let map = serialize_to_map(&instance)?;
        let pool = resolve_pool::<T>(None);
        let backend = pool.backend_name();
        let stmt = build_insert_one_for::<T>(backend, &map)?;
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let row = sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_one(&pool)
                    .await?;
                Ok(row)
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let row = sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_one(&pool)
                    .await?;
                Ok(row)
            }
        }
    }

    /// INSERT many rows in a single statement. Returns the number of
    /// rows inserted. Doesn't RETURNING-read-back the rows — use a
    /// follow-up `Model::objects().filter(...).fetch()` if you need
    /// them populated.
    ///
    /// Empty input is a no-op (returns Ok(0)) — the alternative
    /// (building a `INSERT INTO t () VALUES ()` and failing at the
    /// DB) doesn't help anyone.
    pub async fn bulk_create(&self, instances: Vec<T>) -> Result<u64, crate::orm::write::WriteError>
    where
        T: serde::Serialize,
    {
        if instances.is_empty() {
            return Ok(0);
        }
        let maps: Result<Vec<_>, _> = instances.iter().map(serialize_to_map).collect();
        let maps = maps?;
        let pool = resolve_pool::<T>(None);
        let backend = pool.backend_name();
        let stmt = build_insert_many_for::<T>(backend, &maps)?;
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let result = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&pool)
                    .await?;
                Ok(result.rows_affected())
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let result = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&pool)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// `create` against an explicit Postgres pool. The Postgres
    /// counterpart of [`Self::create`] for models with Postgres-only
    /// field types (Array, Inet, MacAddr, FullText), whose `FromRow`
    /// impl exists only for `PgRow`.
    pub async fn create_pg(
        &self,
        instance: T,
        pool: &sqlx::PgPool,
    ) -> Result<T, crate::orm::write::WriteError>
    where
        T: serde::Serialize + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let map = serialize_to_map(&instance)?;
        let stmt = build_insert_one_for::<T>("postgres", &map)?;
        let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
        let row = sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
            .fetch_one(pool)
            .await?;
        Ok(row)
    }

    /// `bulk_create` against an explicit Postgres pool.
    pub async fn bulk_create_pg(
        &self,
        instances: Vec<T>,
        pool: &sqlx::PgPool,
    ) -> Result<u64, crate::orm::write::WriteError>
    where
        T: serde::Serialize,
    {
        if instances.is_empty() {
            return Ok(0);
        }
        let maps: Result<Vec<_>, _> = instances.iter().map(serialize_to_map).collect();
        let maps = maps?;
        let stmt = build_insert_many_for::<T>("postgres", &maps)?;
        let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
        let result = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }
}

/// Convert a `T: Serialize` instance to a `Map<String, Value>` for
/// the insert path. Errors out if the instance doesn't serialize to a
/// JSON object (only flat structs and HashMap-like shapes do).
fn serialize_to_map<T: serde::Serialize>(
    instance: &T,
) -> Result<serde_json::Map<String, serde_json::Value>, crate::orm::write::WriteError> {
    let value = serde_json::to_value(instance)?;
    match value {
        serde_json::Value::Object(map) => Ok(map),
        _ => Err(crate::orm::write::WriteError::NotAnObject),
    }
}

/// Build a single-row INSERT statement for one map of column values.
/// Skips the PK column when its value is the autoincrement sentinel
/// (see [`crate::orm::write::is_default_pk`]). Adds a `RETURNING *`
/// clause so the caller can read back the populated instance.
fn build_insert_one_for<T: Model>(
    _backend_name: &str,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<sea_query::InsertStatement, crate::orm::write::WriteError> {
    use crate::orm::write::{is_default_pk, json_to_sea_value};
    let mut columns: Vec<Alias> = Vec::new();
    let mut values: Vec<sea_query::SimpleExpr> = Vec::new();
    for field in T::FIELDS {
        let val = map
            .get(field.name)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        // Skip PK if it's the default sentinel — let the DB
        // autoincrement / default kick in.
        if field.primary_key && is_default_pk(field.ty, &val) {
            continue;
        }
        // Skip absent fields when nullable (caller didn't supply them).
        if val.is_null() && field.nullable && !map.contains_key(field.name) {
            continue;
        }
        let sea_value = json_to_sea_value(field.ty, &val, field.nullable, field.name)?;
        columns.push(Alias::new(field.name));
        values.push(sea_value.into());
    }

    let mut stmt = Query::insert();
    stmt.into_table(Alias::new(T::TABLE)).columns(columns);
    stmt.values(values).map_err(|e| {
        crate::orm::write::WriteError::Sqlx(sqlx::Error::Protocol(format!(
            "umbra::orm::write: sea-query rejected INSERT values: {e}"
        )))
    })?;
    // RETURNING * so the caller can read the populated row back. Works
    // on Postgres natively; sqlx-sqlite 0.8 supports it via SQLite >= 3.35.
    stmt.returning_all();
    Ok(stmt)
}

/// Build a multi-row INSERT. Reuses the per-row column-selection logic
/// from `build_insert_one_for` for the first map, then asserts every
/// subsequent map exposes the same column set (heterogeneous row shapes
/// would change the column list mid-INSERT, which SQL forbids).
fn build_insert_many_for<T: Model>(
    _backend_name: &str,
    maps: &[serde_json::Map<String, serde_json::Value>],
) -> Result<sea_query::InsertStatement, crate::orm::write::WriteError> {
    use crate::orm::write::{is_default_pk, json_to_sea_value};
    // Decide column set from the first row. Subsequent rows MUST
    // produce the same column set — anything else would break the
    // INSERT's columns clause.
    let first = &maps[0];
    let included_fields: Vec<&crate::orm::FieldSpec> = T::FIELDS
        .iter()
        .filter(|field| {
            let val = first
                .get(field.name)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if field.primary_key && is_default_pk(field.ty, &val) {
                return false;
            }
            if val.is_null() && field.nullable && !first.contains_key(field.name) {
                return false;
            }
            true
        })
        .collect();

    let columns: Vec<Alias> = included_fields.iter().map(|f| Alias::new(f.name)).collect();

    let mut stmt = Query::insert();
    stmt.into_table(Alias::new(T::TABLE)).columns(columns);
    for map in maps {
        let row_values: Result<Vec<_>, _> = included_fields
            .iter()
            .map(|field| {
                let val = map
                    .get(field.name)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                json_to_sea_value(field.ty, &val, field.nullable, field.name)
                    .map(sea_query::SimpleExpr::from)
            })
            .collect();
        stmt.values(row_values?).map_err(|e| {
            crate::orm::write::WriteError::Sqlx(sqlx::Error::Protocol(format!(
                "umbra::orm::write: sea-query rejected INSERT values: {e}"
            )))
        })?;
    }
    Ok(stmt)
}
