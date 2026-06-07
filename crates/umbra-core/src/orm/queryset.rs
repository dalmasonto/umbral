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

use std::collections::HashMap;
use std::marker::PhantomData;

use sea_query::{
    Alias, Expr, Func, IntoIden, Order, PostgresQueryBuilder, Query, SqliteQueryBuilder,
};
use sea_query_binder::SqlxBinder;
use serde_json::Value as JsonValue;
use sqlx::Column as _;

use crate::db::DbPool;
use crate::orm::{FExpr, HydrateRelated, Model, OrderExpr, Predicate};

/// Entry point for queries on a model.
///
/// `Manager<T>` wraps a freshly-constructed `QuerySet<T>` and exposes
/// the same chainable surface. The user never constructs one directly;
/// `Post::objects()` is the only door.
pub struct Manager<T> {
    _phantom: PhantomData<T>,
    /// Per-Manager override for the `atomic_transactions` builder
    /// default. `None` = inherit the global default; `Some(true)` =
    /// wrap subsequent writes in a transaction; `Some(false)` =
    /// explicitly opt out. Propagates into the QuerySet `queryset()`
    /// constructs.
    atomic: Option<bool>,
}

impl<T> Manager<T> {
    pub(crate) fn new() -> Self {
        Self {
            _phantom: PhantomData,
            atomic: None,
        }
    }

    /// Wrap every write terminal that hangs off this Manager in a
    /// transaction. Equivalent to calling `.atomic()` on each
    /// QuerySet derived from it. Per-call `.non_atomic()` overrides.
    pub fn atomic(mut self) -> Self {
        self.atomic = Some(true);
        self
    }

