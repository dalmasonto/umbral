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

mod backend_pg;
mod backend_sqlite;
mod errors;
pub(crate) mod hydration;
mod tx;
mod write_helpers;

pub use errors::{GetError, TryForEachError};
use hydration::{hydrate_prefetch_related, hydrate_select_related};
pub use tx::QuerySetTx;
use write_helpers::{
    build_insert_many_for, build_insert_one_for, fk_pk_hint, pk_field, serialize_to_map,
};

use std::collections::HashMap;
use std::marker::PhantomData;

use sea_query::{
    Alias, Expr, Func, IntoIden, Order, PostgresQueryBuilder, Query, SqliteQueryBuilder,
};
use sea_query_binder::SqlxBinder;
use serde_json::Value as JsonValue;

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
    /// Feature #72 — soft-delete state. Snapshotted from
    /// `T::SOFT_DELETE` when the QuerySet is constructed from a
    /// Manager (the no-bounds `QuerySet::new` constructor leaves
    /// this `false` so hand-built QuerySets stay opt-out by
    /// default).
    pub(crate) soft_delete_active: bool,
    /// True when the caller opted back into soft-deleted rows via
    /// `.with_deleted()`. Skips the auto `WHERE deleted_at IS NULL`
    /// injection.
    pub(crate) with_deleted: bool,
    /// True when the caller wants ONLY soft-deleted rows via
    /// `.only_deleted()`. Inverts the auto-filter to
    /// `WHERE deleted_at IS NOT NULL`.
    pub(crate) only_deleted: bool,
    /// True when the caller asked for a real DELETE via
    /// `.hard_delete()` — bypasses the soft-delete rewrite that
    /// would normally turn `delete()` into an UPDATE.
    pub(crate) hard_delete: bool,
    /// Gap #111 — column projection set by [`Self::only`]. When
    /// `Some`, [`Self::to_sql`] / [`Self::to_sql_pg`] swap the
    /// SELECT list for just these columns, and the typed terminals
    /// (`fetch` / `first` / `get`) refuse to run with a clear error
    /// pointing the caller at [`Self::values`] (FromRow can't
    /// hydrate `T` from a partial-column row). `None` keeps the
    /// pre-#111 behaviour (full SELECT).
    pub(crate) only_cols: Option<Vec<String>>,
    /// FK field names requested for JOIN-based prefetch via
    /// [`Self::join_related`]. Distinct from `select_related`:
    /// `join_related` weaves `LEFT JOIN <related_table>` into the
    /// main SELECT (with aliased columns `<field>__<col>`) so one
    /// round-trip pulls parent + related rows together. The existing
    /// `select_related` path keeps its "batched-IN-followup-query"
    /// shape — both are valid; this one wins when round-trip count
    /// matters more than the per-row column overhead.
    pub(crate) join_related: Vec<String>,
    /// Related-aggregate annotations added via
    /// [`Self::annotate_related`] / [`Self::annotate_count`]. Applied
    /// inside `build_query_for`, so EVERY terminal and introspection
    /// path — `fetch_annotated`, `explain`, `to_sql`, `to_sql_pg` —
    /// sees the same correlated subqueries. That's the Django
    /// `annotate()` contract: an annotation is query-builder state,
    /// not a side query.
    pub(crate) annotations: Vec<RelatedAnnotation>,
    _phantom: PhantomData<T>,
}

/// One related-aggregate annotation on a [`QuerySet`] —
/// `(alias, relation, aggregate)` resolved against the model's
/// `REVERSE_FK_RELATIONS` at builder time. A name that fails to
/// resolve is stored poisoned (`resolved: Err`) so the infallible
/// builder stays chainable while every fallible consumer
/// (`fetch_annotated`, `explain`) reports it loudly.
#[derive(Debug, Clone)]
pub(crate) struct RelatedAnnotation {
    pub(crate) alias: String,
    pub(crate) agg: crate::orm::Aggregate,
    /// `Ok((child_table, fk_column, parent_table, parent_pk))` or the
    /// loud error message for an unknown relation.
    pub(crate) resolved: Result<(String, String, String, String), String>,
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
            soft_delete_active: false,
            with_deleted: false,
            only_deleted: false,
            hard_delete: false,
            only_cols: None,
            join_related: Vec::new(),
            annotations: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Feature #72 — include soft-deleted rows in this query. Skips
    /// the auto `WHERE deleted_at IS NULL` injection. No-op on
    /// models that aren't tagged `#[umbra(soft_delete)]`.
    pub fn with_deleted(mut self) -> Self {
        self.with_deleted = true;
        self
    }

    /// Feature #72 — only soft-deleted rows. Useful for admin
    /// trash views and undelete workflows. No-op on models that
    /// aren't tagged `#[umbra(soft_delete)]`.
    pub fn only_deleted(mut self) -> Self {
        self.only_deleted = true;
        self
    }

    /// Feature #72 — force a real DELETE for the next `.delete()`
    /// terminal call. Soft-delete models normally rewrite delete()
    /// as `UPDATE ... SET deleted_at = NOW()`; `.hard_delete()`
    /// bypasses that for GDPR purges, test cleanup, or any other
    /// case where the row truly should be gone. No-op on models
    /// that aren't tagged `#[umbra(soft_delete)]` (their delete()
    /// is already a hard DELETE).
    pub fn hard_delete(mut self) -> Self {
        self.hard_delete = true;
        self
    }