    /// Opt this Manager (and every QuerySet derived from it) out of
    /// the global `App::builder().atomic_transactions(true)` default.
    pub fn non_atomic(mut self) -> Self {
        self.atomic = Some(false);
        self
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
    /// FK field names requested for eager loading via `select_related`.
    /// After the main query returns rows, a batch `IN (...)` query
    /// fetches the related rows for each named field and calls
    /// `HydrateRelated::hydrate_fk` to populate `ForeignKey.resolved`.
    pub(crate) select_related: Vec<String>,
    /// M2M field names requested for eager loading via
    /// `prefetch_related`. After the main query, one batched JOIN
    /// against the junction + child table fetches every related row
    /// for every parent in a single round-trip; each parent's
    /// `M2M.resolved` slot is populated via
    /// `HydrateRelated::set_m2m_resolved_json`. Gap #19.
    pub(crate) prefetch_related: Vec<String>,
    /// BUG-8: `#[umbra(ordering = [...])]` lowers to a default ORDER
    /// BY applied at terminal time when the caller didn't supply an
    /// explicit `.order_by(...)`. Mirrors Django's
    /// `Meta.ordering` semantics: explicit calls REPLACE the default
    /// rather than appending to it.
    pub(crate) default_ordering: Vec<(&'static str, bool)>,
    /// Set to `true` the first time `.order_by(...)` is called; when
    /// `false`, `build_query_for` applies `default_ordering`.
    pub(crate) explicit_order: bool,
    /// Per-QuerySet override for the `atomic_transactions` builder
    /// default. `None` = inherit the global default via
    /// [`crate::db::atomic_default`]; `Some(true)` = wrap this
    /// QuerySet's write terminal in a transaction; `Some(false)` =
    /// explicitly opt out.
    pub(crate) atomic: Option<bool>,
    _phantom: PhantomData<T>,
}

impl<T> QuerySet<T> {
    pub(crate) fn new(query: sea_query::SelectStatement) -> Self {
        Self {
            query,
            default_ordering: Vec::new(),
            explicit_order: false,
            predicates: Vec::new(),
            explicit_pool: None,
            select_related: Vec::new(),
            prefetch_related: Vec::new(),
            atomic: None,
            _phantom: PhantomData,
        }
    }

    /// Wrap this QuerySet's write terminal in a transaction. Reads are
    /// unaffected (read terminals are single statements and the DB
    /// gives them a consistent snapshot). Mutually exclusive with
    /// [`Self::on_tx`] — if both are set, `on_tx` wins (you're
    /// already inside a transaction, so wrapping again would deadlock
    /// or fail on backends without nested transactions).
    pub fn atomic(mut self) -> Self {
        self.atomic = Some(true);
        self
    }

    /// Opt this QuerySet's write terminal out of the global
    /// `App::builder().atomic_transactions(true)` default. Useful in
    /// hot-path batches where the caller already owns the outer
    /// transaction.
    pub fn non_atomic(mut self) -> Self {
        self.atomic = Some(false);
        self
    }

    /// Resolve whether this QuerySet should auto-wrap its write
    /// terminal in a transaction. Per-call override > builder global.
    pub(crate) fn should_atomic_wrap(&self) -> bool {
        self.atomic.unwrap_or_else(crate::db::atomic_default)
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
        // BUG-8: default ORDER BY applies only when the caller didn't
        // supply an explicit `.order_by(...)`. Mirrors Django's
        // `Meta.ordering` semantics.
        if !self.explicit_order {
            for (col, desc) in &self.default_ordering {
                let order = if *desc { Order::Desc } else { Order::Asc };
                q.order_by(Alias::new(*col), order);
            }
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

    /// Add a negated WHERE condition. The negated predicate ANDs into
    /// the chain alongside any `filter()` calls, so
    /// `.filter(A).exclude(B).filter(C)` renders as `WHERE A AND NOT B
    /// AND C`. Sugar for `filter(Q::not(p))`.
    ///
    /// Mirrors Django's `QuerySet.exclude()`.
    pub fn exclude(self, p: Predicate<T>) -> Self {
        self.filter(crate::orm::Q::not(p))
    }

    /// Add an ORDER BY clause. Multiple `.order_by` calls append.
    /// The first explicit call also opts out of the model's
    /// `#[umbra(ordering = [...])]` default (BUG-8) — Django semantics:
    /// explicit ordering replaces the default rather than stacking on
    /// top of it.
    pub fn order_by(mut self, o: OrderExpr<T>) -> Self {
        let order = if o.descending {
            Order::Desc
        } else {
            Order::Asc
        };
        self.query.order_by(Alias::new(o.column), order);
        self.explicit_order = true;
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

    /// Attach this `QuerySet` to an open transaction.
    ///
    /// Returns a [`QuerySetTx`] that holds both the query and a mutable
    /// reference to the transaction. Every terminal on `QuerySetTx`
    /// (`fetch`, `first`, `count`, `exists`, `get`, `delete`,
    /// `update_values`) executes inside the open transaction so all
    /// operations in the same closure commit or roll back as a unit.
    ///
    /// ```rust,ignore
    /// umbra::db::transaction(|tx| async move {
    ///     let order = Order::objects().on_tx(tx).create(new_order).await?;
    ///     Stock::objects()
    ///         .on_tx(tx)
    ///         .filter(stock::SKU.eq(sku))
    ///         .update_values(delta)
    ///         .await?;
    ///     Ok::<_, MyError>(order)
    /// }).await?;
    /// ```
    pub fn on_tx(self, tx: &mut crate::db::Transaction) -> QuerySetTx<'_, T> {
        QuerySetTx { qs: self, tx }
    }

    /// Eagerly load a single FK field by name.
    ///
    /// After the main SELECT returns rows, a batch `SELECT ... FROM <related_table>
    /// WHERE id IN (...)` fetches all referenced rows in one round-trip. Each
    /// returned row is deserialised as the target model and stored in
    /// `ForeignKey<U>.resolved` so template rendering (`{{ post.author.username }}`)
    /// and `serde_json::to_value(&post)["author"]["username"]` both work without
    /// additional queries.
    ///
    /// Calling `select_related` multiple times accumulates the names:
    /// `.select_related("author").select_related("editor")` works the same as
    /// `.select_related_many(&["author", "editor"])`.
    ///
    /// ## What is NOT in scope
    ///
    /// - Nested traversal (`"author__manager"`) — deferred. Only one-hop FKs
    ///   are supported. Chains require successive `.select_related` calls on
    ///   the resolved row, not a dot-notation shorthand.
    /// - Reverse FK (`prefetch_related`) — deferred. See gap 28 docs.
    /// - Many-to-many joins — deferred.
    pub fn select_related(mut self, field_name: impl Into<String>) -> Self {
        self.select_related.push(field_name.into());
        self
    }

    /// Eagerly load multiple FK fields in one call.
    ///
    /// Sugar for chained `.select_related(name)` calls.
    pub fn select_related_many(mut self, field_names: &[&str]) -> Self {
        for name in field_names {
            self.select_related.push(name.to_string());
        }
        self
    }

    /// Eagerly load an M2M relation via a single batched join.
    ///
    /// After the main SELECT returns rows, one query of the shape
    /// `SELECT j.parent_id, child.* FROM <child_table> child INNER
    /// JOIN <junction> j ON child.<pk> = j.child_id WHERE
    /// j.parent_id IN (...)` fetches every related child for every
    /// parent in one round-trip. Each parent's `M2M.resolved` slot
    /// is populated with its matching children.
    ///
    /// The M2M counterpart of [`Self::select_related`] for FKs —
    /// same goal of killing N+1. Mirrors Django's
    /// `prefetch_related('tags')`.
    ///
    /// ## Scope (v1)
    ///
    /// - **M2M only.** Reverse-FK collections
    ///   (`prefetch_related("comment_set")`) need a Vec-on-parent
    ///   slot that doesn't exist yet — a follow-up.
    /// - **i64 parent PK only.** Same constraint as the rest of the
    ///   M2M plumbing; models with non-i64 PKs surface a clean
    ///   compile error.
    /// - **Unknown field name** is a silent no-op — same forgiving
    ///   posture as `select_related`.
    pub fn prefetch_related(mut self, field_name: impl Into<String>) -> Self {
        self.prefetch_related.push(field_name.into());
        self
    }

    /// Eagerly load multiple M2M relations. Sugar for chained
    /// `.prefetch_related(name)` calls.
    pub fn prefetch_related_many(mut self, field_names: &[&str]) -> Self {
        for name in field_names {
            self.prefetch_related.push(name.to_string());
        }
        self
    }

    /// Convert this QuerySet into a [`Subquery`] suitable for use in
    /// an `IN (SELECT ...)` predicate. Projects only the named
    /// column; the accumulated WHERE / ORDER BY survive.
    ///
    /// `Post::objects().filter(...).into_subquery("author_id")` →
    /// `Subquery` you can hand to `user::ID.in_subquery(...)`.
    pub fn into_subquery(self, col_name: &str) -> crate::orm::Subquery {
        let mut q = self.build_query_for("sqlite");
        q.clear_selects();
        q.column(Alias::new(col_name));
        crate::orm::Subquery::from_select(q)
    }

    /// Combine this QuerySet with `other` via SQL `UNION` (gap #28).
    /// Both QuerySets must produce the same column shape — which
    /// they always do here because both are typed `QuerySet<T>`.
    /// Duplicates are removed (the de-duplicating UNION, not UNION
    /// ALL).
    pub fn union(self, other: QuerySet<T>) -> Self {
        self.combine(other, sea_query::UnionType::Distinct)
    }

    /// Combine this QuerySet with `other` via SQL `INTERSECT`
    /// (gap #28). Returns rows present in BOTH inputs.
    pub fn intersect(self, other: QuerySet<T>) -> Self {
        self.combine(other, sea_query::UnionType::Intersect)
    }

    /// Combine this QuerySet with `other` via SQL `EXCEPT`
    /// (gap #28). Returns rows present in `self` but not in `other`.
    pub fn except(self, other: QuerySet<T>) -> Self {
        self.combine(other, sea_query::UnionType::Except)
    }

    /// Internal: attach `other`'s SelectStatement to `self`'s
    /// SelectStatement with the given UnionType. Both sides apply
    /// their accumulated predicates / ORDER BY before the union.
    fn combine(mut self, other: QuerySet<T>, ty: sea_query::UnionType) -> Self {
        let backend = "sqlite";
        let other_select = other.build_query_for(backend);
        // Fold our own predicates into the base query so the union
        // sees them; further `.filter()` calls on the returned
        // QuerySet would still apply to the OUTER (combined) query.
        let mut base = self.build_query_for(backend);
        self.predicates.clear();
        base.union(ty, other_select);
        self.query = base;
        self
    }

    /// Emit `SELECT DISTINCT ...` for this query (gap #17). Most
    /// useful when combined with [`Self::values`] to dedupe a
    /// column-projected list (`distinct().values(&["tag"])`); the
    /// full-row DISTINCT is rarely what you want.
    ///
    /// Postgres-specific `DISTINCT ON (cols)` is deferred until a
    /// real consumer surfaces the need — the standard `DISTINCT`
    /// covers most use cases.
    pub fn distinct(mut self) -> Self {
        self.query.distinct();
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

/// Error type for [`QuerySet::get`] / [`Manager::get`] (Django's
/// exactly-one shape).
///
/// `.get()` deliberately returns this rather than `Result<Option<T>,
/// sqlx::Error>` because three outcomes need three branches:
///
/// - `Ok(row)` — exactly one matched.
/// - `Err(NotFound)` — zero matched. The common 404 path.
/// - `Err(MultipleObjectsReturned)` — more than one matched. A
///   data-integrity signal: filters that should pin a unique row
///   (PK lookup, UNIQUE-constrained column) hitting this variant
///   means an invariant has already broken upstream.
/// - `Err(Sqlx)` — the DB itself returned an error.
#[derive(Debug)]
pub enum GetError {
    NotFound,
    MultipleObjectsReturned,
    Sqlx(sqlx::Error),
}

impl std::fmt::Display for GetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no matching row"),
            Self::MultipleObjectsReturned => {
                write!(f, "expected exactly one row, found more")
            }
            Self::Sqlx(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for GetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlx(e) => Some(e),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for GetError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

/// Feature 29 — composite error returned by
/// [`QuerySet::try_for_each`]. The chunked streaming terminal can
/// fail in two ways and the call site usually wants to distinguish:
/// a SQL fetch failure is a system-level problem (DB went away,
/// schema mismatch, etc.), while a callback error is whatever
/// domain-specific failure the user's body produced (file write
/// blew up, validation rejected the row, etc.).
#[derive(Debug)]
pub enum TryForEachError<E> {
    /// A database fetch returned an error mid-iteration. The
    /// callback never saw this row.
    Sqlx(sqlx::Error),
    /// The user's callback returned an error for some row. The
    /// walk stopped immediately; rows after the failing one were
    /// not fetched.
    Callback(E),
}

impl<E: std::fmt::Display> std::fmt::Display for TryForEachError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlx(e) => write!(f, "{e}"),
            Self::Callback(e) => write!(f, "{e}"),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for TryForEachError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlx(e) => Some(e),
            Self::Callback(_) => None,
        }
    }
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
    ///
    /// If `.select_related(name)` was called, a follow-up batch query
    /// populates `ForeignKey<U>.resolved` for each named field before
    /// the rows are returned.
    pub async fn fetch(self) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        let sr_fields = self.select_related.clone();
        let prefetch_fields = self.prefetch_related.clone();
        // The turbofish on `query_as_with::<DB, _, _>` is load-bearing:
        // with both `sqlx-sqlite` and `sqlx-postgres` features on
        // sea-query-binder, `SqlxValues` implements `IntoArguments` for
        // both backends, so the compiler can't infer DB from the values
        // alone. Naming DB explicitly pins which `FromRow` bound is
        // checked.
        let mut rows = match resolve_pool::<T>(self.explicit_pool.clone()) {
            DbPool::Sqlite(pool) => {
                let q = self.build_query_for("sqlite");
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_all(&pool)
                    .await?
            }
            DbPool::Postgres(pool) => {
                let q = self.build_query_for("postgres");
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_all(&pool)
                    .await?
            }
        };
        // BUG-16 step 2: wire each row's PK into its `M2M<U>` slots so
        // `add`/`remove`/`clear` know which parent they belong to.
        // No-op for models with no M2M fields.
        for r in &mut rows {
            r.set_m2m_parent_ids();
        }
        if !sr_fields.is_empty() {
            let pool = resolve_pool::<T>(self.explicit_pool.clone());
            hydrate_select_related::<T>(&mut rows, &sr_fields, &pool).await?;
        }
        if !prefetch_fields.is_empty() {
            let pool = resolve_pool::<T>(self.explicit_pool.clone());
            hydrate_prefetch_related::<T>(&mut rows, &prefetch_fields, &pool).await?;
        }
        Ok(rows)
    }

    /// Feature 29 Phase 1 — chunked streaming via a callback.
    ///
    /// Runs the SELECT in pages of `chunk_size` rows and invokes
    /// `callback` once per row. Memory bound = `chunk_size *
    /// sizeof::<T>` instead of the full row count `fetch()` would
    /// buffer, so this is the right shape for million-row exports,
    /// migrations, and batch transforms.
    ///
    /// Deliberately NOT named `iterator()` — that name suggests a
    /// `Stream`-shaped return value, which would force a
    /// `futures-util` dep. The callback shape is idiomatic Rust,
    /// requires no new crates, and ships the same memory bound. A
    /// future `iterator()` returning `BoxStream<T>` can land later
    /// once `futures-util` is in the workspace for some other reason
    /// (likely SSE / WebSockets).
    ///
    /// Error contract: the callback may return any error type `E`.
    /// SQL failures become `TryForEachError::Sqlx`; callback errors
    /// become `TryForEachError::Callback(e)`. The first error stops
    /// the walk — subsequent rows are not fetched.
    ///
    /// Caveats: pages are stable only if the result set isn't being
    /// mutated concurrently. For consistent-snapshot iteration over
    /// a live table, wrap the call in a serialised-or-repeatable-read
    /// transaction. `select_related` and `prefetch_related` hooks
    /// are NOT applied on each row — `try_for_each` is intentionally
    /// the "raw column data, one row at a time" terminal.
    pub async fn try_for_each<F, E>(
        self,
        chunk_size: usize,
        mut callback: F,
    ) -> Result<(), TryForEachError<E>>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
        F: FnMut(T) -> Result<(), E>,
    {
        let chunk_size = chunk_size.max(1);
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let mut offset: u64 = 0;
        loop {
            let mut rows: Vec<T> = match &pool {
                DbPool::Sqlite(pg) => {
                    let mut q = self.build_query_for("sqlite");
                    q.limit(chunk_size as u64).offset(offset);
                    let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                    sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                        .fetch_all(pg)
                        .await
                        .map_err(TryForEachError::Sqlx)?
                }
                DbPool::Postgres(pg) => {
                    let mut q = self.build_query_for("postgres");
                    q.limit(chunk_size as u64).offset(offset);
                    let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                    sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                        .fetch_all(pg)
                        .await
                        .map_err(TryForEachError::Sqlx)?
                }
            };
            let fetched = rows.len();
            if fetched == 0 {
                break;
            }
            for row in rows.drain(..) {
                callback(row).map_err(TryForEachError::Callback)?;
            }
            if fetched < chunk_size {
                break;
            }
            offset += fetched as u64;
        }
        Ok(())
    }

    /// Run the SELECT with LIMIT 1 and return the first row, if any.
    pub async fn first(mut self) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        let sr_fields = self.select_related.clone();
        self.query.limit(1);
        let row = match resolve_pool::<T>(self.explicit_pool.clone()) {
            DbPool::Sqlite(pool) => {
                let q = self.build_query_for("sqlite");
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_optional(&pool)
                    .await?
            }
            DbPool::Postgres(pool) => {
                let q = self.build_query_for("postgres");
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_optional(&pool)
                    .await?
            }
        };
        if row.is_none() {
            return Ok(row);
        }
        // BUG-16 step 2: wire the row's PK into its M2M slots.
        let mut rows = vec![row.unwrap()];
        rows[0].set_m2m_parent_ids();
        if sr_fields.is_empty() {
            return Ok(rows.pop());
        }
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        hydrate_select_related::<T>(&mut rows, &sr_fields, &pool).await?;
        Ok(Some(rows.pop().unwrap()))
    }

    /// Return the row with the smallest value in `col_name`. Sugar
    /// for `order_by(col.asc()).first()`. Mirrors Django's
    /// `QuerySet.earliest('created_at')`.
    ///
    /// Takes a `&'static str` column name (same shape as
    /// `select_related`) so the call site stays Django-flavoured —
    /// `.earliest("created_at")` reads naturally without spelling out
    /// `.asc()`.
    pub async fn earliest(self, col_name: &'static str) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        self.order_by(OrderExpr::new(col_name, false)).first().await
    }

    /// Return the row with the largest value in `col_name`. Sugar
    /// for `order_by(col.desc()).first()`. Mirrors Django's
    /// `QuerySet.latest('created_at')`.
    pub async fn latest(self, col_name: &'static str) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        self.order_by(OrderExpr::new(col_name, true)).first().await
    }

    /// Fetch many rows by their primary keys and return a
    /// `HashMap<i64, T>` keyed by PK. The everyday companion to a
    /// cached list of ids — `User::objects().in_bulk(user_ids)`
    /// gives you direct lookup access without a second
    /// `.iter().find(...)` pass per id.
    ///
    /// Missing ids are silently absent from the map; callers that
    /// need the existence check can compare `map.len()` to
    /// `pks.len()`. Empty input is a no-op (returns the empty map).
    ///
    /// v1 limitation: i64-PK models only (matches `pk_i64()`'s
    /// constraint). Non-i64 PK models silently drop every row from
    /// the result map.
    pub async fn in_bulk(self, pks: Vec<i64>) -> Result<HashMap<i64, T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        if pks.is_empty() {
            return Ok(HashMap::new());
        }
        let pk_name = pk_field::<T>().map(|f| f.name).unwrap_or("id");
        let pk_pred: Predicate<T> =
            Predicate::new(Expr::col(Alias::new(pk_name)).is_in(pks.iter().copied()));
        let rows = self.filter(pk_pred).fetch().await?;
        let mut out: HashMap<i64, T> = HashMap::with_capacity(rows.len());
        for row in rows {
            if let Some(id) = row.pk_i64() {
                out.insert(id, row);
            }
        }
        Ok(out)
    }

    /// Return the database's execution plan for this query as a
    /// plain-text string. Doesn't run the underlying query — just
    /// asks the DB how it would be executed.
    ///
    /// Backend dispatch:
    ///
    /// - SQLite: `EXPLAIN QUERY PLAN <sql>` — returns the planner's
    ///   nested loop hierarchy, one row per access step.
    /// - Postgres: `EXPLAIN <sql>` — returns the default text plan.
    ///   For machine-readable output use raw sqlx with
    ///   `EXPLAIN (FORMAT JSON)`; the framework defaults to text
    ///   because most callers want eyeball-able output.
    ///
    /// Lines are joined with newlines. The returned string is what a
    /// developer would paste into a debugger or a perf-review issue.
    pub async fn explain(self) -> Result<String, sqlx::Error> {
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        let q = self.build_query_for(backend);
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let explain_sql = format!("EXPLAIN QUERY PLAN {sql}");
                let rows = sqlx::query_with::<sqlx::Sqlite, _>(&explain_sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out = String::new();
                for row in &rows {
                    use sqlx::Row;
                    // SQLite returns: id, parent, notused, detail.
                    // The `detail` column is the human-readable step.
                    let detail: String = row.try_get("detail")?;
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&detail);
                }
                Ok(out)
            }
            DbPool::Postgres(pool) => {
                let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                let explain_sql = format!("EXPLAIN {sql}");
                let rows = sqlx::query_with::<sqlx::Postgres, _>(&explain_sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out = String::new();
                for row in &rows {
                    use sqlx::Row;
                    // Postgres EXPLAIN returns one column named
                    // "QUERY PLAN", one row per line of the plan.
                    let line: String = row.try_get("QUERY PLAN")?;
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&line);
                }
                Ok(out)
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
        // Postgres rejects `"*"` as a quoted identifier (SQLite tolerates
        // it); use sea_query's Asterisk token which renders bare `*`
        // on both backends.
        rebuilt.expr(Func::count(Expr::col(sea_query::Asterisk)));
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
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        let rows = self.limit(1).fetch().await?;
        Ok(!rows.is_empty())
    }

    /// `.get()` — Django's exactly-one terminal.
    ///
    /// Returns `Ok(row)` when the filter chain matches exactly one
    /// row. The two not-exactly-one cases each get their own
    /// `GetError` variant so the caller can branch deliberately:
    ///
    /// - [`GetError::NotFound`] — zero rows matched. The right
    ///   choice for "fetch the row this user just clicked on; 404
    ///   if it's gone."
    /// - [`GetError::MultipleObjectsReturned`] — more than one row
    ///   matched. The right choice for filters that should be
    ///   uniquely-keyed (e.g. `.filter(user::EMAIL.eq("..."))`
    ///   when email has a UNIQUE constraint); a result of 2+ is a
    ///   data-integrity bug worth crashing on.
    /// - The underlying sqlx error wraps as [`GetError::Sqlx`].
    ///
    /// Internally this issues `SELECT ... LIMIT 2` — the cheapest
    /// way to distinguish "one row" from "many." The second row, if
    /// it exists, isn't materialised beyond the bare FromRow call.
    ///
    /// ```ignore
    /// match Post::objects().filter(post::ID.eq(42)).get().await {
    ///     Ok(p)                                            => /* render */,
    ///     Err(GetError::NotFound)                          => /* 404 */,
    ///     Err(GetError::MultipleObjectsReturned)           => unreachable!("ID is unique"),
    ///     Err(GetError::Sqlx(e))                           => /* 500 */,
    /// }
    /// ```
    pub async fn get(self) -> Result<T, GetError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        let mut rows = self.limit(2).fetch().await.map_err(GetError::Sqlx)?;
        match rows.len() {
            0 => Err(GetError::NotFound),
            1 => Ok(rows.pop().unwrap()),
            _ => Err(GetError::MultipleObjectsReturned),
        }
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

    /// Project the query to only the named columns, returning a
    /// vector of `serde_json::Value::Object` rows instead of typed
    /// `T` instances. Mirrors Django's `QuerySet.values('id', 'title')`.
    ///
    /// Use when a list view only needs a few fields — skipping the
    /// 50KB body BLOB on every Post saves both memory and the
    /// FromRow hydration overhead. Each returned `Value` is an
    /// object keyed by the requested column names, with values
    /// typed per the column's declared SqlType (integers stay
    /// integers, booleans stay booleans, dates render as ISO
    /// strings).
    ///
    /// Unknown column names fail loudly with
    /// `sqlx::Error::Protocol` naming the offending column.
    /// Composes with `filter`, `exclude`, `order_by`, `limit`,
    /// `offset` exactly the way the typed terminals do.
    ///
    /// ```rust,ignore
    /// let rows = Post::objects()
    ///     .filter(post::PUBLISHED.eq(true))
    ///     .order_by(post::ID.desc())
    ///     .values(&["id", "title"])
    ///     .await?;
    /// // [ { "id": 3, "title": "c" }, ... ]
    /// ```
    pub async fn values(self, columns: &[&str]) -> Result<Vec<JsonValue>, sqlx::Error> {
        let meta = crate::migrate::ModelMeta::for_::<T>();
        // Resolve every requested name against the model's metadata
        // up front so an unknown column errors before any SQL runs.
        let mut chosen: Vec<&crate::migrate::Column> = Vec::with_capacity(columns.len());
        for name in columns {
            let col = meta
                .fields
                .iter()
                .find(|c| c.name == *name)
                .ok_or_else(|| {
                    sqlx::Error::Protocol(format!(
                        "umbra::orm::values: unknown column `{}` on model `{}`",
                        name,
                        T::NAME
                    ))
                })?;
            chosen.push(col);
        }
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        // Build the base query (predicates + ORDER BY) then swap its
        // SELECT list for only the requested columns.
        let mut q = self.build_query_for(backend);
        q.clear_selects();
        for col in &chosen {
            q.column(Alias::new(col.name.as_str()));
        }
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out: Vec<JsonValue> = Vec::with_capacity(rows.len());
                for row in &rows {
                    let mut obj = serde_json::Map::with_capacity(chosen.len());
                    for col in &chosen {
                        let v = crate::orm::dynamic::decode_to_json(row, col)?;
                        obj.insert(col.name.clone(), v);
                    }
                    out.push(JsonValue::Object(obj));
                }
                Ok(out)
            }
            DbPool::Postgres(pool) => {
                let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out: Vec<JsonValue> = Vec::with_capacity(rows.len());
                for row in &rows {
                    let mut obj = serde_json::Map::with_capacity(chosen.len());
                    for col in &chosen {
                        let v = crate::orm::dynamic::decode_pg_to_json(row, col)?;
                        obj.insert(col.name.clone(), v);
                    }
                    out.push(JsonValue::Object(obj));
                }
                Ok(out)
            }
        }
    }

    /// Single-row aggregate. Runs `SELECT AGG(col) AS name, ...` with
    /// the QuerySet's accumulated WHERE clause (ORDER BY / LIMIT /
    /// OFFSET are dropped — they make no sense over an aggregate
    /// without GROUP BY).
    ///
    /// Returns a `serde_json::Value::Object` keyed by the supplied
    /// names. COUNT comes back as an integer; AVG as a float; SUM /
    /// MAX / MIN inherit the source column's declared type.
    ///
    /// ```rust,ignore
    /// use umbra::orm::Aggregate;
    /// let summary = Post::objects()
    ///     .filter(post::PUBLISHED.eq(true))
    ///     .aggregate(&[
    ///         ("count", Aggregate::count()),
    ///         ("total", Aggregate::sum("view_count")),
    ///     ])
    ///     .await?;
    /// // { "count": 42, "total": 9999 }
    /// ```
    pub async fn aggregate(
        self,
        aggs: &[(&str, crate::orm::Aggregate)],
    ) -> Result<JsonValue, sqlx::Error> {
        let meta = crate::migrate::ModelMeta::for_::<T>();
        // Validate every aggregate's source column exists.
        for (name, agg) in aggs {
            if let Some(col) = agg.source_column()
                && !meta.fields.iter().any(|c| c.name == col)
            {
                return Err(sqlx::Error::Protocol(format!(
                    "umbra::orm::aggregate: unknown column `{}` on model `{}` for aggregate `{}`",
                    col,
                    T::NAME,
                    name
                )));
            }
        }
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        let mut q = self.build_query_for(backend);
        q.clear_selects();
        q.reset_limit();
        q.reset_offset();
        for (name, agg) in aggs {
            q.expr_as(agg.to_simple_expr(), Alias::new(*name));
        }
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let row = sqlx::query_with::<sqlx::Sqlite, _>(&sql, vals)
                    .fetch_one(&pool)
                    .await?;
                let mut obj = serde_json::Map::with_capacity(aggs.len());
                for (name, agg) in aggs {
                    let source_ty = agg
                        .source_column()
                        .and_then(|c| meta.fields.iter().find(|f| f.name == c).map(|f| f.ty));
                    obj.insert(
                        name.to_string(),
                        decode_agg_sqlite(&row, name, agg, source_ty)?,
                    );
                }
                Ok(JsonValue::Object(obj))
            }
            DbPool::Postgres(pool) => {
                let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                let row = sqlx::query_with::<sqlx::Postgres, _>(&sql, vals)
                    .fetch_one(&pool)
                    .await?;
                let mut obj = serde_json::Map::with_capacity(aggs.len());
                for (name, agg) in aggs {
                    let source_ty = agg
                        .source_column()
                        .and_then(|c| meta.fields.iter().find(|f| f.name == c).map(|f| f.ty));
                    obj.insert(name.to_string(), decode_agg_pg(&row, name, agg, source_ty)?);
                }
                Ok(JsonValue::Object(obj))
            }
        }
    }

    /// Grouped aggregate. Runs `SELECT <group_cols>, AGG(col) AS name,
    /// ... GROUP BY <group_cols>` with the accumulated WHERE clause.
    ///
    /// Returns one `Value::Object` per group, with both the group
    /// columns and each named aggregate as fields. Group columns are
    /// decoded per their declared SqlType (so an integer
    /// `author_id` stays a JSON number).
    ///
    /// ```rust,ignore
    /// let by_author = Post::objects()
    ///     .annotate(&["author_id"], &[("count", Aggregate::count())])
    ///     .await?;
    /// // [ { "author_id": 1, "count": 3 }, { "author_id": 2, "count": 2 } ]
    /// ```
    pub async fn annotate(
        self,
        group_cols: &[&str],
        aggs: &[(&str, crate::orm::Aggregate)],
    ) -> Result<Vec<JsonValue>, sqlx::Error> {
        let meta = crate::migrate::ModelMeta::for_::<T>();
        // Resolve group columns up front so unknown names fail
        // before any SQL runs.
        let mut chosen_groups: Vec<&crate::migrate::Column> = Vec::with_capacity(group_cols.len());
        for name in group_cols {
            let col = meta
                .fields
                .iter()
                .find(|c| c.name == *name)
                .ok_or_else(|| {
                    sqlx::Error::Protocol(format!(
                        "umbra::orm::annotate: unknown group column `{}` on model `{}`",
                        name,
                        T::NAME
                    ))
                })?;
            chosen_groups.push(col);
        }
        // Validate aggregate source columns.
        for (name, agg) in aggs {
            if let Some(col) = agg.source_column()
                && !meta.fields.iter().any(|c| c.name == col)
            {
                return Err(sqlx::Error::Protocol(format!(
                    "umbra::orm::annotate: unknown column `{}` on model `{}` for aggregate `{}`",
                    col,
                    T::NAME,
                    name
                )));
            }
        }
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        let mut q = self.build_query_for(backend);
        q.clear_selects();
        // GROUP BY columns appear in the SELECT list AND the GROUP BY
        // clause. Aggregates only in the SELECT.
        for col in &chosen_groups {
            q.column(Alias::new(col.name.as_str()));
            q.add_group_by([sea_query::SimpleExpr::Column(sea_query::ColumnRef::Column(
                Alias::new(col.name.as_str()).into_iden(),
            ))]);
        }
        for (name, agg) in aggs {
            q.expr_as(agg.to_simple_expr(), Alias::new(*name));
        }
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out: Vec<JsonValue> = Vec::with_capacity(rows.len());
                for row in &rows {
                    let mut obj = serde_json::Map::with_capacity(chosen_groups.len() + aggs.len());
                    for col in &chosen_groups {
                        obj.insert(
                            col.name.clone(),
                            crate::orm::dynamic::decode_to_json(row, col)?,
                        );
                    }
                    for (name, agg) in aggs {
                        let source_ty = agg
                            .source_column()
                            .and_then(|c| meta.fields.iter().find(|f| f.name == c).map(|f| f.ty));
                        obj.insert(
                            name.to_string(),
                            decode_agg_sqlite(row, name, agg, source_ty)?,
                        );
                    }
                    out.push(JsonValue::Object(obj));
                }
                Ok(out)
            }
            DbPool::Postgres(pool) => {
                let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out: Vec<JsonValue> = Vec::with_capacity(rows.len());
                for row in &rows {
                    let mut obj = serde_json::Map::with_capacity(chosen_groups.len() + aggs.len());
                    for col in &chosen_groups {
                        obj.insert(
                            col.name.clone(),
                            crate::orm::dynamic::decode_pg_to_json(row, col)?,
                        );
                    }
                    for (name, agg) in aggs {
                        let source_ty = agg
                            .source_column()
                            .and_then(|c| meta.fields.iter().find(|f| f.name == c).map(|f| f.ty));
                        obj.insert(name.to_string(), decode_agg_pg(row, name, agg, source_ty)?);
                    }
                    out.push(JsonValue::Object(obj));
                }
                Ok(out)
            }
        }
    }

    /// `DELETE FROM table WHERE <predicates>`. Returns the number of
    /// rows deleted. With no `.filter` calls, deletes every row.
    ///
    /// Fires `bulk_post_delete:<table>` once with the list of removed
    /// PKs when at least one row was deleted. Per-row `pre_delete` /
    /// `post_delete` are NOT fired by this path — use
    /// [`Manager::delete_instance`] when per-row signal semantics are
    /// required.
    pub async fn delete(self) -> Result<u64, sqlx::Error> {
        let atomic = self.should_atomic_wrap();
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        let mut stmt = self.build_delete_for(backend);
        let pk = pk_field::<T>();
        if let Some(field) = pk {
            stmt.returning_col(Alias::new(field.name));
        }
        let ids: Vec<JsonValue> = match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let rows = if atomic {
                    let mut tx = pool.begin().await?;
                    let r = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                        .fetch_all(&mut *tx)
                        .await;
                    match r {
                        Ok(rows) => {
                            tx.commit().await?;
                            rows
                        }
                        Err(e) => {
                            let _ = tx.rollback().await;
                            return Err(e);
                        }
                    }
                } else {
                    sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?
                };
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| pk_to_json_sqlite(r, field.name, field.ty))
                        .collect::<Result<_, _>>()?,
                    None => Vec::new(),
                }
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let rows = if atomic {
                    let mut tx = pool.begin().await?;
                    let r = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                        .fetch_all(&mut *tx)
                        .await;
                    match r {
                        Ok(rows) => {
                            tx.commit().await?;
                            rows
                        }
                        Err(e) => {
                            let _ = tx.rollback().await;
                            return Err(e);
                        }
                    }
                } else {
                    sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?
                };
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| pk_to_json_pg(r, field.name, field.ty))
                        .collect::<Result<_, _>>()?,
                    None => Vec::new(),
                }
            }
        };
        let count = ids.len() as u64;
        if !ids.is_empty() {
            crate::signals::emit_bulk_post_delete::<T>(ids).await;
        }
        Ok(count)
    }

    /// `UPDATE table SET col = <expr> WHERE <predicates>` using an
    /// F-expression for the new value.
    ///
    /// This is the companion to `update_values` for atomic column
    /// arithmetic. `F::col("views").add(1)` produces an [`FExpr`] that
    /// renders as `SET views = views + 1` — the database computes the
    /// increment atomically on the server side rather than needing a
    /// read-modify-write round-trip in application code.
    ///
    /// ```rust,ignore
    /// use umbra::orm::F;
    ///
    /// Post::objects()
    ///     .filter(post::ID.eq(42))
    ///     .update_expr("views", F::col("views").add(1))
    ///     .await?;
    /// ```
    ///
    /// Mixing `update_values` and `update_expr` for different columns in
    /// one statement requires two separate calls. A combined API (a map
    /// where values can be either JSON or FExpr) would require a new sum
    /// type; deferred until a consumer surfaces the need.
    pub async fn update_expr(
        self,
        col_name: &str,
        expr: FExpr,
    ) -> Result<u64, crate::orm::write::WriteError> {
        use crate::orm::write::WriteError;
        // Validate the column exists on the model.
        let field = T::FIELDS
            .iter()
            .find(|f| f.name == col_name)
            .ok_or_else(|| WriteError::UnknownColumn {
                field: col_name.to_string(),
            })?;
        if field.primary_key {
            // Silently skip PK rewrites, same as update_values.
            return Ok(0);
        }
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();

        let mut stmt = sea_query::Query::update();
        stmt.table(Alias::new(T::TABLE));
        stmt.value(Alias::new(field.name), expr.to_simple_expr());
        for p in &self.predicates {
            stmt.and_where(p.cond_for(backend));
        }
        let pk = pk_field::<T>();
        if let Some(pkf) = pk {
            stmt.returning_col(Alias::new(pkf.name));
        }

        let ids: Vec<JsonValue> = match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .fetch_all(&pool)
                    .await?;
                match pk {
                    Some(pkf) => rows
                        .iter()
                        .map(|r| pk_to_json_sqlite(r, pkf.name, pkf.ty))
                        .collect::<Result<_, _>>()
                        .map_err(crate::orm::write::WriteError::Sqlx)?,
                    None => Vec::new(),
                }
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .fetch_all(&pool)
                    .await?;
                match pk {
                    Some(pkf) => rows
                        .iter()
                        .map(|r| pk_to_json_pg(r, pkf.name, pkf.ty))
                        .collect::<Result<_, _>>()
                        .map_err(crate::orm::write::WriteError::Sqlx)?,
                    None => Vec::new(),
                }
            }
        };
        let count = ids.len() as u64;
        if !ids.is_empty() {
            crate::signals::emit_bulk_post_save::<T>(ids, false).await;
        }
        Ok(count)
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
    ///
    /// Fires `bulk_post_save:<table>` once with `{ ids, created:
    /// false, actor }` when at least one row matched. Per-row
    /// `pre_save` / `post_save` are NOT fired — use [`Manager::save`]
    /// when per-row signal semantics are required.
    pub async fn update_values(
        self,
        values: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, crate::orm::write::WriteError> {
        let atomic = self.should_atomic_wrap();
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        let mut stmt = self.build_update_for(backend, &values)?;
        // RETURNING <pk> so bulk_post_save can include the matched ids.
        let pk = pk_field::<T>();
        if let Some(field) = pk {
            stmt.returning_col(Alias::new(field.name));
        }
        let ids: Vec<JsonValue> = match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let rows = if atomic {
                    let mut tx = pool
                        .begin()
                        .await
                        .map_err(crate::orm::write::WriteError::Sqlx)?;
                    let r = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                        .fetch_all(&mut *tx)
                        .await;
                    match r {
                        Ok(rows) => {
                            tx.commit()
                                .await
                                .map_err(crate::orm::write::WriteError::Sqlx)?;
                            rows
                        }
                        Err(e) => {
                            let _ = tx.rollback().await;
                            return Err(crate::orm::write::WriteError::Sqlx(e));
                        }
                    }
                } else {
                    sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?
                };
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| pk_to_json_sqlite(r, field.name, field.ty))
                        .collect::<Result<_, _>>()
                        .map_err(crate::orm::write::WriteError::Sqlx)?,
                    None => Vec::new(),
                }
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let rows = if atomic {
                    let mut tx = pool
                        .begin()
                        .await
                        .map_err(crate::orm::write::WriteError::Sqlx)?;
                    let r = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                        .fetch_all(&mut *tx)
                        .await;
                    match r {
                        Ok(rows) => {
                            tx.commit()
                                .await
                                .map_err(crate::orm::write::WriteError::Sqlx)?;
                            rows
                        }
                        Err(e) => {
                            let _ = tx.rollback().await;
                            return Err(crate::orm::write::WriteError::Sqlx(e));
                        }
                    }
                } else {
                    sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?
                };
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| pk_to_json_pg(r, field.name, field.ty))
                        .collect::<Result<_, _>>()
                        .map_err(crate::orm::write::WriteError::Sqlx)?,
                    None => Vec::new(),
                }
            }
        };
        let count = ids.len() as u64;
        if !ids.is_empty() {
            crate::signals::emit_bulk_post_save::<T>(ids, false).await;
        }
        Ok(count)
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
        rebuilt.expr(Func::count(Expr::col(sea_query::Asterisk)));
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

    /// Exactly-one terminal against an explicit `PgPool`.
    /// See [`QuerySet::get`] for the error-variant semantics.
    pub async fn get_pg(self, pool: &sqlx::PgPool) -> Result<T, GetError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let mut rows = self.limit(2).fetch_pg(pool).await.map_err(GetError::Sqlx)?;
        match rows.len() {
            0 => Err(GetError::NotFound),
            1 => Ok(rows.pop().unwrap()),
            _ => Err(GetError::MultipleObjectsReturned),
        }
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
        let mut qs = QuerySet::new(query);
        // BUG-8: seed the default ORDER BY from `Model::ORDERING` so
        // terminals that don't see an explicit `.order_by(...)` still
        // get a deterministic row order.
        qs.default_ordering = T::ORDERING.to_vec();
        // Propagate the Manager's atomic override so QuerySet
        // terminals inherit it without the caller re-specifying.
        qs.atomic = self.atomic;
        qs
    }

    /// Resolve whether write terminals on this Manager should auto-wrap
    /// in a transaction. Per-call override > builder global.
    fn should_atomic_wrap(&self) -> bool {
        self.atomic.unwrap_or_else(crate::db::atomic_default)
    }

    /// See `QuerySet::filter`.
    pub fn filter(&self, p: Predicate<T>) -> QuerySet<T> {
        self.queryset().filter(p)
    }

    /// See `QuerySet::exclude`.
    pub fn exclude(&self, p: Predicate<T>) -> QuerySet<T> {
        self.queryset().exclude(p)
    }

    /// See `QuerySet::into_subquery`.
    pub fn into_subquery(&self, col_name: &str) -> crate::orm::Subquery {
        self.queryset().into_subquery(col_name)
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
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        self.queryset().fetch().await
    }

    /// See `QuerySet::first`.
    pub async fn first(&self) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
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
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        self.queryset().exists().await
    }

    /// `.get(predicate)` — sugar for `.filter(predicate).get()`.
    ///
    /// The Django-shape one-liner: `User::objects().get(user::ID.eq(1))`.
    /// See [`QuerySet::get`] for error-variant semantics.
    pub async fn get(&self, p: Predicate<T>) -> Result<T, GetError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        self.queryset().filter(p).get().await
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

    /// Postgres-only sugar for `.filter(predicate).get_pg(pool)`.
    pub async fn get_pg(&self, pool: &sqlx::PgPool, p: Predicate<T>) -> Result<T, GetError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.queryset().filter(p).get_pg(pool).await
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
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        use crate::orm::write::WriteError;
        let map = serialize_to_map(&instance)?;

        // Same pre-DB validation pipeline the dynamic
        // `insert_json` path runs — choices + FK existence +
        // M2M shape. Empty-string + required-field checks are
        // intentionally relaxed on the typed path: a Rust
        // `pub title: String` field set to `""` is the caller's
        // deliberate choice, not a form-default leak, and
        // missing-required can't happen because the struct
        // forced the caller to supply every column at compile
        // time. We only validate the things the typed path
        // can't catch at compile time.
        let meta = crate::migrate::ModelMeta::for_::<T>();
        let validation_errors = crate::orm::validation::validate_on_typed_create(&meta, &map).await;
        if !validation_errors.is_empty() {
            return Err(WriteError::Multiple {
                errors: validation_errors,
            });
        }

        let pool = resolve_pool::<T>(None);
        let backend = pool.backend_name();
        let stmt = build_insert_one_for::<T>(backend, &map)?;
        let atomic = self.should_atomic_wrap();
        // Post-execution SQL classification: turns the DB's
        // UNIQUE / FK / NOT NULL / CHECK violations into the
        // structured `WriteError` variants instead of a raw
        // `Sqlx(_)` 500. Symmetric with `DynQuerySet::insert_json`.
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let row_result = if atomic {
                    let mut tx = pool.begin().await.map_err(WriteError::Sqlx)?;
                    let r = sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                        .fetch_one(&mut *tx)
                        .await;
                    match r {
                        Ok(row) => {
                            tx.commit().await.map_err(WriteError::Sqlx)?;
                            Ok(row)
                        }
                        Err(e) => {
                            let _ = tx.rollback().await;
                            Err(e)
                        }
                    }
                } else {
                    sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                        .fetch_one(&pool)
                        .await
                };
                let mut row = row_result.map_err(|e| {
                    crate::orm::validation::classify_sql_error(&e, &map)
                        .unwrap_or(WriteError::Sqlx(e))
                })?;
                // BUG-16 step 2: every materialised row, including the
                // post-INSERT readback, needs `parent_id` +
                // `junction_table` seeded on its M2M slots — otherwise
                // `row.tags.add(...)` is a silent no-op.
                row.set_m2m_parent_ids();
                Ok(row)
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let row_result = if atomic {
                    let mut tx = pool.begin().await.map_err(WriteError::Sqlx)?;
                    let r = sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                        .fetch_one(&mut *tx)
                        .await;
                    match r {
                        Ok(row) => {
                            tx.commit().await.map_err(WriteError::Sqlx)?;
                            Ok(row)
                        }
                        Err(e) => {
                            let _ = tx.rollback().await;
                            Err(e)
                        }
                    }
                } else {
                    sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                        .fetch_one(&pool)
                        .await
                };
                let mut row = row_result.map_err(|e| {
                    crate::orm::validation::classify_sql_error(&e, &map)
                        .unwrap_or(WriteError::Sqlx(e))
                })?;
                row.set_m2m_parent_ids();
                Ok(row)
            }
        }
    }

    /// INSERT many rows in a single statement. Returns the number of
    /// rows inserted. The full populated rows aren't materialised —
    /// use a follow-up `Model::objects().filter(...).fetch()` if you
    /// need them.
    ///
    /// Empty input is a no-op (returns Ok(0)) — the alternative
    /// (building an `INSERT INTO t () VALUES ()` and failing at the
    /// DB) doesn't help anyone.
    ///
    /// Fires `bulk_post_save:<table>` once with `{ ids, created: true,
    /// actor }` when at least one row was inserted. Per-row
    /// `pre_save` / `post_save` are NOT fired — use [`Self::save`]
    /// when per-row signal semantics are required.
    pub async fn bulk_create(&self, instances: Vec<T>) -> Result<u64, crate::orm::write::WriteError>
    where
        T: serde::Serialize,
    {
        use crate::orm::write::WriteError;
        if instances.is_empty() {
            return Ok(0);
        }
        let maps: Result<Vec<_>, _> = instances.iter().map(serialize_to_map).collect();
        let maps = maps?;
        // Validate every instance through the typed-create
        // pipeline. Collected into one `Multiple` so a caller
        // that submitted ten rows and got two bad ones can fix
        // both in one pass.
        let meta = crate::migrate::ModelMeta::for_::<T>();
        let mut all_errors: Vec<WriteError> = Vec::new();
        for map in &maps {
            let errs = crate::orm::validation::validate_on_typed_create(&meta, map).await;
            all_errors.extend(errs);
        }
        if !all_errors.is_empty() {
            return Err(WriteError::Multiple { errors: all_errors });
        }
        let pool = resolve_pool::<T>(None);
        let backend = pool.backend_name();
        let mut stmt = build_insert_many_for::<T>(backend, &maps)?;
        // First row's map is used to enrich UNIQUE / FK
        // messages with the offending value when the engine
        // doesn't name it. Imperfect for bulk (the failing row
        // could be later in the batch) but better than the raw
        // sqlx error.
        let first_map = maps.first().cloned().unwrap_or_default();
        // Add `RETURNING <pk>` so the bulk_post_save signal payload can
        // carry the inserted PKs. Both backends support this — SQLite
        // since 3.35, Postgres natively. Replaces the previous
        // `execute()` + rows_affected path; count comes from the
        // returned row vector instead.
        let pk = pk_field::<T>();
        if let Some(field) = pk {
            stmt.returning_col(Alias::new(field.name));
        }
        let atomic = self.should_atomic_wrap();
        let ids: Vec<JsonValue> = match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let rows = if atomic {
                    let mut tx = pool.begin().await.map_err(WriteError::Sqlx)?;
                    let r = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                        .fetch_all(&mut *tx)
                        .await;
                    match r {
                        Ok(rows) => {
                            tx.commit().await.map_err(WriteError::Sqlx)?;
                            rows
                        }
                        Err(e) => {
                            let _ = tx.rollback().await;
                            return Err(crate::orm::validation::classify_sql_error(&e, &first_map)
                                .unwrap_or(WriteError::Sqlx(e)));
                        }
                    }
                } else {
                    sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| {
                            crate::orm::validation::classify_sql_error(&e, &first_map)
                                .unwrap_or(WriteError::Sqlx(e))
                        })?
                };
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| pk_to_json_sqlite(r, field.name, field.ty))
                        .collect::<Result<_, _>>()
                        .map_err(WriteError::Sqlx)?,
                    None => Vec::new(),
                }
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let rows = if atomic {
                    let mut tx = pool.begin().await.map_err(WriteError::Sqlx)?;
                    let r = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                        .fetch_all(&mut *tx)
                        .await;
                    match r {
                        Ok(rows) => {
                            tx.commit().await.map_err(WriteError::Sqlx)?;
                            rows
                        }
                        Err(e) => {
                            let _ = tx.rollback().await;
                            return Err(crate::orm::validation::classify_sql_error(&e, &first_map)
                                .unwrap_or(WriteError::Sqlx(e)));
                        }
                    }
                } else {
                    sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| {
                            crate::orm::validation::classify_sql_error(&e, &first_map)
                                .unwrap_or(WriteError::Sqlx(e))
                        })?
                };
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| pk_to_json_pg(r, field.name, field.ty))
                        .collect::<Result<_, _>>()
                        .map_err(WriteError::Sqlx)?,
                    None => Vec::new(),
                }
            }
        };
        let count = ids.len() as u64;
        if !ids.is_empty() {
            crate::signals::emit_bulk_post_save::<T>(ids, true).await;
        }
        Ok(count)
    }

    /// Django's `get_or_create`: fetch the first row matching `predicate`;
    /// if none exists, insert `defaults` and return it. Returns
    /// `(row, created)` so the caller can branch on whether the write
    /// happened. Two queries on the miss path (filter+first then create),
    /// one query on the hit path.
    ///
    /// Race condition: a concurrent inserter can win between the two
    /// calls. The DB's UNIQUE constraint on the `predicate` columns is
    /// the backstop; without one, two callers can both create rows and
    /// the second's `create` won't see the first. Pair with a UNIQUE
    /// constraint for true at-most-one semantics.
    pub async fn get_or_create(
        &self,
        predicate: Predicate<T>,
        defaults: T,
    ) -> Result<(T, bool), crate::orm::write::WriteError>
    where
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        if let Some(existing) = self
            .filter(predicate)
            .first()
            .await
            .map_err(crate::orm::write::WriteError::Sqlx)?
        {
            return Ok((existing, false));
        }
        let created = self.create(defaults).await?;
        Ok((created, true))
    }

    /// Django's `update_or_create`: fetch the first row matching
    /// `predicate`; if found, update its non-PK columns with the
    /// `defaults` instance's values and return the fresh row;
    /// otherwise insert `defaults` and return it. Returns
    /// `(row, created)` so the caller can branch on the path taken.
    ///
    /// The defaults' PK is intentionally ignored on the update path —
    /// the matched row keeps its original PK. On the insert path the
    /// defaults' PK is honoured (autoincrement sentinel `0` → DB
    /// assigns; explicit value → DB uses it).
    ///
    /// Race condition mirrors `get_or_create`: a concurrent writer
    /// can win between the SELECT and the UPDATE / INSERT. Pair the
    /// match columns with a UNIQUE constraint for true at-most-one
    /// semantics.
    ///
    /// Implementation: 2 queries on the hit path (`first` + `save`),
    /// 2 queries on the miss path (`first` + `create`).
    pub async fn update_or_create(
        &self,
        predicate: Predicate<T>,
        defaults: T,
    ) -> Result<(T, bool), crate::orm::write::WriteError>
    where
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        use crate::orm::write::WriteError;
        let pk = pk_field::<T>().ok_or_else(|| {
            WriteError::Sqlx(sqlx::Error::Protocol(
                "update_or_create: model has no primary key".to_string(),
            ))
        })?;
        let pk_name = pk.name;

        if let Some(existing) = self
            .filter(predicate)
            .first()
            .await
            .map_err(WriteError::Sqlx)?
        {
            // Serialize defaults to a JSON map, drop the PK so the
            // existing row's PK is preserved, then UPDATE WHERE
            // <pk_col> = <existing_pk>. Re-fetch to return the
            // populated row.
            let mut update_map = serialize_to_map(&defaults)?;
            update_map.remove(pk_name);
            // Build a PK predicate from the existing row's serialized
            // PK value. Goes through serde_json so any built-in PK
            // type (i64, String, Uuid) round-trips through sea-query.
            let existing_json =
                serde_json::to_value(&existing).map_err(WriteError::SerializeFailed)?;
            let pk_value_json = existing_json
                .get(pk_name)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let pk_sea =
                crate::orm::write::json_to_sea_value(pk.ty, &pk_value_json, false, pk_name)?;
            let pk_pred: Predicate<T> =
                Predicate::new(sea_query::Expr::col(sea_query::Alias::new(pk_name)).eq(pk_sea));
            // Run the UPDATE.
            self.filter(pk_pred).update_values(update_map).await?;
            // Re-fetch to return the populated row. The PK predicate
            // is rebuilt because Predicate isn't Clone and the prior
            // one was moved into update_values.
            let pk_value_json2 = pk_value_json.clone();
            let pk_sea2 =
                crate::orm::write::json_to_sea_value(pk.ty, &pk_value_json2, false, pk_name)?;
            let refetch_pred: Predicate<T> =
                Predicate::new(sea_query::Expr::col(sea_query::Alias::new(pk_name)).eq(pk_sea2));
            let updated = self
                .filter(refetch_pred)
                .first()
                .await
                .map_err(WriteError::Sqlx)?
                .ok_or_else(|| {
                    WriteError::Sqlx(sqlx::Error::Protocol(
                        "update_or_create: row vanished between UPDATE and re-fetch".to_string(),
                    ))
                })?;
            return Ok((updated, false));
        }

        let created = self.create(defaults).await?;
        Ok((created, true))
    }

    /// INSERT-or-UPDATE keyed on the primary key. The row's PK column
    /// is the conflict target; on a hit, every non-PK column is
    /// overwritten with the supplied value. Returns the row as the DB
    /// stored it (post-upsert).
    ///
    /// Both backends use `INSERT ... ON CONFLICT(<pk>) DO UPDATE SET
    /// col = excluded.col, ...`. The SQLite and Postgres syntax happens
    /// to match exactly here so a single sea-query `OnConflict` builder
    /// covers both.
    pub async fn upsert(&self, instance: T) -> Result<T, crate::orm::write::WriteError>
    where
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let map = serialize_to_map(&instance)?;
        let pool = resolve_pool::<T>(None);
        let backend = pool.backend_name();
        let mut stmt = build_insert_one_for::<T>(backend, &map)?;

        // Conflict target = PK column. update_columns = every non-PK
        // column the body included. sea-query renders `DO UPDATE SET
        // col = excluded.col` (SQLite) / `DO UPDATE SET col =
        // EXCLUDED.col` (PG) — both forms work cross-dialect.
        let pk_name = T::FIELDS
            .iter()
            .find(|f| f.primary_key)
            .map(|f| f.name)
            .ok_or_else(|| {
                crate::orm::write::WriteError::Sqlx(sqlx::Error::Protocol(
                    "upsert: model has no primary key — use get_or_create or create instead"
                        .to_string(),
                ))
            })?;
        let update_cols: Vec<Alias> = T::FIELDS
            .iter()
            .filter(|f| !f.primary_key && map.contains_key(f.name))
            .map(|f| Alias::new(f.name))
            .collect();
        let mut on_conflict = sea_query::OnConflict::column(Alias::new(pk_name));
        if !update_cols.is_empty() {
            on_conflict.update_columns(update_cols);
        } else {
            // No non-PK columns to overwrite — this is a "INSERT OR
            // IGNORE" shape. sea-query encodes that as `DO NOTHING`.
            on_conflict.do_nothing();
        }
        stmt.on_conflict(on_conflict);

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

    /// Apply per-row differing values to a list of instances in one
    /// statement. Each instance carries its own PK and its own
    /// column values; the generated SQL uses one `CASE pk WHEN ...
    /// THEN ... END` per non-PK column, plus a `WHERE pk IN (...)`
    /// to scope the update.
    ///
    /// Mirrors Django's `QuerySet.bulk_update(objs, fields)` but
    /// updates every non-PK column rather than asking the caller to
    /// list them. Returns the number of rows affected.
    ///
    /// Empty input is a no-op (returns 0). The pattern works on both
    /// SQLite and Postgres — the `CASE` expression is SQL-standard.
    ///
    /// Limitations:
    /// - All instances must have a non-default PK (the caller has
    ///   already loaded the rows). Default-PK instances are skipped.
    /// - Bulk-write signals are NOT fired by this path — it's the
    ///   `Manager::bulk_create` analogue for UPDATE, deliberately
    ///   silent for speed.
    pub async fn bulk_update(&self, instances: Vec<T>) -> Result<u64, crate::orm::write::WriteError>
    where
        T: serde::Serialize,
    {
        use crate::orm::write::{WriteError, is_default_pk, json_to_sea_value};
        if instances.is_empty() {
            return Ok(0);
        }
        let pk = pk_field::<T>().ok_or_else(|| {
            WriteError::Sqlx(sqlx::Error::Protocol(
                "bulk_update: model has no primary key".to_string(),
            ))
        })?;
        let pk_name = pk.name;
        let pk_ty = pk.ty;

        // Serialize every instance, collecting (pk_value, full_map)
        // for the CASE branches and the IN clause. Skip rows whose
        // PK is still the default sentinel — they were never
        // persisted and a bulk UPDATE on them is a no-op anyway.
        let mut serialized: Vec<(
            serde_json::Value,
            serde_json::Map<String, serde_json::Value>,
        )> = Vec::with_capacity(instances.len());
        for instance in &instances {
            let map = serialize_to_map(instance)?;
            let pk_val = map.get(pk_name).cloned().unwrap_or(serde_json::Value::Null);
            if is_default_pk(pk_ty, &pk_val) {
                continue;
            }
            serialized.push((pk_val, map));
        }
        if serialized.is_empty() {
            return Ok(0);
        }

        // Collect the list of non-PK columns to update from the
        // first row (every row contributes the same column set
        // because they're typed instances of T).
        let update_cols: Vec<&crate::orm::FieldSpec> =
            T::FIELDS.iter().filter(|f| !f.primary_key).collect();

        // Build the UPDATE: one CASE per column, IN clause for the
        // WHERE. Goes through sea-query's update statement for
        // backend portability.
        let mut stmt = sea_query::Query::update();
        stmt.table(Alias::new(T::TABLE));

        for field in &update_cols {
            // CASE pk_col
            //   WHEN <pk1> THEN <val1>
            //   WHEN <pk2> THEN <val2>
            //   ...
            // END
            let mut case = sea_query::CaseStatement::new();
            for (pk_val, map) in &serialized {
                let val = map
                    .get(field.name)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let cell = json_to_sea_value(field.ty, &val, field.nullable, field.name)?;
                let pk_sea = json_to_sea_value(pk_ty, pk_val, false, pk_name)?;
                case = case.case(sea_query::Expr::col(Alias::new(pk_name)).eq(pk_sea), cell);
            }
            stmt.value(Alias::new(field.name), case);
        }

        // WHERE pk IN (<pk1>, <pk2>, ...)
        let pk_seas: Vec<sea_query::Value> = serialized
            .iter()
            .map(|(pk_val, _)| json_to_sea_value(pk_ty, pk_val, false, pk_name))
            .collect::<Result<_, _>>()?;
        stmt.and_where(sea_query::Expr::col(Alias::new(pk_name)).is_in(pk_seas));

        let pool = resolve_pool::<T>(None);
        let affected = match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&pool)
                    .await
                    .map_err(WriteError::Sqlx)?
                    .rows_affected()
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&pool)
                    .await
                    .map_err(WriteError::Sqlx)?
                    .rows_affected()
            }
        };
        Ok(affected)
    }

    /// Run a hand-written SQL query and return typed `Vec<T>` rows.
    ///
    /// The escape hatch for queries the QuerySet builder can't (or
    /// shouldn't) model — CTEs, vendor-specific functions, ad-hoc
    /// reporting. Delegates to `sqlx::query_as` against the ambient
    /// pool and dispatches on backend, so user code stays portable.
    /// The string is sent verbatim; no parameter binding (use
    /// `Predicate` / the typed query path for parameterised
    /// queries). Inject input only after manual sanitisation.
    ///
    /// Skips the `select_related` / `prefetch_related` chain — those
    /// only apply to the typed QuerySet build path.
    pub async fn raw(&self, sql: &str) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let pool = resolve_pool::<T>(None);
        match pool {
            DbPool::Sqlite(pool) => {
                sqlx::query_as::<sqlx::Sqlite, T>(sql)
                    .fetch_all(&pool)
                    .await
            }
            DbPool::Postgres(pool) => {
                sqlx::query_as::<sqlx::Postgres, T>(sql)
                    .fetch_all(&pool)
                    .await
            }
        }
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

// =========================================================================
// QuerySetTx — a QuerySet bound to an open transaction
// =========================================================================

/// A `QuerySet` bound to an open transaction.
///
/// Obtained via [`QuerySet::on_tx`]. All terminals execute inside the
/// transaction so they commit or roll back as a unit with every other
/// operation in the same `umbra::db::transaction(...)` closure.
///
/// The struct borrows `&mut Transaction` so the borrow checker enforces
/// that only one `QuerySetTx` uses the transaction at a time, and that
/// the transaction stays alive for the duration of each terminal call.
pub struct QuerySetTx<'tx, T> {
    qs: QuerySet<T>,
    tx: &'tx mut crate::db::Transaction,
}

impl<'tx, T: Model> QuerySetTx<'tx, T> {
    // -----------------------------------------------------------------------
    // Read terminals
    // -----------------------------------------------------------------------

    /// SELECT all matching rows inside the transaction.
    pub async fn fetch(self) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        let q = self.qs.build_query_for(self.tx.backend_name());
        let mut rows = match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await?
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await?
            }
        };
        // BUG-16 step 2: wire each row's PK into its M2M slots so
        // junction-table accessors used inside the transaction see
        // the right parent.
        for r in &mut rows {
            r.set_m2m_parent_ids();
        }
        Ok(rows)
    }

    /// SELECT LIMIT 1 and return the first row, if any.
    pub async fn first(mut self) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        self.qs.query.limit(1);
        let q = self.qs.build_query_for(self.tx.backend_name());
        let mut row = match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_optional(&mut **tx)
                    .await?
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_optional(&mut **tx)
                    .await?
            }
        };
        if let Some(r) = row.as_mut() {
            r.set_m2m_parent_ids();
        }
        Ok(row)
    }

    /// SELECT COUNT(*) inside the transaction.
    pub async fn count(self) -> Result<i64, sqlx::Error> {
        let backend = self.tx.backend_name();
        let mut rebuilt = self.qs.build_query_for(backend);
        rebuilt.clear_selects();
        rebuilt.expr(Func::count(Expr::col(Alias::new("*"))));
        rebuilt.reset_limit();
        rebuilt.reset_offset();
        match backend {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = rebuilt.build_sqlx(SqliteQueryBuilder);
                let (n,): (i64,) = sqlx::query_as_with::<sqlx::Sqlite, (i64,), _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await?;
                Ok(n)
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = rebuilt.build_sqlx(PostgresQueryBuilder);
                let (n,): (i64,) = sqlx::query_as_with::<sqlx::Postgres, (i64,), _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await?;
                Ok(n)
            }
        }
    }

    /// Return whether any row matches, inside the transaction.
    pub async fn exists(mut self) -> Result<bool, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.qs.query.limit(1);
        let backend = self.tx.backend_name();
        let q = self.qs.build_query_for(backend);
        let row_opt: Option<T> = match backend {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_optional(&mut **tx)
                    .await?
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_optional(&mut **tx)
                    .await?
            }
        };
        Ok(row_opt.is_some())
    }

    /// Exactly-one terminal inside the transaction. See [`QuerySet::get`].
    pub async fn get(mut self) -> Result<T, GetError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.qs.query.limit(2);
        let q = self.qs.build_query_for(self.tx.backend_name());
        let mut rows: Vec<T> = match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(GetError::Sqlx)?
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(GetError::Sqlx)?
            }
        };
        match rows.len() {
            0 => Err(GetError::NotFound),
            1 => Ok(rows.pop().unwrap()),
            _ => Err(GetError::MultipleObjectsReturned),
        }
    }

    // -----------------------------------------------------------------------
    // Write terminals
    // -----------------------------------------------------------------------

    /// DELETE inside the transaction. Returns the number of rows deleted.
    pub async fn delete(self) -> Result<u64, sqlx::Error> {
        let stmt = self.qs.build_delete_for(self.tx.backend_name());
        match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let result = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&mut **tx)
                    .await?;
                Ok(result.rows_affected())
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let result = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&mut **tx)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// UPDATE inside the transaction. Takes the same `column → JSON value`
    /// map as [`QuerySet::update_values`].
    pub async fn update_values(
        self,
        values: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, crate::orm::write::WriteError> {
        let stmt = self.qs.build_update_for(self.tx.backend_name(), &values)?;
        match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let result = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&mut **tx)
                    .await?;
                Ok(result.rows_affected())
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let result = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&mut **tx)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// INSERT one row and return the populated row, inside the transaction.
    ///
    /// This is the `Manager::create_in_tx` equivalent called through the
    /// QuerySet API: `Post::objects().on_tx(tx).create(instance).await?`.
    pub async fn create(self, instance: T) -> Result<T, crate::orm::write::WriteError>
    where
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        let map = serialize_to_map(&instance)?;
        let stmt = build_insert_one_for::<T>(self.tx.backend_name(), &map)?;
        match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let mut row = sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await?;
                row.set_m2m_parent_ids();
                Ok(row)
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let mut row = sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await?;
                row.set_m2m_parent_ids();
                Ok(row)
            }
        }
    }
}