    /// Gap #111 — restrict the SELECT to the named columns.
    ///
    /// Affects [`Self::to_sql`] / [`Self::to_sql_pg`] (the SELECT list
    /// shrinks to just these columns) and propagates into
    /// [`Self::values`] when that terminal is called without its own
    /// explicit column slice.
    ///
    /// **The typed terminals (`fetch` / `first` / `get`) refuse to
    /// run with `.only()` set** because a partial-column row can't
    /// satisfy `T`'s `FromRow` impl. The error message points at
    /// `.values(...)` (returns `Vec<serde_json::Value>`) as the
    /// execution path. Use `.only()` for `.to_sql()` inspection and
    /// `.values()` for actual reads.
    ///
    /// ```rust,ignore
    /// // Inspect: "SELECT \"id\", \"name\" FROM \"brand\" WHERE \"id\" = ?"
    /// let sql = Brand::objects()
    ///     .filter(brand::ID.eq(1))
    ///     .only(&["id", "name"])
    ///     .to_sql();
    ///
    /// // Execute (returns JSON rows):
    /// let rows = Brand::objects()
    ///     .filter(brand::ID.eq(1))
    ///     .values(&["id", "name"])
    ///     .await?;
    /// ```
    ///
    /// Unknown column names are not validated here — they surface at
    /// terminal time the same way `.values()` reports them (the
    /// rendered SQL contains the bad identifier and SQLite/Postgres
    /// raises). This keeps the chainable surface return-type-stable.
    pub fn only(mut self, cols: &[&str]) -> Self {
        self.only_cols = Some(cols.iter().map(|s| s.to_string()).collect());
        self
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
        // Feature #72 — soft-delete auto-filter. When the model
        // opted in via `#[umbra(soft_delete)]` AND the caller
        // didn't switch the visibility via `.with_deleted()` /
        // `.only_deleted()`, inject `WHERE deleted_at IS NULL`.
        // `.with_deleted()` shows everything; `.only_deleted()`
        // shows just the soft-deleted rows.
        if self.soft_delete_active {
            use sea_query::Expr;
            if self.only_deleted {
                q.and_where(Expr::col(Alias::new("deleted_at")).is_not_null());
            } else if !self.with_deleted {
                q.and_where(Expr::col(Alias::new("deleted_at")).is_null());
            }
        }
        // Related-aggregate annotations: one correlated scalar
        // subquery per entry, aliased onto the SELECT list. Living
        // HERE is what makes `.annotate_*` compose with everything —
        // explain(), to_sql(), fetch_annotated() all see the same
        // query. Poisoned entries (unknown relation) are skipped in
        // this infallible path; the fallible consumers call
        // `check_annotations()` first and fail loudly instead.
        for ann in &self.annotations {
            if let Ok((child_table, fk_col, parent_table, parent_pk)) = &ann.resolved {
                let sub = sea_query::Query::select()
                    .expr(ann.agg.to_simple_expr())
                    .from(Alias::new(child_table.as_str()))
                    .and_where(
                        sea_query::Expr::col((
                            Alias::new(child_table.as_str()),
                            Alias::new(fk_col.as_str()),
                        ))
                        .equals((
                            Alias::new(parent_table.as_str()),
                            Alias::new(parent_pk.as_str()),
                        )),
                    )
                    .to_owned();
                q.expr_as(
                    sea_query::SimpleExpr::SubQuery(
                        None,
                        Box::new(sea_query::SubQueryStatement::SelectStatement(sub)),
                    ),
                    Alias::new(ann.alias.as_str()),
                );
            }
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
    /// ## Nested traversal (post-#42)
    ///
    /// `.select_related("author__manager")` walks the FK chain through
    /// the `__` separator. One batched `IN (...)` query per hop —
    /// `1 + len(hops)` round-trips regardless of parent count. No
    /// N+1. Each hop's related row is embedded into the prior level's
    /// JSON, and recursive `ForeignKey<T>::Deserialize` unpacks the
    /// chain into `resolved()` slots at every depth. Bonus: a
    /// select_related'd model now round-trips through
    /// `serde_json::to_value(&t)` / `from_value` without losing the
    /// resolved relation.
    ///
    /// ## Companion shapes
    ///
    /// - **`join_related(name)`** — same goal (load related rows) via
    ///   a true `LEFT JOIN` in the main SELECT. One round-trip total
    ///   vs. `select_related`'s `1 + N` batched-IN approach. Wider
    ///   per-row payload; better when round-trip count dominates.
    /// - **`prefetch_related(name)`** — M2M batched loading (one
    ///   query per declared M2M field). For reverse-FK collections
    ///   (`prefetch_related("comment_set")`-style) see gap #44 — not
    ///   yet implemented.
    ///
    /// ## Loud errors
    ///
    /// Unknown field names (typos, M2M names accidentally passed
    /// here, fields without `fk_target`) return a clear
    /// `sqlx::Error::Protocol` from the terminal naming the bad hop
    /// and the table it was looked up against. Pre-#42 these
    /// silently no-op'd.
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

    /// JOIN-based eager FK loading — emits `LEFT JOIN <related> ON ...`
    /// in the main SELECT (with aliased child columns `<field>__<col>`)
    /// so one round-trip pulls the parent + related rows together.
    ///
    /// Trade-off vs. [`Self::select_related`]:
    ///   - `select_related` runs ONE extra batched query after the
    ///     main fetch (`SELECT * FROM related WHERE id IN (...)`).
    ///     Two round-trips total; rows stay narrow.
    ///   - `join_related` runs the main query AS the join — one
    ///     round-trip total — at the cost of a wider per-row payload
    ///     (every related column rides along even for duplicated
    ///     parents).
    ///
    /// Both populate `ForeignKey<U>.resolved` the same way so
    /// downstream code (templates, serde) doesn't care which path
    /// was used. Pick `join_related` when round-trip count dominates
    /// (hot listing pages, small related tables) and `select_related`
    /// when the related row is wide or only loaded for a subset of
    /// the parent rows.
    ///
    /// Composes with `.select_related(other_fk)` and
    /// `.prefetch_related(m2m)` — different fields can take different
    /// paths in the same query.
    ///
    /// **Constraints at v1**: one-hop only (no `"author__manager"`
    /// chains), FK fields must live in `model.fields` (M2M routes
    /// through `prefetch_related`), and the related model must be
    /// registered with the framework (`App::builder().model::<U>()`
    /// or contributed by a plugin) so we can resolve its column
    /// layout for the aliased SELECT.
    pub fn join_related(mut self, field_name: impl Into<String>) -> Self {
        self.join_related.push(field_name.into());
        self
    }

    /// Sugar for chained [`Self::join_related`] calls.
    pub fn join_related_many(mut self, field_names: &[&str]) -> Self {
        for name in field_names {
            self.join_related.push(name.to_string());
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
    /// ## Reverse-FK collections (post-#44)
    ///
    /// `prefetch_related` also loads `ReverseSet<C>` fields — the
    /// "for each Post, give me every Comment that points at it"
    /// shape. Declare the field on the parent with
    /// `#[sqlx(skip)] #[serde(skip)]
    /// #[umbra(reverse_fk = "<fk_col>")] pub <name>: ReverseSet<C>`
    /// where `<fk_col>` names the FK column on `C` pointing back.
    /// One `SELECT * FROM <child> WHERE <fk_col> IN (parent_pks)`
    /// regardless of parent count — no N+1.
    ///
    /// ## Scope (v1)
    ///
    /// - **M2M + reverse-FK only.** FK fields go through
    ///   [`Self::select_related`] (batched IN) or
    ///   [`Self::join_related`] (LEFT JOIN).
    /// - **i64 parent PK only.** Same constraint as the rest of the
    ///   M2M plumbing; models with non-i64 PKs surface a clean
    ///   compile error.
    /// - **Unknown field name → loud error** (post-#42). If the
    ///   name matches neither an M2M nor a `ReverseSet` field,
    ///   fetch returns a clear `sqlx::Error::Protocol` pointing at
    ///   the right method.
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
/// Validate that every name passed to `.join_related(...)` resolves
/// to a foreign-key field on `T`. Loud-error path that replaces the
/// pre-#42 silent no-op (where an unknown field, an M2M field, or a
/// non-FK column produced an empty JOIN list and a confusing
/// "ForeignKey::resolved() returned None" downstream).
fn validate_join_related_fields<T: Model>(fields: &[String]) -> Result<(), sqlx::Error> {
    for field_name in fields {
        // Try as a regular column first.
        let col = T::FIELDS.iter().find(|f| f.name == field_name.as_str());
        if let Some(col) = col {
            if col.fk_target.is_some() {
                continue; // FK column — OK.
            }
            return Err(sqlx::Error::Protocol(format!(
                "umbra::orm::join_related: field `{field_name}` on `{}` is not a foreign \
                 key (it has no fk_target)",
                T::NAME
            )));
        }
        // M2M field name? Post-#113 these go through the double
        // LEFT JOIN path: apply_join_related emits
        // `LEFT JOIN <junction> LEFT JOIN <child>` with aliased
        // child cols, and fetch()'s dedup-aware path collects M2M
        // children per parent. Trade-off documented at the join_related
        // docstring — M2M JOINs multiply parent rows by avg
        // cardinality, so prefetch_related stays the default for
        // any list page where M2M cardinality isn't tiny.
        if T::M2M_RELATIONS
            .iter()
            .any(|r| r.field_name == field_name.as_str())
        {
            continue;
        }
        return Err(sqlx::Error::Protocol(format!(
            "umbra::orm::join_related: unknown field `{field_name}` on model `{}`",
            T::NAME
        )));
    }
    Ok(())
}

/// Gap #111 — error returned when a typed terminal (`fetch` / `first`
/// / `get`) runs against a QuerySet that has `.only(...)` set. A
/// partial-column row can't satisfy `T`'s `FromRow` impl, so the
/// caller has to either drop the `.only(...)` (full SELECT, typed
/// rows back) or terminate via `.values(&[...])` (JSON rows with
/// just the requested columns). The message names the offending
/// terminal so the fix is one rename away.
fn only_with_typed_terminal_error(terminal: &'static str) -> sqlx::Error {
    sqlx::Error::Protocol(format!(
        "umbra::orm::{terminal}: cannot run a typed terminal on a QuerySet \
         with `.only(...)` set — a partial-column row can't hydrate `T` via \
         FromRow. Either drop `.only(...)` to fetch full typed rows, or \
         terminate via `.values(&[...])` to get JSON rows with just the \
         projected columns."
    ))
}

fn resolve_pool<T: Model>(explicit: Option<DbPool>) -> DbPool {
    if let Some(pool) = explicit {
        return pool;
    }
    if let Some(alias) = crate::migrate::model_alias(T::NAME) {
        return crate::db::pool_for_dispatched(&alias).clone();
    }
    crate::db::pool_dispatched().clone()
}

// GetError / TryForEachError moved to `errors`; re-exported above.

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
        let mut q = self.build_query_for("sqlite");
        self.apply_join_related(&mut q);
        self.apply_only_projection(&mut q);
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
        let mut q = self.build_query_for("postgres");
        self.apply_join_related(&mut q);
        self.apply_only_projection(&mut q);
        let (sql, _values) = q.build_sqlx(PostgresQueryBuilder);
        sql
    }

    /// Internal helper — when `.only(...)` was set, swap the SELECT
    /// list for just those columns. Shared by `to_sql` / `to_sql_pg`
    /// so the inspection surface stays in sync with what `values()`
    /// would emit. No-op when `only_cols` is `None`.
    fn apply_only_projection(&self, q: &mut sea_query::SelectStatement) {
        if let Some(cols) = &self.only_cols {
            q.clear_selects();
            for c in cols {
                q.column(Alias::new(c.as_str()));
            }
        }
    }

    /// Internal helper — when `.join_related(name)` was set, wrap the
    /// current query as a subquery and build an outer SELECT that
    /// LEFT JOINs every requested related table (with child columns
    /// aliased as `<field>__<col>`). The subquery wrapper is the
    /// load-bearing trick: WHERE / ORDER BY / LIMIT predicates inside
    /// the inner query reference parent columns only — there's no
    /// JOIN in scope, so bare names like `id` resolve unambiguously
    /// to the parent. Without this wrapper SQLite raises
    /// "ambiguous column name: id" on any predicate sharing a column
    /// name with a JOIN'd table.
    ///
    /// No-op when `join_related` is empty. Unknown field names /
    /// unregistered related models / FK columns missing `fk_target`
    /// are silently skipped — the SQL just won't carry the JOIN and
    /// the caller notices when `ForeignKey::resolved()` stays empty.
    fn apply_join_related(&self, q: &mut sea_query::SelectStatement) {
        if self.join_related.is_empty() {
            return;
        }
        use sea_query::{Expr, Query};
        let registered = crate::migrate::registered_models();

        // Inner-subquery column trim. When `.only(...)` is also set,
        // the outer SELECT only references a subset of parent
        // columns (plus the JOIN'd child columns it gets through
        // its alias). The inner subquery only needs to expose:
        //   - parent columns named in `.only(...)` (intersected
        //     with T::FIELDS so the JOIN aliases like
        //     `category__name` don't leak in here — those live on
        //     the JOIN'd table, not on the parent),
        //   - PLUS the FK columns each `.join_related(name)` needs
        //     for its `ON __p.<name> = <child>.<pk>` clause.
        // WHERE / ORDER BY inside the inner subquery can still
        // reference any parent column (SQL doesn't require an
        // ORDER BY column to be in the SELECT list), so we don't
        // need to promote those. Postgres often skips this prune
        // through subquery boundaries; SQLite usually doesn't —
        // either way trimming here is a measurable win on wide
        // tables (think 30-column Product on a busy hot path).
        if let Some(only) = &self.only_cols {
            let parent_field_names: std::collections::HashSet<&str> =
                T::FIELDS.iter().map(|f| f.name).collect();
            let mut needed: std::collections::HashSet<String> = only
                .iter()
                .filter(|c| parent_field_names.contains(c.as_str()))
                .cloned()
                .collect();
            for join_field in &self.join_related {
                if parent_field_names.contains(join_field.as_str()) {
                    needed.insert(join_field.clone());
                }
            }
            if !needed.is_empty() {
                q.clear_selects();
                // Stable ordering so the SQL is deterministic
                // across runs (HashSet iteration is not).
                let mut ordered: Vec<String> = needed.into_iter().collect();
                ordered.sort();
                for col in &ordered {
                    q.column(Alias::new(col.as_str()));
                }
            }
        }

        // Take ownership of the (possibly-trimmed) inner query and
        // re-mount it as the FROM clause of the new outer SELECT.
        let inner = std::mem::replace(q, Query::select().take());
        let parent_alias = Alias::new("__p");
        let mut outer = Query::select();
        outer.from_subquery(inner, parent_alias.clone());
        // Re-project the parent's full column set so the outer SELECT
        // exposes them unaliased — FromRow on `T` reads parent
        // columns by their bare names (`id`, `name`, ...). When
        // `.only(...)` later clears this list in
        // `apply_only_projection`, the inner-subquery trim above
        // means we still didn't pay for columns we ended up
        // dropping anyway.
        for f in T::FIELDS {
            outer.expr(Expr::col((parent_alias.clone(), Alias::new(f.name))));
        }
        for field_name in &self.join_related {
            // FK branch first (the original path).
            if let Some(fk_field) = T::FIELDS.iter().find(|f| f.name == field_name.as_str())
                && let Some(related_table) = fk_field.fk_target
                && let Some(related_meta) = registered.iter().find(|m| m.table == related_table)
                && let Some(related_pk) = related_meta.fields.iter().find(|c| c.primary_key)
            {
                // Per-field table alias so two FKs to the SAME related
                // table (e.g. `category` and `brand` both → `category`)
                // each get a distinct identifier in the JOIN. Without
                // this SQLite raises "ambiguous column name" on the
                // second JOIN.
                let join_alias = Alias::new(format!("__j_{field_name}"));
                outer.join_as(
                    sea_query::JoinType::LeftJoin,
                    Alias::new(related_table),
                    join_alias.clone(),
                    Expr::col((parent_alias.clone(), Alias::new(field_name.as_str())))
                        .equals((join_alias.clone(), Alias::new(related_pk.name.as_str()))),
                );
                // Aliased child cols pull from the per-field JOIN alias,
                // so `category__name` and `brand__name` resolve to two
                // different jr_category rows even though they share a
                // table.
                for col in &related_meta.fields {
                    let alias = format!("{}__{}", field_name, col.name);
                    outer.expr_as(
                        Expr::col((join_alias.clone(), Alias::new(col.name.as_str()))),
                        Alias::new(alias),
                    );
                }
                continue;
            }

            // M2M branch (post-#113). Emit the double LEFT JOIN
            // through the junction table:
            //   LEFT JOIN <junction> AS __jm_<field>
            //     ON __p.<parent_pk> = __jm_<field>.parent_id
            //   LEFT JOIN <child_table> AS __j_<field>
            //     ON __jm_<field>.child_id = __j_<field>.<child_pk>
            // Aliased child cols use the same `<field>__<col>` shape
            // as the FK branch so the decode helper can be reused.
            if let Some(m2m_rel) = T::M2M_RELATIONS
                .iter()
                .find(|r| r.field_name == field_name.as_str())
                && let Some(parent_pk) = T::FIELDS.iter().find(|f| f.primary_key)
                && let Some(child_meta) =
                    registered.iter().find(|m| m.table == m2m_rel.target_table)
                && let Some(child_pk) = child_meta.fields.iter().find(|c| c.primary_key)
            {
                let junction_table = format!("{}_{}", T::TABLE, field_name);
                let junction_alias = Alias::new(format!("__jm_{field_name}"));
                let child_alias = Alias::new(format!("__j_{field_name}"));
                outer.join_as(
                    sea_query::JoinType::LeftJoin,
                    Alias::new(junction_table),
                    junction_alias.clone(),
                    Expr::col((parent_alias.clone(), Alias::new(parent_pk.name)))
                        .equals((junction_alias.clone(), Alias::new("parent_id"))),
                );
                outer.join_as(
                    sea_query::JoinType::LeftJoin,
                    Alias::new(m2m_rel.target_table),
                    child_alias.clone(),
                    Expr::col((junction_alias.clone(), Alias::new("child_id")))
                        .equals((child_alias.clone(), Alias::new(child_pk.name.as_str()))),
                );
                for col in &child_meta.fields {
                    let alias = format!("{}__{}", field_name, col.name);
                    outer.expr_as(
                        Expr::col((child_alias.clone(), Alias::new(col.name.as_str()))),
                        Alias::new(alias),
                    );
                }
                continue;
            }
        }
        *q = outer;
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
        if self.only_cols.is_some() {
            return Err(only_with_typed_terminal_error("fetch"));
        }
        let sr_fields = self.select_related.clone();
        let prefetch_fields = self.prefetch_related.clone();
        let join_fields = self.join_related.clone();
        // Validate join_related field names up front so a typo or an
        // M2M field name doesn't silently no-op (it used to render a
        // SELECT with no JOIN). Pre-#42 the failure mode was
        // `ForeignKey::resolved()` stays None and the caller debugs
        // the wrong thing. Now they get a typed error.
        validate_join_related_fields::<T>(&join_fields)?;
        // The turbofish on `query_as_with::<DB, _, _>` is load-bearing:
        // with both `sqlx-sqlite` and `sqlx-postgres` features on
        // sea-query-binder, `SqlxValues` implements `IntoArguments` for
        // both backends, so the compiler can't infer DB from the values
        // alone. Naming DB explicitly pins which `FromRow` bound is
        // checked.
        // Split join_fields into FK vs M2M groups up front. The
        // M2M branch needs parent dedup (one parent row per JOIN
        // combo would surface duplicate Ts to the caller); the FK
        // branch is one-to-one with rows.
        let (m2m_join_fields, fk_join_fields): (Vec<String>, Vec<String>) = join_fields
            .iter()
            .cloned()
            .partition(|f| T::M2M_RELATIONS.iter().any(|r| r.field_name == f.as_str()));
        let has_m2m_join = !m2m_join_fields.is_empty();

        let mut rows = match resolve_pool::<T>(self.explicit_pool.clone()) {
            DbPool::Sqlite(pool) => {
                let mut q = self.build_query_for("sqlite");
                self.apply_join_related(&mut q);
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                if join_fields.is_empty() {
                    sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?
                } else if !has_m2m_join {
                    // FK-only JOIN path: one row in → one T out.
                    let raw_rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?;
                    let mut typed = Vec::with_capacity(raw_rows.len());
                    for row in &raw_rows {
                        let mut t = <T as sqlx::FromRow<_>>::from_row(row)?;
                        backend_sqlite::hydrate_joined_rels::<T>(&mut t, row, &fk_join_fields)?;
                        typed.push(t);
                    }
                    typed
                } else {
                    // Mixed (FK + M2M) or pure M2M JOIN path:
                    // dedup parents, collect M2M children per
                    // (parent_pk, field).
                    let raw_rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?;
                    dedup_decode_sqlite::<T>(&raw_rows, &fk_join_fields, &m2m_join_fields)?
                }
            }
            DbPool::Postgres(pool) => {
                let mut q = self.build_query_for("postgres");
                self.apply_join_related(&mut q);
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                if join_fields.is_empty() {
                    sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?
                } else if !has_m2m_join {
                    let raw_rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?;
                    let mut typed = Vec::with_capacity(raw_rows.len());
                    for row in &raw_rows {
                        let mut t = <T as sqlx::FromRow<_>>::from_row(row)?;
                        backend_pg::hydrate_joined_rels::<T>(&mut t, row, &fk_join_fields)?;
                        typed.push(t);
                    }
                    typed
                } else {
                    let raw_rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                        .fetch_all(&pool)
                        .await?;
                    dedup_decode_pg::<T>(&raw_rows, &fk_join_fields, &m2m_join_fields)?
                }
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
        if self.only_cols.is_some() {
            return Err(only_with_typed_terminal_error("first"));
        }
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
        // Annotations are part of the built query (they render inside
        // build_query_for), so the plan below includes them — but a
        // poisoned annotation (unknown relation) must fail loudly
        // here, not silently vanish from the plan.
        self.check_annotations()?;
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
        // Gap #46 follow-up: if any name uses `__` traversal
        // (`author__id`), route to the JOIN-aware path that builds
        // nested per-relation JSON objects. The unbranched path
        // below stays byte-for-byte identical for the common
        // parent-cols-only case.
        if columns.iter().any(|c| c.contains("__")) {
            return self.values_with_traversal(columns).await;
        }
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

    /// `.values("author__name")`-style traversal. One-hop only at
    /// v1 (`a__b`, not `a__b__c` — that fails loudly so the user
    /// doesn't get a silent partial result). Emits one LEFT JOIN
    /// per distinct relation referenced, aliases child columns as
    /// `<rel>__<col>`, and returns each row as a nested JSON
    /// object: `{id, title, author: {id, name}, editor: {id}}`.
    ///
    /// A LEFT JOIN miss (nullable FK pointing at nothing) maps the
    /// whole relation key to `Value::Null` rather than a nested
    /// object full of nulls — caller code that branches on
    /// `obj["author"].is_null()` works naturally.
    ///
    /// Validates every name up front so a typo errors before any
    /// SQL runs. Unknown parent col / non-FK relation name /
    /// unknown child col / deeper-than-one-hop path all surface
    /// distinct messages.
    async fn values_with_traversal(self, columns: &[&str]) -> Result<Vec<JsonValue>, sqlx::Error> {
        use sea_query::{Expr, Query};
        let meta = crate::migrate::ModelMeta::for_::<T>();
        let registered = crate::migrate::registered_models();

        // Split each column name into (relation, child_col) or
        // (None, parent_col). Reject paths with more than one `__`
        // hop — nested traversal across two relation layers is a
        // separate piece of work (it'd need to chain JOINs and
        // build doubly-nested JSON).
        let mut parent_cols: Vec<String> = Vec::new();
        let mut per_rel: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for raw in columns {
            let mut parts = raw.splitn(3, "__");
            let first = parts.next().unwrap_or("");
            let second = parts.next();
            let third = parts.next();
            if third.is_some() {
                return Err(sqlx::Error::Protocol(format!(
                    "umbra::orm::values: nested `{raw}` is not supported in v1 \
                     (one-hop only — `a__b`, not `a__b__c`)"
                )));
            }
            match second {
                Some(child) => {
                    per_rel
                        .entry(first.to_string())
                        .or_default()
                        .push(child.to_string());
                }
                None => parent_cols.push(first.to_string()),
            }
        }

        // Validate every parent name against T::FIELDS.
        for name in &parent_cols {
            if !meta.fields.iter().any(|c| c.name == *name) {
                return Err(sqlx::Error::Protocol(format!(
                    "umbra::orm::values: unknown column `{name}` on model `{}`",
                    T::NAME
                )));
            }
        }

        // Validate every relation + child trio. Build a struct per
        // relation that the SQL/decoder loops below need.
        struct RelInfo<'a> {
            rel_name: String,
            related_table: &'a str,
            related_pk: &'a crate::migrate::Column,
            child_cols: Vec<&'a crate::migrate::Column>,
        }
        let mut rel_infos: Vec<RelInfo<'_>> = Vec::with_capacity(per_rel.len());
        for (rel_name, child_names) in &per_rel {
            let fk_field = meta.fields.iter().find(|c| c.name == *rel_name);
            let Some(fk_field) = fk_field else {
                return Err(sqlx::Error::Protocol(format!(
                    "umbra::orm::values: unknown relation `{rel_name}` on model `{}` \
                     (used in `{rel_name}__...`)",
                    T::NAME
                )));
            };
            let Some(related_table) = fk_field.fk_target.as_deref() else {
                return Err(sqlx::Error::Protocol(format!(
                    "umbra::orm::values: field `{rel_name}` on `{}` is not a foreign \
                     key — `__` traversal only works through FK fields",
                    T::NAME
                )));
            };
            let Some(related_meta) = registered.iter().find(|m| m.table == related_table) else {
                return Err(sqlx::Error::Protocol(format!(
                    "umbra::orm::values: related model for table `{related_table}` \
                     is not registered"
                )));
            };
            let Some(related_pk) = related_meta.fields.iter().find(|c| c.primary_key) else {
                return Err(sqlx::Error::Protocol(format!(
                    "umbra::orm::values: related model `{related_table}` has no \
                     primary key column"
                )));
            };
            let mut child_cols: Vec<&crate::migrate::Column> =
                Vec::with_capacity(child_names.len());
            for name in child_names {
                let col = related_meta
                    .fields
                    .iter()
                    .find(|c| c.name == *name)
                    .ok_or_else(|| {
                        sqlx::Error::Protocol(format!(
                            "umbra::orm::values: unknown child column `{name}` on \
                             related model `{related_table}` (full path `{rel_name}__{name}`)"
                        ))
                    })?;
                child_cols.push(col);
            }
            rel_infos.push(RelInfo {
                rel_name: rel_name.clone(),
                related_table,
                related_pk,
                child_cols,
            });
        }

        // Build the SQL. Subquery-wrap the parent (WHERE / ORDER
        // BY / LIMIT all stay scoped to it) so JOIN'd tables can't
        // shadow bare-column predicates — same trick
        // apply_join_related uses.
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        let inner = self.build_query_for(backend);
        let parent_alias = Alias::new("__p");
        let mut outer = Query::select();
        outer.from_subquery(inner, parent_alias.clone());
        // Outer SELECT: parent cols (bare), aliased child cols.
        for name in &parent_cols {
            outer.expr_as(
                Expr::col((parent_alias.clone(), Alias::new(name.as_str()))),
                Alias::new(name.as_str()),
            );
        }
        for info in &rel_infos {
            let join_alias = Alias::new(format!("__j_{}", info.rel_name));
            outer.join_as(
                sea_query::JoinType::LeftJoin,
                Alias::new(info.related_table),
                join_alias.clone(),
                Expr::col((parent_alias.clone(), Alias::new(info.rel_name.as_str()))).equals((
                    join_alias.clone(),
                    Alias::new(info.related_pk.name.as_str()),
                )),
            );
            // Always include the related PK alias so the decoder
            // can detect a LEFT JOIN miss → emit Value::Null for
            // the whole relation. Plus every requested child col.
            let pk_alias = format!("{}__{}", info.rel_name, info.related_pk.name);
            outer.expr_as(
                Expr::col((
                    join_alias.clone(),
                    Alias::new(info.related_pk.name.as_str()),
                )),
                Alias::new(pk_alias),
            );
            for col in &info.child_cols {
                let alias = format!("{}__{}", info.rel_name, col.name);
                outer.expr_as(
                    Expr::col((join_alias.clone(), Alias::new(col.name.as_str()))),
                    Alias::new(alias),
                );
            }
        }

        // Execute + decode. Per-backend dispatch.
        match pool {
            DbPool::Sqlite(pool) => {
                let (sql, vals) = outer.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out: Vec<JsonValue> = Vec::with_capacity(rows.len());
                for row in &rows {
                    let mut obj = serde_json::Map::new();
                    // Parent cols decode by their bare alias name.
                    for name in &parent_cols {
                        if let Some(col) = meta.fields.iter().find(|c| c.name == *name) {
                            let v = crate::orm::dynamic::decode_to_json_aliased(row, col, name)?;
                            obj.insert(name.clone(), v);
                        }
                    }
                    // Per-relation nested object — `null` on LEFT JOIN miss.
                    for info in &rel_infos {
                        let pk_alias = format!("{}__{}", info.rel_name, info.related_pk.name);
                        let pk_is_null =
                            sqlx::Row::try_get::<Option<i64>, _>(row, pk_alias.as_str())
                                .map(|v| v.is_none())
                                .unwrap_or(true);
                        if pk_is_null {
                            obj.insert(info.rel_name.clone(), JsonValue::Null);
                            continue;
                        }
                        let mut nested = serde_json::Map::with_capacity(info.child_cols.len());
                        for col in &info.child_cols {
                            let alias = format!("{}__{}", info.rel_name, col.name);
                            let v = crate::orm::dynamic::decode_to_json_aliased(row, col, &alias)?;
                            nested.insert(col.name.clone(), v);
                        }
                        obj.insert(info.rel_name.clone(), JsonValue::Object(nested));
                    }
                    out.push(JsonValue::Object(obj));
                }
                Ok(out)
            }
            DbPool::Postgres(pool) => {
                let (sql, vals) = outer.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out: Vec<JsonValue> = Vec::with_capacity(rows.len());
                for row in &rows {
                    let mut obj = serde_json::Map::new();
                    for name in &parent_cols {
                        if let Some(col) = meta.fields.iter().find(|c| c.name == *name) {
                            let v = crate::orm::dynamic::decode_pg_to_json_aliased(row, col, name)?;
                            obj.insert(name.clone(), v);
                        }
                    }
                    for info in &rel_infos {
                        let pk_alias = format!("{}__{}", info.rel_name, info.related_pk.name);
                        let pk_is_null =
                            sqlx::Row::try_get::<Option<i64>, _>(row, pk_alias.as_str())
                                .map(|v| v.is_none())
                                .unwrap_or(true);
                        if pk_is_null {
                            obj.insert(info.rel_name.clone(), JsonValue::Null);
                            continue;
                        }
                        let mut nested = serde_json::Map::with_capacity(info.child_cols.len());
                        for col in &info.child_cols {
                            let alias = format!("{}__{}", info.rel_name, col.name);
                            let v =
                                crate::orm::dynamic::decode_pg_to_json_aliased(row, col, &alias)?;
                            nested.insert(col.name.clone(), v);
                        }
                        obj.insert(info.rel_name.clone(), JsonValue::Object(nested));
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
                        backend_sqlite::decode_agg(&row, name, agg, source_ty)?,
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
                    obj.insert(
                        name.to_string(),
                        backend_pg::decode_agg(&row, name, agg, source_ty)?,
                    );
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
                            backend_sqlite::decode_agg(row, name, agg, source_ty)?,
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
                        obj.insert(
                            name.to_string(),
                            backend_pg::decode_agg(row, name, agg, source_ty)?,
                        );
                    }
                    out.push(JsonValue::Object(obj));
                }
                Ok(out)
            }
        }
    }

    /// Django's chainable `annotate(alias=Agg("relation"))`: attach a
    /// related-aggregate annotation to this QuerySet. The annotation
    /// is **query-builder state** — it renders as a correlated scalar
    /// subquery inside the one SELECT every terminal builds, so it
    /// composes with `.filter` / `.order_by` / `.limit`, stacks with
    /// further annotations, and shows up in [`Self::explain`] /
    /// [`Self::to_sql`] out of the box. Never a side query, never an
    /// N+1.
    ///
    /// `relation` names a `ReverseSet` relation on the model
    /// (`#[umbra(reverse_fk = "...")]`), the same names
    /// `prefetch_related` accepts. Any [`crate::orm::Aggregate`]
    /// works; non-count aggregates name a column on the CHILD model:
    ///
    /// ```rust,ignore
    /// let rows = Plugin::objects()
    ///     .filter(plugin::MODERATION.eq("approved"))
    ///     .annotate_count("comment_set")                                // COUNT(*)
    ///     .annotate_related("rating_avg", "review_set", Aggregate::avg("rating"))
    ///     .fetch_annotated()
    ///     .await?;                       // Vec<(Plugin, Map<alias, value>)>
    /// ```
    ///
    /// An unknown relation name doesn't panic the (infallible)
    /// builder — it poisons the annotation, and every fallible
    /// consumer (`fetch_annotated`, `explain`) reports it loudly.
    /// v1 caveats: child rows aggregate unconditionally — a
    /// child-side predicate (Django's `Count(..., filter=Q(...))`)
    /// and child soft-delete awareness are tracked follow-ups
    /// (gaps2 #39).
    pub fn annotate_related(
        mut self,
        alias: &str,
        relation: &str,
        agg: crate::orm::Aggregate,
    ) -> Self {
        let resolved = T::REVERSE_FK_RELATIONS
            .iter()
            .find(|r| r.field_name == relation)
            .map(|spec| {
                let pk = T::FIELDS
                    .iter()
                    .find(|f| f.primary_key)
                    .map(|f| f.name)
                    .unwrap_or("id");
                (
                    spec.target_table.to_string(),
                    spec.fk_column.to_string(),
                    T::TABLE.to_string(),
                    pk.to_string(),
                )
            })
            .ok_or_else(|| {
                format!(
                    "umbra::orm::annotate_related: `{relation}` is not a reverse-FK relation on `{}` — declared relations: [{}]",
                    T::NAME,
                    T::REVERSE_FK_RELATIONS
                        .iter()
                        .map(|r| r.field_name)
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            });
        self.annotations.push(RelatedAnnotation {
            alias: alias.to_string(),
            agg,
            resolved,
        });
        self
    }

    /// Sugar for the overwhelmingly common annotation:
    /// `annotate_related("<relation>_count", relation, Aggregate::count())`.
    /// `.annotate_count("comment_set")` exposes the value under the
    /// `comment_set_count` alias in [`Self::fetch_annotated`].
    pub fn annotate_count(self, relation: &str) -> Self {
        let alias = format!("{relation}_count");
        self.annotate_related(&alias, relation, crate::orm::Aggregate::count())
    }

    /// Loud-failure check for poisoned annotations (unknown relation
    /// names recorded by the infallible builder). Called by every
    /// fallible consumer before SQL runs.
    fn check_annotations(&self) -> Result<(), sqlx::Error> {
        for ann in &self.annotations {
            if let Err(msg) = &ann.resolved {
                return Err(sqlx::Error::Protocol(msg.clone()));
            }
        }
        Ok(())
    }

    /// Run the SELECT and return every matching row **with its
    /// annotation values** — the execution terminal for
    /// [`Self::annotate_related`] / [`Self::annotate_count`]. One
    /// query; each row's annotations arrive as an `alias → JSON
    /// value` map (count → integer, AVG → float/null, SUM/MAX/MIN →
    /// typed per the child column, NULL on empty sets for the
    /// non-count aggregates).
    pub async fn fetch_annotated(
        self,
    ) -> Result<Vec<(T, serde_json::Map<String, JsonValue>)>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.check_annotations()?;
        // Child-column types for SUM/MAX/MIN decoding, resolved from
        // the runtime registry (the child model is known by table
        // name only). `None` falls back to decode_agg's string path.
        let source_types: Vec<(String, crate::orm::Aggregate, Option<crate::orm::SqlType>)> = {
            let registry_up = crate::migrate::is_initialised();
            self.annotations
                .iter()
                .map(|ann| {
                    let ty = match (&ann.resolved, registry_up, ann.agg.source_column()) {
                        (Ok((child_table, ..)), true, Some(col)) => {
                            crate::migrate::registered_models()
                                .into_iter()
                                .find(|m| m.table == *child_table)
                                .and_then(|m| m.fields.iter().find(|f| f.name == col).map(|f| f.ty))
                        }
                        _ => None,
                    };
                    (ann.alias.clone(), ann.agg.clone(), ty)
                })
                .collect()
        };

        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        match pool {
            DbPool::Sqlite(pool) => {
                let q = self.build_query_for("sqlite");
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out = Vec::with_capacity(rows.len());
                for row in &rows {
                    let t = <T as sqlx::FromRow<_>>::from_row(row)?;
                    let mut anns = serde_json::Map::with_capacity(source_types.len());
                    for (alias, agg, ty) in &source_types {
                        anns.insert(
                            alias.clone(),
                            backend_sqlite::decode_agg(row, alias, agg, *ty)?,
                        );
                    }
                    out.push((t, anns));
                }
                Ok(out)
            }
            DbPool::Postgres(pool) => {
                let q = self.build_query_for("postgres");
                let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, vals)
                    .fetch_all(&pool)
                    .await?;
                let mut out = Vec::with_capacity(rows.len());
                for row in &rows {
                    let t = <T as sqlx::FromRow<_>>::from_row(row)?;
                    let mut anns = serde_json::Map::with_capacity(source_types.len());
                    for (alias, agg, ty) in &source_types {
                        anns.insert(alias.clone(), backend_pg::decode_agg(row, alias, agg, *ty)?);
                    }
                    out.push((t, anns));
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
    ///
    /// Feature #72: for `#[umbra(soft_delete)]` models this rewrites
    /// to `UPDATE ... SET deleted_at = NOW() WHERE ...` so rows
    /// survive in the DB (filtered out of subsequent queries by the
    /// auto `WHERE deleted_at IS NULL`). Call `.hard_delete()`
    /// beforehand for a real DELETE (GDPR purge, test cleanup).
    pub async fn delete(self) -> Result<u64, sqlx::Error> {
        // Feature #72 — soft-delete redirect. The whole `delete()`
        // contract collapses to an UPDATE setting `deleted_at`. We
        // keep the bulk_post_delete signal so subscribers see the
        // same event shape regardless of the underlying SQL.
        if self.soft_delete_active && !self.hard_delete {
            return self.soft_delete_update().await;
        }
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
                        .map(|r| backend_sqlite::pk_to_json(r, field.name, field.ty))
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
                        .map(|r| backend_pg::pk_to_json(r, field.name, field.ty))
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
                        .map(|r| backend_sqlite::pk_to_json(r, pkf.name, pkf.ty))
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
                        .map(|r| backend_pg::pk_to_json(r, pkf.name, pkf.ty))
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
                        .map(|r| backend_sqlite::pk_to_json(r, field.name, field.ty))
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
                        .map(|r| backend_pg::pk_to_json(r, field.name, field.ty))
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

    /// Feature #72 — soft-delete rewrite: turn `DELETE FROM table
    /// WHERE ...` into `UPDATE table SET deleted_at = NOW() WHERE
    /// ... AND deleted_at IS NULL`. The trailing `IS NULL` guard
    /// makes the operation idempotent: re-soft-deleting an already-
    /// soft-deleted row doesn't bump its timestamp. Fires
    /// `bulk_post_delete:<table>` so subscribers see the same event
    /// shape as a hard delete.
    async fn soft_delete_update(self) -> Result<u64, sqlx::Error> {
        let atomic = self.should_atomic_wrap();
        let pool = resolve_pool::<T>(self.explicit_pool.clone());
        let backend = pool.backend_name();
        let now = chrono::Utc::now();
        let mut stmt = sea_query::Query::update();
        stmt.table(Alias::new(T::TABLE));
        stmt.value(
            Alias::new("deleted_at"),
            sea_query::Value::ChronoDateTimeUtc(Some(Box::new(now))),
        );
        for p in &self.predicates {
            stmt.and_where(p.cond_for(backend));
        }
        // Idempotency guard — never bump an already-set deleted_at.
        stmt.and_where(sea_query::Expr::col(Alias::new("deleted_at")).is_null());
        let pk = pk_field::<T>();
        if let Some(pkf) = pk {
            stmt.returning_col(Alias::new(pkf.name));
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
                        .map(|r| backend_sqlite::pk_to_json(r, field.name, field.ty))
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
                        .map(|r| backend_pg::pk_to_json(r, field.name, field.ty))
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
            let sea_value =
                json_to_sea_value(field.ty, val, field.nullable, field.name, fk_pk_hint(field))?;
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
        // Feature #72 — snapshot the model's soft-delete opt-in into
        // the QuerySet so the build_query_for path knows whether to
        // auto-inject `WHERE deleted_at IS NULL`. Without this
        // snapshot the `impl<T> QuerySet<T>` path can't read
        // `T::SOFT_DELETE` (T is unbounded there).
        qs.soft_delete_active = T::SOFT_DELETE;
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

    /// Feature #72 — see `QuerySet::with_deleted`.
    pub fn with_deleted(&self) -> QuerySet<T> {
        self.queryset().with_deleted()
    }

    /// Feature #72 — see `QuerySet::only_deleted`.
    pub fn only_deleted(&self) -> QuerySet<T> {
        self.queryset().only_deleted()
    }

    /// Gap #111 — see [`QuerySet::only`].
    pub fn only(&self, cols: &[&str]) -> QuerySet<T> {
        self.queryset().only(cols)
    }

    /// See [`QuerySet::join_related`].
    pub fn join_related(&self, field_name: impl Into<String>) -> QuerySet<T> {
        self.queryset().join_related(field_name)
    }

    /// See [`QuerySet::join_related_many`].
    pub fn join_related_many(&self, field_names: &[&str]) -> QuerySet<T> {
        self.queryset().join_related_many(field_names)
    }

    /// See [`QuerySet::select_related`].
    pub fn select_related(&self, field_name: impl Into<String>) -> QuerySet<T> {
        self.queryset().select_related(field_name)
    }

    /// See [`QuerySet::select_related_many`].
    pub fn select_related_many(&self, field_names: &[&str]) -> QuerySet<T> {
        self.queryset().select_related_many(field_names)
    }

    /// See [`QuerySet::prefetch_related`].
    pub fn prefetch_related(&self, field_name: impl Into<String>) -> QuerySet<T> {
        self.queryset().prefetch_related(field_name)
    }

    /// See [`QuerySet::prefetch_related_many`].
    pub fn prefetch_related_many(&self, field_names: &[&str]) -> QuerySet<T> {
        self.queryset().prefetch_related_many(field_names)
    }

    /// Feature #72 — see `QuerySet::hard_delete`.
    pub fn hard_delete(&self) -> QuerySet<T> {
        self.queryset().hard_delete()
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

    /// See [`QuerySet::annotate_related`] — starts an annotated chain
    /// from the manager, like `filter` does.
    pub fn annotate_related(
        &self,
        alias: &str,
        relation: &str,
        agg: crate::orm::Aggregate,
    ) -> QuerySet<T> {
        self.queryset().annotate_related(alias, relation, agg)
    }

    /// See [`QuerySet::annotate_count`].
    pub fn annotate_count(&self, relation: &str) -> QuerySet<T> {
        self.queryset().annotate_count(relation)
    }

    /// See [`QuerySet::fetch_annotated`].
    pub async fn fetch_annotated(
        &self,
    ) -> Result<Vec<(T, serde_json::Map<String, JsonValue>)>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.queryset().fetch_annotated().await
    }

    /// See [`QuerySet::values`].
    pub async fn values(&self, columns: &[&str]) -> Result<Vec<JsonValue>, sqlx::Error> {
        self.queryset().values(columns).await
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
    pub async fn create(&self, mut instance: T) -> Result<T, crate::orm::write::WriteError>
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
                // Carry form-staged M2M pending ids from the caller's
                // instance onto the readback row, then flush them to
                // junction rows now that parent_id + junction_table are
                // seeded on the readback row.
                instance.take_pending_m2m_into(&mut row);
                row.write_pending_m2m().await?;
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
                instance.take_pending_m2m_into(&mut row);
                row.write_pending_m2m().await?;
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
                        .map(|r| backend_sqlite::pk_to_json(r, field.name, field.ty))
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
                        .map(|r| backend_pg::pk_to_json(r, field.name, field.ty))
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
                crate::orm::write::json_to_sea_value(pk.ty, &pk_value_json, false, pk_name, None)?;
            let pk_pred: Predicate<T> =
                Predicate::new(sea_query::Expr::col(sea_query::Alias::new(pk_name)).eq(pk_sea));
            // Run the UPDATE.
            self.filter(pk_pred).update_values(update_map).await?;
            // Re-fetch to return the populated row. The PK predicate
            // is rebuilt because Predicate isn't Clone and the prior
            // one was moved into update_values.
            let pk_value_json2 = pk_value_json.clone();
            let pk_sea2 =
                crate::orm::write::json_to_sea_value(pk.ty, &pk_value_json2, false, pk_name, None)?;
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
                let cell = json_to_sea_value(
                    field.ty,
                    &val,
                    field.nullable,
                    field.name,
                    fk_pk_hint(field),
                )?;
                let pk_sea = json_to_sea_value(pk_ty, pk_val, false, pk_name, None)?;
                case = case.case(sea_query::Expr::col(Alias::new(pk_name)).eq(pk_sea), cell);
            }
            stmt.value(Alias::new(field.name), case);
        }

        // WHERE pk IN (<pk1>, <pk2>, ...)
        let pk_seas: Vec<sea_query::Value> = serialized
            .iter()
            .map(|(pk_val, _)| json_to_sea_value(pk_ty, pk_val, false, pk_name, None))
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

// QuerySetTx (struct + impl) moved to `super::tx`; re-exported above.

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
                    fk_pk_hint(field),
                )
                .map_err(SaveError::Write)?;
                stmt.value(Alias::new(field.name), sea_val);
            }
            // WHERE pk = <value>
            let pk_sea = crate::orm::write::json_to_sea_value(
                pk_field.ty,
                &pk_val,
                false,
                pk_field.name,
                None,
            )
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
            crate::orm::write::json_to_sea_value(pk_field.ty, &pk_val, false, pk_field.name, None)
                .map_err(SaveError::Write)?;

        use sea_query::{Alias, Expr, Query};
        // Feature #72 — soft-delete redirect. For models tagged
        // `#[umbra(soft_delete)]`, set deleted_at instead of issuing
        // DELETE. Pre/post_delete signals still fire because the
        // logical contract ("this row is gone from the visible
        // table") is preserved — only the physical SQL changed.
        // Hard-delete is not exposed through delete_instance (it's
        // a typed per-row helper); call `QuerySet::filter(pk =
        // instance.id).hard_delete().delete()` when you need it.
        let stmt_sql = if T::SOFT_DELETE {
            let now = chrono::Utc::now();
            let mut up = Query::update();
            up.table(Alias::new(T::TABLE));
            up.value(
                Alias::new("deleted_at"),
                sea_query::Value::ChronoDateTimeUtc(Some(Box::new(now))),
            );
            up.and_where(Expr::col(Alias::new(pk_field.name)).eq(pk_sea));
            // Idempotency guard — don't bump an already-set timestamp.
            up.and_where(Expr::col(Alias::new("deleted_at")).is_null());
            SoftOrHardStatement::Update(up)
        } else {
            let mut stmt = Query::delete();
            stmt.from_table(Alias::new(T::TABLE));
            stmt.and_where(Expr::col(Alias::new(pk_field.name)).eq(pk_sea));
            SoftOrHardStatement::Delete(stmt)
        };

        let pool = resolve_pool::<T>(None);
        let affected = match (&pool, stmt_sql) {
            (DbPool::Sqlite(pool), SoftOrHardStatement::Delete(stmt)) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(pool)
                    .await
                    .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                    .rows_affected()
            }
            (DbPool::Postgres(pool), SoftOrHardStatement::Delete(stmt)) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(pool)
                    .await
                    .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                    .rows_affected()
            }
            (DbPool::Sqlite(pool), SoftOrHardStatement::Update(stmt)) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(pool)
                    .await
                    .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                    .rows_affected()
            }
            (DbPool::Postgres(pool), SoftOrHardStatement::Update(stmt)) => {
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(pool)
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

/// Internal enum used by `Manager::delete_instance` to dispatch on
/// soft-delete vs hard-delete without duplicating the four backend ×
/// statement match arms inline. One variant per SQL shape.
enum SoftOrHardStatement {
    Delete(sea_query::DeleteStatement),
    Update(sea_query::UpdateStatement),
}

// Hydration helpers (hydrate_select_related, hydrate_select_related_nested,
// hydrate_prefetch_related, hydrate_reverse_fk_for_field, fetch_related_as_json,
// fetch_reverse_fk_children) moved to `super::hydration`.

// Insert builders (serialize_to_map, build_insert_one_for, build_insert_many_for)
// and pk_field moved to `super::write_helpers`.

// `decode_agg_sqlite` / `decode_agg_pg` moved to
// `backend_sqlite::decode_agg` / `backend_pg::decode_agg`.

// =========================================================================
// #113: M2M-via-JOIN dedup decoders
//
// When .join_related() includes one or more M2M fields, the result
// set has one row per (parent, child) combo, so a parent with N
// matching children appears N times. The caller wants ONE T per
// parent with the M2M slot populated.
//
// The algorithm:
//   - First time we see a parent PK: decode T via FromRow, hydrate
//     any FK joins from this row.
//   - Every subsequent row for the same parent: extract the M2M
//     child JsonValue (or skip on LEFT JOIN miss).
//   - Dedup children by (parent_pk, field, child_pk) so that
//     joining TWO M2Ms doesn't multiply the child sets (the JOIN
//     produces parent × m2m1 × m2m2 rows).
//   - Hand each parent its M2M buckets via set_m2m_resolved_json.
// =========================================================================

fn dedup_decode_sqlite<T: Model + HydrateRelated>(
    raw_rows: &[sqlx::sqlite::SqliteRow],
    fk_join_fields: &[String],
    m2m_join_fields: &[String],
) -> Result<Vec<T>, sqlx::Error>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>,
{
    let pk_name = T::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.name)
        .ok_or_else(|| {
            sqlx::Error::Protocol(format!(
                "umbra::orm::join_related: model `{}` has no primary key, M2M JOIN \
                 dedup requires one",
                T::NAME
            ))
        })?;
    let registered = crate::migrate::registered_models();
    let mut typed: Vec<T> = Vec::new();
    let mut idx_by_pk: HashMap<i64, usize> = HashMap::new();
    // (parent_pk, field) → Vec<JsonValue> + a Set tracking seen child PKs.
    let mut buckets: HashMap<(i64, String), Vec<JsonValue>> = HashMap::new();
    let mut seen_children: HashMap<(i64, String), std::collections::HashSet<i64>> = HashMap::new();
    for row in raw_rows {
        use sqlx::Row;
        let Ok(parent_pk) = row.try_get::<i64, _>(pk_name) else {
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(e) = idx_by_pk.entry(parent_pk) {
            let mut t = <T as sqlx::FromRow<_>>::from_row(row)?;
            backend_sqlite::hydrate_joined_rels::<T>(&mut t, row, fk_join_fields)?;
            e.insert(typed.len());
            typed.push(t);
        }
        for m2m_field in m2m_join_fields {
            let Some(rel) = T::M2M_RELATIONS
                .iter()
                .find(|r| r.field_name == m2m_field.as_str())
            else {
                continue;
            };
            let Some(child_meta) = registered.iter().find(|m| m.table == rel.target_table) else {
                continue;
            };
            let Some(child_json) =
                backend_sqlite::extract_m2m_child_json(row, m2m_field, child_meta)?
            else {
                continue;
            };
            // Dedup by child PK so multi-M2M cartesian doesn't
            // duplicate this field's children.
            let child_pk = child_json
                .as_object()
                .and_then(|m| {
                    let pk_col = child_meta.fields.iter().find(|c| c.primary_key)?;
                    m.get(&pk_col.name)?.as_i64()
                })
                .unwrap_or(0);
            let key = (parent_pk, m2m_field.clone());
            let seen = seen_children.entry(key.clone()).or_default();
            if seen.insert(child_pk) {
                buckets.entry(key).or_default().push(child_json);
            }
        }
    }
    for ((parent_pk, field), children) in buckets {
        if let Some(&idx) = idx_by_pk.get(&parent_pk) {
            typed[idx].set_m2m_resolved_json(&field, children);
        }
    }
    // LEFT JOIN miss handling: walk every (parent, field) pair
    // we expected to populate and zero-init any slot that never
    // got a hit. Without this a parent with no matching M2M
    // children would leave its slot None — distinguishable from
    // "loaded, empty" only by callers checking the absence.
    for (&parent_pk, &idx) in idx_by_pk.iter() {
        for field in m2m_join_fields {
            if !seen_children.contains_key(&(parent_pk, field.clone())) {
                typed[idx].set_m2m_resolved_json(field, Vec::new());
            }
        }
    }
    Ok(typed)
}

fn dedup_decode_pg<T: Model + HydrateRelated>(
    raw_rows: &[sqlx::postgres::PgRow],
    fk_join_fields: &[String],
    m2m_join_fields: &[String],
) -> Result<Vec<T>, sqlx::Error>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
{
    let pk_name = T::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.name)
        .ok_or_else(|| {
            sqlx::Error::Protocol(format!(
                "umbra::orm::join_related: model `{}` has no primary key, M2M JOIN \
                 dedup requires one",
                T::NAME
            ))
        })?;
    let registered = crate::migrate::registered_models();
    let mut typed: Vec<T> = Vec::new();
    let mut idx_by_pk: HashMap<i64, usize> = HashMap::new();
    let mut buckets: HashMap<(i64, String), Vec<JsonValue>> = HashMap::new();
    let mut seen_children: HashMap<(i64, String), std::collections::HashSet<i64>> = HashMap::new();
    for row in raw_rows {
        use sqlx::Row;
        let Ok(parent_pk) = row.try_get::<i64, _>(pk_name) else {
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(e) = idx_by_pk.entry(parent_pk) {
            let mut t = <T as sqlx::FromRow<_>>::from_row(row)?;
            backend_pg::hydrate_joined_rels::<T>(&mut t, row, fk_join_fields)?;
            e.insert(typed.len());
            typed.push(t);
        }
        for m2m_field in m2m_join_fields {
            let Some(rel) = T::M2M_RELATIONS
                .iter()
                .find(|r| r.field_name == m2m_field.as_str())
            else {
                continue;
            };
            let Some(child_meta) = registered.iter().find(|m| m.table == rel.target_table) else {
                continue;
            };
            let Some(child_json) = backend_pg::extract_m2m_child_json(row, m2m_field, child_meta)?
            else {
                continue;
            };
            let child_pk = child_json
                .as_object()
                .and_then(|m| {
                    let pk_col = child_meta.fields.iter().find(|c| c.primary_key)?;
                    m.get(&pk_col.name)?.as_i64()
                })
                .unwrap_or(0);
            let key = (parent_pk, m2m_field.clone());
            let seen = seen_children.entry(key.clone()).or_default();
            if seen.insert(child_pk) {
                buckets.entry(key).or_default().push(child_json);
            }
        }
    }
    for ((parent_pk, field), children) in buckets {
        if let Some(&idx) = idx_by_pk.get(&parent_pk) {
            typed[idx].set_m2m_resolved_json(&field, children);
        }
    }
    // Same LEFT JOIN miss zero-init as the SQLite path.
    for (&parent_pk, &idx) in idx_by_pk.iter() {
        for field in m2m_join_fields {
            if !seen_children.contains_key(&(parent_pk, field.clone())) {
                typed[idx].set_m2m_resolved_json(field, Vec::new());
            }
        }
    }
    Ok(typed)
}