impl<T: Model> Manager<T> {
    /// Begin a new query on this manager attached to the given open transaction.
    ///
    /// Sugar for `T::objects().on_tx(tx)` — lets callers skip the intermediate
    /// `QuerySet` construction when they want to go straight to a terminal:
    ///
    /// ```rust,ignore
    /// umbra::db::transaction(|tx| async move {
    ///     let post = Post::objects().on_tx(tx).create(new_post).await?;
    ///     Ok::<_, MyError>(post)
    /// }).await?;
    /// ```
    pub fn on_tx<'a>(&self, tx: &'a mut crate::db::Transaction) -> QuerySetTx<'a, T> {
        self.queryset().on_tx(tx)
    }

    /// INSERT one row inside `tx` and return the populated row.
    ///
    /// This is the primary Manager-level entry point for transactional writes.
    /// Equivalent to `Post::objects().on_tx(tx).create(instance)` but more
    /// ergonomic when you only need the one INSERT (no filter chain needed).
    ///
    /// ```rust,ignore
    /// umbra::db::transaction(|tx| async move {
    ///     let post = Post::objects().create_in_tx(new_post, tx).await?;
    ///     Ok::<_, MyError>(post)
    /// }).await?;
    /// ```
    pub async fn create_in_tx(
        &self,
        instance: T,
        tx: &mut crate::db::Transaction,
    ) -> Result<T, crate::orm::write::WriteError>
    where
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        let map = serialize_to_map(&instance)?;
        let stmt = build_insert_one_for::<T>(tx.backend_name(), &map)?;
        match tx.backend_name() {
            "sqlite" => {
                let inner = tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let mut row = sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_one(&mut **inner)
                    .await?;
                row.set_m2m_parent_ids();
                Ok(row)
            }
            _ => {
                let inner = tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let mut row = sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_one(&mut **inner)
                    .await?;
                row.set_m2m_parent_ids();
                Ok(row)
            }
        }
    }

    /// INSERT many rows inside `tx`.
    ///
    /// Returns the number of rows inserted. Empty input is a no-op.
    pub async fn bulk_create_in_tx(
        &self,
        instances: Vec<T>,
        tx: &mut crate::db::Transaction,
    ) -> Result<u64, crate::orm::write::WriteError>
    where
        T: serde::Serialize,
    {
        if instances.is_empty() {
            return Ok(0);
        }
        let maps: Result<Vec<_>, _> = instances.iter().map(serialize_to_map).collect();
        let maps = maps?;
        let stmt = build_insert_many_for::<T>(tx.backend_name(), &maps)?;
        match tx.backend_name() {
            "sqlite" => {
                let inner = tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let result = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&mut **inner)
                    .await?;
                Ok(result.rows_affected())
            }
            _ => {
                let inner = tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let result = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&mut **inner)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    // =====================================================================
    // Per-instance signal-firing write methods.
    //
    // `save(instance)` and `delete_instance(instance)` are the methods that
    // fire the ORM lifecycle signals (`pre_save` / `post_save` /
    // `pre_delete` / `post_delete`). The existing bulk methods
    // (`create`, `bulk_create`, `QuerySet::update_values`,
    // `QuerySet::delete`) remain signal-free, matching Django's own
    // behaviour: bulk operations bypass signals for performance.
    //
    // Signal name format: `<event>:<table>` — e.g. `post_save:post`.
    // Payload shapes:
    //   save:   `{ "instance": <M as JSON>, "created": bool }`
    //   delete: `{ "instance": <M as JSON> }`
    //
    // The `created` flag on save follows Django's convention:
    //   `true`  when the PK is the default sentinel → INSERT path.
    //   `false` when the PK is non-default           → UPDATE path.
    // =====================================================================

    /// Save one instance, firing `pre_save` + `post_save` signals.
    ///
    /// Determines INSERT vs UPDATE by checking whether the primary key
    /// is the autoincrement sentinel (`0` for integers, nil UUID, empty
    /// string). If it is, an INSERT is performed (`created = true`);
    /// otherwise an `UPDATE ... WHERE pk = <value>` is run (`created = false`).
    ///
    /// Returns the row as it exists in the database after the write
    /// (populated PK for inserts, same row for updates).
    ///
    /// ## Signal contract
    ///
    /// - `pre_save:<table>` fires before the database write with
    ///   `{ "instance": ..., "created": bool, "actor": ... }`.
    /// - `post_save:<table>` fires after the database write with the
    ///   DB-read-back row and the same envelope keys.
    ///
    /// The `"actor"` value is set by the nearest enclosing
    /// [`crate::signals::with_actor`] scope; `Value::Null` when no
    /// scope is active.
    ///
    /// ## Bulk paths fire bulk signals, not per-row signals
    ///
    /// `Manager::create`, `Manager::bulk_create`, and
    /// `QuerySet::update_values` / `QuerySet::delete` do NOT fire
    /// per-row `post_save` / `post_delete`. They fire
    /// `bulk_post_save:<table>` / `bulk_post_delete:<table>` once per
    /// statement with the affected PKs in the payload. Use `save` /
    /// `delete_instance` when per-row signal semantics are needed.
    pub async fn save(&self, instance: T) -> Result<T, crate::orm::write::SaveError>
    where
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        use crate::orm::write::{SaveError, is_default_pk};
        // Determine INSERT vs UPDATE by inspecting the PK field.
        let pk_field = T::FIELDS
            .iter()
            .find(|f| f.primary_key)
            .ok_or(SaveError::NoPrimaryKey)?;
        let map = serialize_to_map(&instance).map_err(SaveError::Write)?;
        let pk_val = map
            .get(pk_field.name)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let created = is_default_pk(pk_field.ty, &pk_val);

        // Fire pre_save before the write.
        crate::signals::emit_pre_save::<T>(&instance, created).await;

        let pool = resolve_pool::<T>(None);
        let backend = pool.backend_name();

        if created {
            // INSERT path.
            let stmt = build_insert_one_for::<T>(backend, &map).map_err(SaveError::Write)?;
            let row = match pool {
                DbPool::Sqlite(pool) => {
                    let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                    sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                        .fetch_one(&pool)
                        .await
                        .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                }
                DbPool::Postgres(pool) => {
                    let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                    sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                        .fetch_one(&pool)
                        .await
                        .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                }
            };
            // Fire post_save with the DB-populated row.
            crate::signals::emit_post_save::<T>(&row, true).await;
            Ok(row)
        } else {
            // UPDATE path: UPDATE ... WHERE <pk> = <value> RETURNING *.
            use sea_query::{Alias, Expr, Query};
            let mut stmt = Query::update();
            stmt.table(Alias::new(T::TABLE));
            for field in T::FIELDS {
                if field.primary_key {
                    continue;
                }
                let val = map
                    .get(field.name)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let sea_val = crate::orm::write::json_to_sea_value(
                    field.ty,
                    &val,
                    field.nullable,
                    field.name,
                )
                .map_err(SaveError::Write)?;
                stmt.value(Alias::new(field.name), sea_val);
            }
            // WHERE pk = <value>
            let pk_sea =
                crate::orm::write::json_to_sea_value(pk_field.ty, &pk_val, false, pk_field.name)
                    .map_err(SaveError::Write)?;
            stmt.and_where(Expr::col(Alias::new(pk_field.name)).eq(pk_sea));
            // RETURNING * so we can return the updated row.
            stmt.returning_all();

            let row = match pool {
                DbPool::Sqlite(pool) => {
                    let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                    sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                        .fetch_one(&pool)
                        .await
                        .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                }
                DbPool::Postgres(pool) => {
                    let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                    sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                        .fetch_one(&pool)
                        .await
                        .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                }
            };
            // Fire post_save with created=false.
            crate::signals::emit_post_save::<T>(&row, false).await;
            Ok(row)
        }
    }

    /// Delete one instance by primary key, firing `pre_delete` +
    /// `post_delete` signals.
    ///
    /// Issues `DELETE FROM <table> WHERE <pk> = <value>`. Returns the
    /// number of rows affected (0 if the row was already gone, 1 otherwise).
    ///
    /// ## Signal contract
    ///
    /// - `pre_delete:<table>` fires before the DELETE with
    ///   `{ "instance": ..., "actor": ... }`.
    /// - `post_delete:<table>` fires after the DELETE with the same
    ///   payload shape.
    ///
    /// The `"actor"` value is set by the nearest enclosing
    /// [`crate::signals::with_actor`] scope; `Value::Null` when no
    /// scope is active. The instance value passed to both signals is
    /// the value supplied by the caller — not a DB read-back. If you
    /// need the freshest DB state before deletion, fetch it first with
    /// `.get(...)` then pass to this method.
    ///
    /// ## Bulk paths fire bulk signals
    ///
    /// `QuerySet::delete()` (the filter-chain DELETE) fires
    /// `bulk_post_delete:<table>` with the list of affected PKs, not
    /// per-row `pre_delete` / `post_delete`. Use `delete_instance` for
    /// per-row signal semantics.
    pub async fn delete_instance(&self, instance: &T) -> Result<u64, crate::orm::write::SaveError>
    where
        T: serde::Serialize,
    {
        use crate::orm::write::SaveError;
        let pk_field = T::FIELDS
            .iter()
            .find(|f| f.primary_key)
            .ok_or(SaveError::NoPrimaryKey)?;
        let map = serialize_to_map(instance).map_err(SaveError::Write)?;
        let pk_val = map
            .get(pk_field.name)
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        // Fire pre_delete before the write.
        crate::signals::emit_pre_delete::<T>(instance).await;

        let pk_sea =
            crate::orm::write::json_to_sea_value(pk_field.ty, &pk_val, false, pk_field.name)
                .map_err(SaveError::Write)?;

        use sea_query::{Alias, Expr, Query};
        let mut stmt = Query::delete();
        stmt.from_table(Alias::new(T::TABLE));
        stmt.and_where(Expr::col(Alias::new(pk_field.name)).eq(pk_sea));

        let pool = resolve_pool::<T>(None);
        let affected = match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&pool)
                    .await
                    .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                    .rows_affected()
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&pool)
                    .await
                    .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                    .rows_affected()
            }
        };

        // Fire post_delete after the write.
        crate::signals::emit_post_delete::<T>(instance).await;

        Ok(affected)
    }
}

// =========================================================================
// select_related hydration
//
// After the main query returns rows, for each FK field name in
// `select_related`:
//
// 1. Look up the field's `fk_target` table name from `T::FIELDS`.
// 2. Serialize all main rows to JSON and collect the FK integer values for
//    that field (using the field name as a JSON key).
// 3. Run `SELECT <cols> FROM <target_table> WHERE id IN (...)` to load all
//    referenced rows in one batch.
// 4. Build a `HashMap<i64, JsonValue>` from the fetched rows.
// 5. Call `HydrateRelated::hydrate_fk` on each main row with the matching
//    resolved JSON object.
//
// This approach requires no JOIN changes to the main query and no macro
// changes to `FromRow`. The cost is one extra round-trip per FK field
// named in `select_related` (not one per row).
// =========================================================================

/// Fetch related rows for each FK field name in `sr_fields` and hydrate
/// `HydrateRelated::hydrate_fk` on each main row.
///
/// Generic parameters:
/// - `T`: the main model type. Bound on `HydrateRelated` so we can call
///   `fk_id_for` and `hydrate_fk` on each row.
async fn hydrate_select_related<T: Model + HydrateRelated>(
    rows: &mut [T],
    sr_fields: &[String],
    pool: &DbPool,
) -> Result<(), sqlx::Error> {
    for field_name in sr_fields {
        // Look up the FK field spec to get the target table name.
        let field_spec = match T::FIELDS.iter().find(|f| f.name == field_name.as_str()) {
            Some(f) => f,
            None => continue, // Unknown field — skip silently.
        };
        let fk_target = match field_spec.fk_target {
            Some(t) => t,
            None => continue, // Not a FK field — skip silently.
        };

        // Collect all FK IDs from the main rows via `HydrateRelated::fk_id_for`.
        // This avoids serializing the whole row just to read one integer.
        let mut ids: Vec<i64> = Vec::with_capacity(rows.len());
        for row in rows.iter() {
            if let Some(id) = row.fk_id_for(field_name.as_str()) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            continue;
        }
        ids.sort_unstable();
        ids.dedup();

        // Build `SELECT * FROM <target_table> WHERE id IN (...)`.
        // We use sqlx raw queries here so we can read the rows as JSON
        // maps via the backup-style column-by-column extraction.
        let related_rows = fetch_related_as_json(fk_target, &ids, pool).await?;

        // Build id → JSON map.
        let id_to_json: HashMap<i64, JsonValue> = related_rows
            .into_iter()
            .filter_map(|obj| {
                if let JsonValue::Object(ref map) = obj {
                    if let Some(JsonValue::Number(n)) = map.get("id") {
                        if let Some(id) = n.as_i64() {
                            return Some((id, obj.clone()));
                        }
                    }
                }
                None
            })
            .collect();

        // Hydrate each main row.
        for row in rows.iter_mut() {
            if let Some(fk_id) = row.fk_id_for(field_name.as_str()) {
                if let Some(resolved_json) = id_to_json.get(&fk_id) {
                    row.hydrate_fk(field_name.as_str(), resolved_json);
                }
            }
        }
    }
    Ok(())
}

/// Gap 19: post-fetch hydration for `prefetch_related` names.
///
/// For each requested M2M field, runs one query joining the child
/// table to the junction:
///
///   SELECT j.parent_id AS __parent_id, child.<col1>, child.<col2>, ...
///   FROM <child_table> child
///   INNER JOIN <junction> j ON child.<child_pk> = j.child_id
///   WHERE j.parent_id IN (<parent_ids>)
///
/// Each result row decodes its child columns to a `serde_json::Value`
/// object (using the child ModelMeta's column types — same machinery
/// as `values()`). Rows are bucketed by parent_id; each parent in
/// `rows` then receives the matching bucket via
/// `HydrateRelated::set_m2m_resolved_json`.
///
/// V1 scope: i64 parent PK only (parents whose `pk_i64()` returns
/// `None` are skipped). Unknown field names, models with no
/// matching `M2M_RELATIONS` entry, and child models that aren't
/// registered are silently no-op'd — matches the forgiving posture
/// of `hydrate_select_related`.
async fn hydrate_prefetch_related<T: Model + HydrateRelated>(
    rows: &mut [T],
    prefetch_fields: &[String],
    pool: &DbPool,
) -> Result<(), sqlx::Error> {
    for field_name in prefetch_fields {
        // 1. Locate the M2M relation spec on T for this field name.
        let spec = match T::M2M_RELATIONS
            .iter()
            .find(|s| s.field_name == field_name.as_str())
        {
            Some(s) => s,
            None => continue,
        };
        let junction_table = format!("{}_{}", T::TABLE, spec.field_name);

        // 2. Look up the child model's ModelMeta via the migrate
        //    registry so we can iterate its columns at decode time.
        let registered: Vec<crate::migrate::ModelMeta> = crate::migrate::registered_models();
        let child_meta = match registered
            .into_iter()
            .find(|m| m.table == spec.target_table)
        {
            Some(m) => m,
            None => continue,
        };
        let child_pk_col = match child_meta.fields.iter().find(|c| c.primary_key) {
            Some(c) => c.name.clone(),
            None => continue,
        };

        // 3. Collect parent PKs (i64 only) from the main rows.
        let mut parent_ids: Vec<i64> = Vec::with_capacity(rows.len());
        for row in rows.iter() {
            if let Some(pk) = row.pk_i64() {
                parent_ids.push(pk);
            }
        }
        if parent_ids.is_empty() {
            // Still need to set empty resolved on every parent so
            // `tags.resolved()` returns `Some(&[])` after prefetch,
            // matching the documented "empty Vec, not None" contract.
            for r in rows.iter_mut() {
                r.set_m2m_resolved_json(field_name.as_str(), Vec::new());
            }
            continue;
        }

        // 4. Build the SELECT joining child + junction.
        let mut q = sea_query::Query::select();
        q.expr_as(
            sea_query::Expr::col((
                sea_query::Alias::new("j"),
                sea_query::Alias::new("parent_id"),
            )),
            sea_query::Alias::new("__parent_id"),
        );
        for col in &child_meta.fields {
            q.expr_as(
                sea_query::Expr::col((
                    sea_query::Alias::new("c"),
                    sea_query::Alias::new(col.name.as_str()),
                )),
                sea_query::Alias::new(col.name.as_str()),
            );
        }
        q.from_as(
            sea_query::Alias::new(child_meta.table.as_str()),
            sea_query::Alias::new("c"),
        )
        .join_as(
            sea_query::JoinType::InnerJoin,
            sea_query::Alias::new(&junction_table),
            sea_query::Alias::new("j"),
            sea_query::Expr::col((
                sea_query::Alias::new("j"),
                sea_query::Alias::new("child_id"),
            ))
            .equals((
                sea_query::Alias::new("c"),
                sea_query::Alias::new(child_pk_col.as_str()),
            )),
        )
        .and_where(
            sea_query::Expr::col((
                sea_query::Alias::new("j"),
                sea_query::Alias::new("parent_id"),
            ))
            .is_in(parent_ids.iter().copied()),
        );

        // 5. Execute and group by parent_id.
        let mut buckets: HashMap<i64, Vec<JsonValue>> = HashMap::new();
        match pool {
            DbPool::Sqlite(p) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let raw_rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, vals)
                    .fetch_all(p)
                    .await?;
                for raw in &raw_rows {
                    use sqlx::Row;
                    let parent_id: i64 = raw.try_get("__parent_id")?;
                    let mut obj = serde_json::Map::with_capacity(child_meta.fields.len());
                    for col in &child_meta.fields {
                        let v = crate::orm::dynamic::decode_to_json(raw, col)?;
                        obj.insert(col.name.clone(), v);
                    }
                    buckets
                        .entry(parent_id)
                        .or_default()
                        .push(JsonValue::Object(obj));
                }
            }
            DbPool::Postgres(p) => {
                let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                let raw_rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, vals)
                    .fetch_all(p)
                    .await?;
                for raw in &raw_rows {
                    use sqlx::Row;
                    let parent_id: i64 = raw.try_get("__parent_id")?;
                    let mut obj = serde_json::Map::with_capacity(child_meta.fields.len());
                    for col in &child_meta.fields {
                        let v = crate::orm::dynamic::decode_pg_to_json(raw, col)?;
                        obj.insert(col.name.clone(), v);
                    }
                    buckets
                        .entry(parent_id)
                        .or_default()
                        .push(JsonValue::Object(obj));
                }
            }
        }

        // 6. Hand each parent its bucket. Parents without children
        //    still get an empty Vec so .resolved() returns Some(&[])
        //    consistently after prefetch.
        for row in rows.iter_mut() {
            let bucket = match row.pk_i64() {
                Some(id) => buckets.remove(&id).unwrap_or_default(),
                None => Vec::new(),
            };
            row.set_m2m_resolved_json(field_name.as_str(), bucket);
        }
    }
    Ok(())
}

/// Fetch rows from `table` where `id IN ids` and return them as a `Vec` of
/// `serde_json::Value::Object`. Uses the backup-style column-walk approach to
/// avoid needing a `FromRow` bound on the target model type.
async fn fetch_related_as_json(
    table: &str,
    ids: &[i64],
    pool: &DbPool,
) -> Result<Vec<JsonValue>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    // Build a raw SQL query: SELECT * FROM <table> WHERE id IN (?, ?, ...)
    // using positional placeholders appropriate for the backend.
    match pool {
        DbPool::Sqlite(pool) => {
            let placeholders: Vec<String> = (0..ids.len()).map(|_| "?".to_string()).collect();
            let sql = format!(
                "SELECT * FROM \"{}\" WHERE id IN ({})",
                table.replace('"', "\"\""),
                placeholders.join(", ")
            );
            let mut query = sqlx::query(&sql);
            for id in ids {
                query = query.bind(*id);
            }
            let rows = query.fetch_all(pool).await?;
            let result = rows.iter().map(sqlite_row_to_json).collect::<Vec<_>>();
            Ok(result)
        }
        DbPool::Postgres(pool) => {
            let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("${i}")).collect();
            let sql = format!(
                "SELECT * FROM \"{}\" WHERE id IN ({})",
                table.replace('"', "\"\""),
                placeholders.join(", ")
            );
            let mut query = sqlx::query(&sql);
            for id in ids {
                query = query.bind(*id);
            }
            let rows = query.fetch_all(pool).await?;
            let result = rows.iter().map(postgres_row_to_json).collect::<Vec<_>>();
            Ok(result)
        }
    }
}

/// Convert a SQLite row to a `serde_json::Value::Object`. Reads every column
/// by index and maps the SQLite type to the closest JSON primitive.
fn sqlite_row_to_json(row: &sqlx::sqlite::SqliteRow) -> JsonValue {
    use sqlx::Row;
    let mut map = serde_json::Map::new();
    let cols = row.columns();
    for col in cols {
        let name = col.name().to_string();
        // Try the types from most to least specific.
        let val: JsonValue = if let Ok(v) = row.try_get::<i64, _>(col.ordinal()) {
            JsonValue::Number(v.into())
        } else if let Ok(v) = row.try_get::<f64, _>(col.ordinal()) {
            serde_json::json!(v)
        } else if let Ok(v) = row.try_get::<bool, _>(col.ordinal()) {
            JsonValue::Bool(v)
        } else if let Ok(v) = row.try_get::<String, _>(col.ordinal()) {
            JsonValue::String(v)
        } else {
            JsonValue::Null
        };
        map.insert(name, val);
    }
    JsonValue::Object(map)
}

/// Convert a Postgres row to a `serde_json::Value::Object`.
fn postgres_row_to_json(row: &sqlx::postgres::PgRow) -> JsonValue {
    use sqlx::Row;
    let mut map = serde_json::Map::new();
    let cols = row.columns();
    for col in cols {
        let name = col.name().to_string();
        let val: JsonValue = if let Ok(v) = row.try_get::<i64, _>(col.ordinal()) {
            JsonValue::Number(v.into())
        } else if let Ok(v) = row.try_get::<f64, _>(col.ordinal()) {
            serde_json::json!(v)
        } else if let Ok(v) = row.try_get::<bool, _>(col.ordinal()) {
            JsonValue::Bool(v)
        } else if let Ok(v) = row.try_get::<String, _>(col.ordinal()) {
            JsonValue::String(v)
        } else {
            JsonValue::Null
        };
        map.insert(name, val);
    }
    JsonValue::Object(map)
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

// =========================================================================
// Primary-key decoding for bulk signal payloads
//
// Bulk write terminals (bulk_create, update_values, update_expr, delete)
// add `RETURNING <pk>` to their statements when at least one subscriber
// is registered for the corresponding bulk signal. The returned rows
// then go through one of these helpers to land as serde_json::Value in
// the signal payload's `ids` array.
//
// The dispatch is keyed on the FieldSpec's SqlType. Anything beyond the
// built-in PK types (i32 / i64 / String / Uuid / FK) lands as Value::Null
// — the signal still fires, just without a usable id for that row. In
// practice the PK type is one of the four built-ins.
// =========================================================================

fn pk_to_json_sqlite(
    row: &sqlx::sqlite::SqliteRow,
    col_name: &str,
    ty: crate::orm::SqlType,
) -> Result<JsonValue, sqlx::Error> {
    use crate::orm::SqlType::*;
    use serde_json::json;
    use sqlx::Row;
    Ok(match ty {
        SmallInt | Integer | BigInt | ForeignKey => json!(row.try_get::<i64, _>(col_name)?),
        Text => json!(row.try_get::<String, _>(col_name)?),
        Uuid => json!(row.try_get::<uuid::Uuid, _>(col_name)?.to_string()),
        _ => JsonValue::Null,
    })
}

fn pk_to_json_pg(
    row: &sqlx::postgres::PgRow,
    col_name: &str,
    ty: crate::orm::SqlType,
) -> Result<JsonValue, sqlx::Error> {
    use crate::orm::SqlType::*;
    use serde_json::json;
    use sqlx::Row;
    Ok(match ty {
        SmallInt => json!(row.try_get::<i16, _>(col_name)?),
        Integer => json!(row.try_get::<i32, _>(col_name)?),
        BigInt | ForeignKey => json!(row.try_get::<i64, _>(col_name)?),
        Text => json!(row.try_get::<String, _>(col_name)?),
        Uuid => json!(row.try_get::<uuid::Uuid, _>(col_name)?.to_string()),
        _ => JsonValue::Null,
    })
}

/// Locate the primary-key FieldSpec for a model. Returns `None` if the
/// model has no PK (pathological — every macro-generated Model has one).
fn pk_field<T: Model>() -> Option<&'static crate::orm::FieldSpec> {
    T::FIELDS.iter().find(|f| f.primary_key)
}

// =========================================================================
// Aggregate result decoding
//
// COUNT always returns BIGINT, AVG always returns DOUBLE — both backends
// agree on this. SUM/MAX/MIN inherit the source column's type, so the
// decoder dispatches on the FieldSpec's SqlType we collected at
// terminal-build time.
// =========================================================================

fn decode_agg_sqlite(
    row: &sqlx::sqlite::SqliteRow,
    name: &str,
    agg: &crate::orm::Aggregate,
    source_ty: Option<crate::orm::SqlType>,
) -> Result<JsonValue, sqlx::Error> {
    use crate::orm::SqlType::*;
    use crate::orm::aggregate::AggregateKind;
    use serde_json::json;
    use sqlx::Row;
    Ok(match agg.kind() {
        AggregateKind::Count => json!(row.try_get::<i64, _>(name)?),
        AggregateKind::Avg => row
            .try_get::<Option<f64>, _>(name)?
            .map_or(JsonValue::Null, |f| json!(f)),
        AggregateKind::Sum | AggregateKind::Max | AggregateKind::Min => match source_ty {
            Some(SmallInt | Integer | BigInt | ForeignKey) => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(JsonValue::Null, |n| json!(n)),
            Some(Real | Double) => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(JsonValue::Null, |f| json!(f)),
            // Default to a string read for date/time/text/uuid; SQLite
            // stores them as TEXT, so a MIN/MAX comes back stringified.
            _ => row
                .try_get::<Option<String>, _>(name)?
                .map_or(JsonValue::Null, JsonValue::String),
        },
    })
}

fn decode_agg_pg(
    row: &sqlx::postgres::PgRow,
    name: &str,
    agg: &crate::orm::Aggregate,
    source_ty: Option<crate::orm::SqlType>,
) -> Result<JsonValue, sqlx::Error> {
    use crate::orm::SqlType::*;
    use crate::orm::aggregate::AggregateKind;
    use serde_json::json;
    use sqlx::Row;
    Ok(match agg.kind() {
        AggregateKind::Count => json!(row.try_get::<i64, _>(name)?),
        AggregateKind::Avg => row
            .try_get::<Option<f64>, _>(name)?
            .map_or(JsonValue::Null, |f| json!(f)),
        AggregateKind::Sum | AggregateKind::Max | AggregateKind::Min => match source_ty {
            Some(SmallInt) => row
                .try_get::<Option<i16>, _>(name)?
                .map_or(JsonValue::Null, |n| json!(n)),
            Some(Integer) => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(JsonValue::Null, |n| json!(n)),
            Some(BigInt | ForeignKey) => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(JsonValue::Null, |n| json!(n)),
            Some(Real) => row
                .try_get::<Option<f32>, _>(name)?
                .map_or(JsonValue::Null, |f| json!(f as f64)),
            Some(Double) => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(JsonValue::Null, |f| json!(f)),
            _ => row
                .try_get::<Option<String>, _>(name)?
                .map_or(JsonValue::Null, JsonValue::String),
        },
    })
}
