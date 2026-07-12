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
use umbral_casing::to_snake_case;

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

/// SQL join flavor recorded per `join_related` hop. `None` in a
/// `JoinReq` means "infer from FK nullability" (gap 4c); an explicit
/// `left_/inner_/right_join_related` records `Some(..)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
}

impl JoinKind {
    /// Lower to sea-query's join type.
    pub(crate) fn sea(self) -> sea_query::JoinType {
        match self {
            JoinKind::Inner => sea_query::JoinType::InnerJoin,
            JoinKind::Left => sea_query::JoinType::LeftJoin,
            JoinKind::Right => sea_query::JoinType::RightJoin,
        }
    }
}

/// One requested eager-join: a dotted relation path (`"plugin__author"`)
/// plus the join type to apply to the LAST hop. `kind: None` means
/// auto-infer per-hop from FK nullability (INNER for NOT NULL, LEFT for
/// nullable), the default. The explicit methods pin `Some(..)`.
#[derive(Debug, Clone)]
pub(crate) struct JoinReq {
    pub(crate) path: String,
    pub(crate) kind: Option<JoinKind>,
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
    /// BUG-8: `#[umbral(ordering = [...])]` lowers to a default ORDER
    /// BY applied at terminal time when the caller didn't supply an
    /// explicit `.order_by(...)`. The semantics: explicit calls REPLACE
    /// the default rather than appending to it.
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
    pub(crate) join_related: Vec<JoinReq>,
    /// Related-aggregate annotations added via
    /// [`Self::annotate_related`] / [`Self::annotate_count`]. Applied
    /// inside `build_query_for`, so EVERY terminal and introspection
    /// path — `fetch_annotated`, `explain`, `to_sql`, `to_sql_pg` —
    /// sees the same correlated subqueries. That's the
    /// `annotate()` contract: an annotation is query-builder state,
    /// not a side query.
    pub(crate) annotations: Vec<RelatedAnnotation>,
    /// gaps3 #29 — `ORDER BY <annotation alias>`. Kept separate from
    /// `order_by`, which is typed against the model's own columns and so cannot
    /// name a computed alias.
    pub(crate) annotation_order: Vec<(String, bool)>,
    /// audit_2 plugin-storage-tasks #6 — when `true`, a read terminal appends
    /// `FOR UPDATE SKIP LOCKED` (Postgres only). Lets N contending workers each
    /// claim a DIFFERENT row instead of all piling onto the same head row and
    /// serializing on its lock. A no-op on SQLite (no such clause; its
    /// single-writer model needs none). Set via [`Self::for_update_skip_locked`].
    pub(crate) for_update_skip_locked: bool,
    _phantom: PhantomData<T>,
}

// Manual `Clone` — NOT `#[derive(Clone)]`, because the derive would force a
// spurious `T: Clone` bound (every field is `T`-independent or carries its
// own `T`-free `Clone`: `Predicate<T>` has a manual `impl<T> Clone`, and
// `PhantomData<T>` clones for any `T`). The doc comment above already
// promised cheap cloning; this is what makes `Paginator` (and any
// requery-without-consume caller) slice the same query per page.
impl<T> Clone for QuerySet<T> {
    fn clone(&self) -> Self {
        Self {
            query: self.query.clone(),
            predicates: self.predicates.clone(),
            explicit_pool: self.explicit_pool.clone(),
            select_related: self.select_related.clone(),
            prefetch_related: self.prefetch_related.clone(),
            default_ordering: self.default_ordering.clone(),
            explicit_order: self.explicit_order,
            atomic: self.atomic,
            soft_delete_active: self.soft_delete_active,
            with_deleted: self.with_deleted,
            only_deleted: self.only_deleted,
            hard_delete: self.hard_delete,
            only_cols: self.only_cols.clone(),
            join_related: self.join_related.clone(),
            annotations: self.annotations.clone(),
            annotation_order: self.annotation_order.clone(),
            for_update_skip_locked: self.for_update_skip_locked,
            _phantom: PhantomData,
        }
    }
}

// Manual `Debug` for the same `T: Debug`-free reason; `Predicate<T>`'s own
// `Debug` is likewise `T`-free.
impl<T> std::fmt::Debug for QuerySet<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Predicate<T>` is intentionally not `Debug` (its `SimpleExpr`
        // carries no `T`), so report the predicate count rather than the
        // opaque expressions.
        f.debug_struct("QuerySet")
            .field("query", &self.query)
            .field(
                "predicates",
                &format_args!("[{} predicate(s)]", self.predicates.len()),
            )
            .field("explicit_pool", &self.explicit_pool)
            .field("select_related", &self.select_related)
            .field("prefetch_related", &self.prefetch_related)
            .field("default_ordering", &self.default_ordering)
            .field("explicit_order", &self.explicit_order)
            .field("atomic", &self.atomic)
            .field("soft_delete_active", &self.soft_delete_active)
            .field("with_deleted", &self.with_deleted)
            .field("only_deleted", &self.only_deleted)
            .field("hard_delete", &self.hard_delete)
            .field("only_cols", &self.only_cols)
            .field("join_related", &self.join_related)
            .field("annotations", &self.annotations)
            .finish()
    }
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
    /// Child model is `#[umbral(soft_delete)]` — fold
    /// `AND <child>.deleted_at IS NULL` into the correlated subquery so
    /// a trashed child stops inflating the parent's count.
    pub(crate) child_soft_delete: bool,
    /// Optional child-side predicate (a filtered count),
    /// pre-rendered to a backend-default `SimpleExpr`. ANDed into the
    /// subquery WHERE. From `annotate_count_where`.
    pub(crate) child_filter: Option<sea_query::SimpleExpr>,
    /// `Some(junction_table)` when this annotation counts M2M junction
    /// rows instead of child rows (`annotate_count` over an `M2M<T>`).
    pub(crate) m2m_junction: Option<String>,
}

/// Outcome of auto-discovering a reverse-FK relation (gaps2 #45) when
/// the parent declares no matching `ReverseSet` field. The resolver
/// scans the registry for children whose FK targets the parent table
/// and matches `relation` against their conventional name forms.
enum AutoDiscovery {
    /// Exactly one (child, fk_column) candidate matched.
    Resolved {
        child_table: String,
        fk_column: String,
        soft_delete: bool,
    },
    /// Two or more candidates matched — the caller must declare a
    /// `#[umbral(reverse_fk = "...")]` field to disambiguate. Carries
    /// the candidate `child.fk` labels for the error message.
    Ambiguous(Vec<String>),
    /// No candidate matched. Carries the list of auto-discoverable
    /// child names so the error can teach the available relations.
    NotFound(Vec<String>),
}

// `snake_case` replaced by `umbral_casing::to_snake_case` (imported above)
// in the gaps2 #77 consolidation refactor.

/// Scan the model registry for children whose FK targets `T::TABLE`,
/// and match `relation` against each candidate's conventional name
/// forms: the child's table name, the child's struct name in
/// snake_case and bare-lowercase, and any of those with a `_set`
/// suffix (the `<model>_set` form). Declared `REVERSE_FK_RELATIONS` /
/// `M2M_RELATIONS` are resolved by the caller BEFORE this runs, so
/// they always take precedence.
fn discover_reverse_relation<T: crate::orm::Model>(relation: &str) -> AutoDiscovery {
    if !crate::migrate::is_initialised() {
        return AutoDiscovery::NotFound(Vec::new());
    }
    let parent_table = T::TABLE;
    // Each candidate: (child_table, fk_column, child_soft_delete).
    let mut candidates: Vec<(String, String, bool)> = Vec::new();
    let mut discoverable: Vec<String> = Vec::new();
    for meta in crate::migrate::registered_models() {
        for col in &meta.fields {
            if col.fk_target.as_deref() != Some(parent_table) {
                continue;
            }
            // Conventional name forms this (child, fk_column) answers to.
            let snake = to_snake_case(&meta.name);
            let lower = meta.name.to_ascii_lowercase();
            let mut forms = vec![
                meta.table.clone(),
                snake.clone(),
                lower.clone(),
                format!("{}_set", meta.table),
                format!("{snake}_set"),
                format!("{lower}_set"),
            ];
            forms.sort();
            forms.dedup();
            // Surface a friendly name for "available children" errors.
            discoverable.push(format!("{}_set", meta.table));
            if forms.iter().any(|f| f == relation) {
                candidates.push((meta.table.clone(), col.name.clone(), meta.soft_delete));
            }
        }
    }
    match candidates.len() {
        1 => {
            let (child_table, fk_column, soft_delete) = candidates.pop().unwrap();
            AutoDiscovery::Resolved {
                child_table,
                fk_column,
                soft_delete,
            }
        }
        0 => {
            discoverable.sort();
            discoverable.dedup();
            AutoDiscovery::NotFound(discoverable)
        }
        _ => {
            let labels = candidates
                .into_iter()
                .map(|(child, fk, _)| format!("{child}.{fk}"))
                .collect();
            AutoDiscovery::Ambiguous(labels)
        }
    }
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
            annotation_order: Vec::new(),
            for_update_skip_locked: false,
            _phantom: PhantomData,
        }
    }

    /// Feature #72 — include soft-deleted rows in this query. Skips
    /// the auto `WHERE deleted_at IS NULL` injection. No-op on
    /// models that aren't tagged `#[umbral(soft_delete)]`.
    pub fn with_deleted(mut self) -> Self {
        self.with_deleted = true;
        self
    }

    /// Feature #72 — only soft-deleted rows. Useful for admin
    /// trash views and undelete workflows. No-op on models that
    /// aren't tagged `#[umbral(soft_delete)]`.
    pub fn only_deleted(mut self) -> Self {
        self.only_deleted = true;
        self
    }

    /// Feature #72 — force a real DELETE for the next `.delete()`
    /// terminal call. Soft-delete models normally rewrite delete()
    /// as `UPDATE ... SET deleted_at = NOW()`; `.hard_delete()`
    /// bypasses that for GDPR purges, test cleanup, or any other
    /// case where the row truly should be gone. No-op on models
    /// that aren't tagged `#[umbral(soft_delete)]` (their delete()
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
    /// Predicates the ORM adds on its own, independent of what the caller
    /// filtered on — today the soft-delete visibility guard.
    ///
    /// **This is the single seam for implicit filtering, and it exists so a
    /// write path cannot silently diverge from the read path.** Both
    /// [`Self::build_query_for`] (SELECT) and [`Self::build_update_for`]
    /// (UPDATE) call it, so a row hidden from reads cannot be updated through a
    /// predicate that forgot the same guard. It was duplicated in both before;
    /// two copies of a security-relevant filter is a drift bug waiting to
    /// happen, and the next filter to land here — the row-level tenant scope —
    /// must reach *every* builder or it is a cross-tenant leak
    /// (`docs/specs/row-level-tenancy.md`).
    ///
    /// Note `build_delete_for` deliberately does NOT call this: a hard delete
    /// may legitimately target already-trashed rows. A tenant predicate, by
    /// contrast, will have to apply there too.
    /// Snapshot the rows this queryset selects — the pre-image an audited write
    /// records (gaps3 #54). Empty (and free) unless the model is `audited`.
    async fn audit_pre(&self, backend: &str) -> Vec<serde_json::Map<String, JsonValue>>
    where
        T: crate::orm::Model,
    {
        let meta = crate::migrate::ModelMeta::for_::<T>();
        if !meta.audited {
            return Vec::new();
        }
        let mut conds: Vec<sea_query::Condition> = Vec::new();
        for p in &self.predicates {
            conds.push(sea_query::Condition::all().add(p.cond_for(backend)));
        }
        for e in self.implicit_predicates() {
            conds.push(sea_query::Condition::all().add(e));
        }
        crate::orm::dynamic::audit_snapshot(&meta, &conds).await
    }

    /// Record an audited write against the rows `ids` names.
    async fn audit_post(
        before: Vec<serde_json::Map<String, JsonValue>>,
        ids: &[JsonValue],
        action: &str,
    ) where
        T: crate::orm::Model,
    {
        let meta = crate::migrate::ModelMeta::for_::<T>();
        if !meta.audited {
            return;
        }
        // DELETE has no after-image; for CREATE/UPDATE re-read BY PK — re-running
        // the caller's filter would miss a row whose filtered column just changed.
        let after = if action == crate::orm::audit::DELETE {
            Vec::new()
        } else {
            match crate::orm::audit::pk_in_condition(&meta, ids) {
                Some(c) => crate::orm::dynamic::audit_snapshot(&meta, &[c]).await,
                None => Vec::new(),
            }
        };
        let pairs = if action == crate::orm::audit::CREATE {
            after
                .into_iter()
                .map(|a| (crate::orm::audit::pk_of(&meta, &a), None, Some(a)))
                .collect()
        } else {
            crate::orm::dynamic::audit_pairs(&meta, before, after)
        };
        crate::orm::audit::record_many(&meta, action, pairs).await;
    }

    fn implicit_predicates(&self) -> Vec<sea_query::SimpleExpr> {
        let mut out = Vec::new();
        if self.soft_delete_active {
            use sea_query::Expr;
            if self.only_deleted {
                out.push(Expr::col(Alias::new("deleted_at")).is_not_null());
            } else if !self.with_deleted {
                out.push(Expr::col(Alias::new("deleted_at")).is_null());
            }
        }
        out
    }

    pub(crate) fn build_query_for(&self, backend_name: &str) -> sea_query::SelectStatement {
        let mut q = self.query.clone();
        for p in &self.predicates {
            q.and_where(p.cond_for(backend_name));
        }
        // Feature #72 — soft-delete auto-filter. When the model
        // opted in via `#[umbral(soft_delete)]` AND the caller
        // didn't switch the visibility via `.with_deleted()` /
        // `.only_deleted()`, inject `WHERE deleted_at IS NULL`.
        // `.with_deleted()` shows everything; `.only_deleted()`
        // shows just the soft-deleted rows.
        for implicit in self.implicit_predicates() {
            q.and_where(implicit);
        }
        // Related-aggregate annotations: one correlated scalar
        // subquery per entry, aliased onto the SELECT list. Living
        // HERE is what makes `.annotate_*` compose with everything —
        // explain(), to_sql(), fetch_annotated() all see the same
        // query. Poisoned entries (unknown relation) are skipped in
        // this infallible path; the fallible consumers call
        // `check_annotations()` first and fail loudly instead.
        for ann in &self.annotations {
            // M2M-junction annotations count rows of the junction table
            // (`<parent>_<field>`, columns parent_id / child_id),
            // correlated on parent_id = <parent>.<pk>.
            if let Some(junction) = &ann.m2m_junction {
                if let Ok((_child_table, _fk_col, parent_table, parent_pk)) = &ann.resolved {
                    let mut sub = sea_query::Query::select();
                    sub.expr(ann.agg.to_simple_expr())
                        .from(crate::db::router::schema_qualified_table(junction.as_str()))
                        .and_where(
                            sea_query::Expr::col((
                                Alias::new(junction.as_str()),
                                Alias::new("parent_id"),
                            ))
                            .equals((
                                Alias::new(parent_table.as_str()),
                                Alias::new(parent_pk.as_str()),
                            )),
                        );
                    q.expr_as(
                        sea_query::SimpleExpr::SubQuery(
                            None,
                            Box::new(sea_query::SubQueryStatement::SelectStatement(
                                sub.to_owned(),
                            )),
                        ),
                        Alias::new(ann.alias.as_str()),
                    );
                }
                continue;
            }
            if let Ok((child_table, fk_col, parent_table, parent_pk)) = &ann.resolved {
                let mut sub = sea_query::Query::select();
                sub.expr(ann.agg.to_simple_expr())
                    .from(crate::db::router::schema_qualified_table(
                        child_table.as_str(),
                    ))
                    .and_where(
                        sea_query::Expr::col((
                            Alias::new(child_table.as_str()),
                            Alias::new(fk_col.as_str()),
                        ))
                        .equals((
                            Alias::new(parent_table.as_str()),
                            Alias::new(parent_pk.as_str()),
                        )),
                    );
                // Auto-exclude soft-deleted children from the count when
                // the child model is `#[umbral(soft_delete)]`.
                if ann.child_soft_delete {
                    sub.and_where(
                        sea_query::Expr::col((
                            Alias::new(child_table.as_str()),
                            Alias::new("deleted_at"),
                        ))
                        .is_null(),
                    );
                }
                // Child-side predicate (annotate_count_where).
                if let Some(filter) = &ann.child_filter {
                    sub.and_where(filter.clone());
                }
                q.expr_as(
                    sea_query::SimpleExpr::SubQuery(
                        None,
                        Box::new(sea_query::SubQueryStatement::SelectStatement(
                            sub.to_owned(),
                        )),
                    ),
                    Alias::new(ann.alias.as_str()),
                );
            }
        }
        // gaps3 #29: ORDER BY an annotation alias — "top scorers by goal count".
        // Both backends allow ordering by a SELECT-list alias, which is what the
        // annotation is. Applied before the model-default ordering so an explicit
        // annotation sort wins, exactly like `order_by` does.
        for (alias, desc) in &self.annotation_order {
            let order = if *desc { Order::Desc } else { Order::Asc };
            q.order_by(Alias::new(alias.as_str()), order);
        }
        // BUG-8: default ORDER BY applies only when the caller didn't
        // supply an explicit `.order_by(...)`: the model-default
        // ordering semantics.
        if !self.explicit_order {
            for (col, desc) in &self.default_ordering {
                let order = if *desc { Order::Desc } else { Order::Asc };
                q.order_by(Alias::new(*col), order);
            }
        }
        // audit_2 plugin-storage-tasks #6 — `FOR UPDATE SKIP LOCKED`, Postgres
        // ONLY. It lets concurrent claimers skip rows another txn already locked
        // (each grabs a different row) instead of all blocking on the head row's
        // lock. SQLite has no such clause and its single-writer model makes it
        // unnecessary, so it's a no-op there (never appended).
        if self.for_update_skip_locked && backend_name != "sqlite" {
            q.lock_with_behavior(
                sea_query::LockType::Update,
                sea_query::LockBehavior::SkipLocked,
            );
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
    /// The negated-filter terminal.
    pub fn exclude(self, p: Predicate<T>) -> Self {
        self.filter(crate::orm::Q::not(p))
    }

    /// Add an ORDER BY clause. Multiple `.order_by` calls append.
    /// The first explicit call also opts out of the model's
    /// `#[umbral(ordering = [...])]` default (BUG-8): explicit ordering
    /// replaces the default rather than stacking on top of it.
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

    /// Append `FOR UPDATE SKIP LOCKED` to a read terminal — **Postgres only**
    /// (a no-op on SQLite). Rows another transaction has already locked are
    /// skipped rather than blocked on, so N concurrent workers running the same
    /// `SELECT ... LIMIT k` each claim DIFFERENT rows instead of all contending
    /// for the same head row. The canonical use is a task/job queue's claim
    /// query (audit_2 plugin-storage-tasks #6): pair it with a conditional
    /// `UPDATE ... WHERE status = 'pending'` inside the same transaction.
    ///
    /// Must be used inside a transaction (`.on_tx(&mut tx)`) — the row locks a
    /// bare `SELECT` takes are released immediately, defeating the point.
    pub fn for_update_skip_locked(mut self) -> Self {
        self.for_update_skip_locked = true;
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
    /// umbral::db::transaction(|tx| async move {
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
    /// Multi-hop FK chains are supported: a `"__"`-separated path like
    /// `"author__manager"` resolves one JOIN per hop in a single query.
    ///
    /// **Constraints**: FK fields must live in `model.fields` (M2M links
    /// route through `prefetch_related`), and every related model along the
    /// chain must be registered with the framework
    /// (`App::builder().model::<U>()` or contributed by a plugin) so we can
    /// resolve its column layout for the aliased SELECT.
    pub fn join_related(mut self, field_name: impl Into<String>) -> Self {
        self.join_related.push(JoinReq {
            path: field_name.into(),
            kind: None,
        });
        self
    }

    /// Sugar for chained [`Self::join_related`] calls.
    pub fn join_related_many(mut self, field_names: &[&str]) -> Self {
        for name in field_names {
            self.join_related.push(JoinReq {
                path: (*name).to_string(),
                kind: None,
            });
        }
        self
    }

    /// `LEFT JOIN` the related path — keeps parent rows whose relation
    /// is absent (the relation hydrates as unresolved/None). Accepts a
    /// nested path (`"plugin__author"`); the join type applies to the
    /// deepest hop.
    pub fn left_join_related(mut self, path: impl Into<String>) -> Self {
        self.join_related.push(JoinReq {
            path: path.into(),
            kind: Some(JoinKind::Left),
        });
        self
    }

    /// `INNER JOIN` the related path — drops parent rows whose relation
    /// is absent. The default for a NOT NULL FK.
    pub fn inner_join_related(mut self, path: impl Into<String>) -> Self {
        self.join_related.push(JoinReq {
            path: path.into(),
            kind: Some(JoinKind::Inner),
        });
        self
    }

    /// `RIGHT JOIN` the related path. Postgres-unconditional; SQLite
    /// needs >= 3.39 — a runtime warning fires on older SQLite (see the
    /// boot/runtime note in the joins docs). The precise version gate
    /// lives at execute time (the SQLite driver's own error); the warn
    /// is the early nudge.
    pub fn right_join_related(mut self, path: impl Into<String>) -> Self {
        self.join_related.push(JoinReq {
            path: path.into(),
            kind: Some(JoinKind::Right),
        });
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
    /// same goal of killing N+1: a batch-loaded `prefetch_related('tags')`.
    ///
    /// ## Reverse-FK collections (post-#44)
    ///
    /// `prefetch_related` also loads `ReverseSet<C>` fields — the
    /// "for each Post, give me every Comment that points at it"
    /// shape. Declare the field on the parent with
    /// `#[sqlx(skip)] #[serde(skip)]
    /// #[umbral(reverse_fk = "<fk_col>")] pub <name>: ReverseSet<C>`
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
        // Nested path (`"plugin__author"` / `"tags__category"`):
        // validate via the hop resolver. A leading M2M segment is a
        // valid chain entry even though it isn't an FK on `T`, so
        // accept it and let `apply_join_related` route it.
        if field_name.contains("__") {
            let first = field_name.split("__").next().unwrap_or(field_name);
            let leads_m2m = T::M2M_RELATIONS.iter().any(|r| r.field_name == first);
            if leads_m2m || resolve_join_hops::<T>(field_name).is_some() {
                continue;
            }
            return Err(sqlx::Error::Protocol(format!(
                "umbral::orm::join_related: nested path `{field_name}` on `{}` has an \
                 unresolvable hop (each segment must be a FK or M2M to a registered model)",
                T::NAME
            )));
        }
        // Try as a regular column first.
        let col = T::FIELDS.iter().find(|f| f.name == field_name.as_str());
        if let Some(col) = col {
            if col.fk_target.is_some() {
                continue; // FK column — OK.
            }
            return Err(sqlx::Error::Protocol(format!(
                "umbral::orm::join_related: field `{field_name}` on `{}` is not a foreign \
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
            "umbral::orm::join_related: unknown field `{field_name}` on model `{}`",
            T::NAME
        )));
    }
    Ok(())
}

/// One resolved hop of a `join_related` FK chain.
#[derive(Debug, Clone)]
pub(crate) struct JoinHop {
    /// FK column name on the *previous* level's table.
    pub(crate) fk_col: String,
    /// Table this hop targets.
    pub(crate) child_table: String,
    /// PK column on `child_table`.
    pub(crate) child_pk: String,
    /// Was the FK column nullable? (drives auto-inference)
    pub(crate) nullable: bool,
}

/// Resolve a dotted FK path (`"plugin__author"`) into ordered hops.
/// Hop 0 reads `T::FIELDS`; deeper hops read the migrate registry's
/// `Column`s for the prior hop's target table. Returns `None` (skip,
/// emit no JOIN) on any unresolved hop — same forgiving posture as the
/// pre-existing one-hop path's silent skip in `to_sql`.
///
/// FK-only: a path whose FIRST segment is an M2M field is NOT handled
/// here (M2M chains route through `apply_join_related`'s M2M branch);
/// this returns `None` for such a path.
pub(crate) fn resolve_join_hops<T: Model>(path: &str) -> Option<Vec<JoinHop>> {
    let registered = crate::migrate::registered_models();
    let segs: Vec<&str> = path.split("__").filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    let mut hops = Vec::with_capacity(segs.len());
    // Hop 0 off the typed parent.
    let f0 = T::FIELDS.iter().find(|f| f.name == segs[0])?;
    let t0 = f0.fk_target?;
    let m0 = registered.iter().find(|m| m.table == t0)?;
    let pk0 = m0.fields.iter().find(|c| c.primary_key)?;
    hops.push(JoinHop {
        fk_col: segs[0].to_string(),
        child_table: t0.to_string(),
        child_pk: pk0.name.clone(),
        nullable: f0.nullable,
    });
    let mut current = t0;
    for seg in &segs[1..] {
        let meta = registered.iter().find(|m| m.table == current)?;
        let col = meta.fields.iter().find(|c| c.name == *seg)?;
        let tgt = col.fk_target.as_deref()?;
        let tmeta = registered.iter().find(|m| m.table == tgt)?;
        let pk = tmeta.fields.iter().find(|c| c.primary_key)?;
        hops.push(JoinHop {
            fk_col: (*seg).to_string(),
            child_table: tgt.to_string(),
            child_pk: pk.name.clone(),
            nullable: col.nullable,
        });
        current = tgt;
    }
    Some(hops)
}

/// Thin wrapper so the backend hydration helpers (a sibling module)
/// can resolve a chain without importing the private name directly.
pub(crate) fn resolve_join_hops_for<T: Model>(path: &str) -> Option<Vec<JoinHop>> {
    resolve_join_hops::<T>(path)
}

/// Resolve a path whose FIRST segment is an M2M field on `T` into the
/// M2M child table + child PK + the onward FK chain hops off that
/// child. `onward` is empty for a bare `"tags"`; for `"tags__category"`
/// it carries the `category` FK hop off the child table. `None` when
/// `segs[0]` isn't an M2M field or any onward hop fails to resolve.
pub(crate) fn resolve_m2m_chain<T: Model>(path: &str) -> Option<(String, String, Vec<JoinHop>)> {
    let registered = crate::migrate::registered_models();
    let segs: Vec<&str> = path.split("__").filter(|s| !s.is_empty()).collect();
    let first = segs.first()?;
    let rel = T::M2M_RELATIONS.iter().find(|r| r.field_name == *first)?;
    let child_meta = registered.iter().find(|m| m.table == rel.target_table)?;
    let child_pk = child_meta.fields.iter().find(|c| c.primary_key)?;
    let mut onward = Vec::with_capacity(segs.len().saturating_sub(1));
    let mut current = rel.target_table;
    for seg in &segs[1..] {
        let meta = registered.iter().find(|m| m.table == current)?;
        let col = meta.fields.iter().find(|c| c.name == *seg)?;
        let tgt = col.fk_target.as_deref()?;
        let tmeta = registered.iter().find(|m| m.table == tgt)?;
        let pk = tmeta.fields.iter().find(|c| c.primary_key)?;
        onward.push(JoinHop {
            fk_col: (*seg).to_string(),
            child_table: tgt.to_string(),
            child_pk: pk.name.clone(),
            nullable: col.nullable,
        });
        current = tgt;
    }
    Some((rel.target_table.to_string(), child_pk.name.clone(), onward))
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
        "umbral::orm::{terminal}: cannot run a typed terminal on a QuerySet \
         with `.only(...)` set — a partial-column row can't hydrate `T` via \
         FromRow. Either drop `.only(...)` to fetch full typed rows, or \
         terminate via `.values(&[...])` to get JSON rows with just the \
         projected columns."
    ))
}

fn resolve_pool<T: Model>(explicit: Option<DbPool>, op: crate::db::RouteOp) -> DbPool {
    if let Some(pool) = explicit {
        return pool;
    }
    // Route through the swappable router when the registry is up.
    if let Some(meta) = crate::migrate::model_meta_ref(T::NAME) {
        let ctx = crate::db::route_context::current();
        let r = crate::db::router::router();
        let alias = match op {
            crate::db::RouteOp::Read => r.db_for_read(meta, &ctx),
            crate::db::RouteOp::Write => r.db_for_write(meta, &ctx),
        };
        return crate::db::pool_for_dispatched(alias.as_str()).clone();
    }
    // Registry-less fallback (low-level tests): today's static behavior.
    if let Some(alias) = crate::migrate::model_alias(T::NAME) {
        return crate::db::pool_for_dispatched(&alias).clone();
    }
    crate::db::pool_dispatched().clone()
}

/// Pin a QuerySet to an explicit pool, dispatching SQLite vs Postgres. Used by
/// the upsert paths (`get_or_create` / `update_or_create`) so their
/// existence-check reads run on the WRITE database — read-your-writes, so a
/// read/write-split router never probes a lagging replica and inserts a
/// duplicate (or reads a stale row back after the update).
fn pin_to_pool<T: Model>(qs: QuerySet<T>, pool: &DbPool) -> QuerySet<T> {
    match pool {
        DbPool::Sqlite(p) => qs.on(p),
        DbPool::Postgres(p) => qs.on_pg(p),
    }
}

// GetError / TryForEachError moved to `errors`; re-exported above.

/// Emit a one-shot advisory when a `right_join_related` is applied
/// against a SQLite pool.
///
/// RIGHT/FULL JOIN landed in SQLite 3.39 (June 2022); Postgres has
/// always supported it. The boot system check (`check.rs`) can't surface
/// this — it's synchronous, has no live pool, and whether a RIGHT join
/// is *reachable* is a runtime QuerySet fact rather than static model
/// metadata. So the spec's "boot warning" is realized here: the first
/// time a RIGHT join is built against a SQLite pool, we `tracing::warn!`
/// once per process. We do NOT probe the library version (that needs an
/// async round-trip the SQL builder doesn't have) — the precise gate is
/// the SQLite driver's own error at execute time on an engine < 3.39;
/// this warn is the early nudge, consistent with `check.rs`'s
/// `Severity::Warning` posture.
///
/// Postgres pools and the no-pool case (a pure `to_sql` build with no
/// app booted) are silent.
fn warn_right_join_on_sqlite() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    if matches!(
        crate::db::try_pool_dispatched(),
        Some(crate::db::DbPool::Sqlite(_))
    ) {
        ONCE.call_once(|| {
            tracing::warn!(
                "umbral::orm::right_join_related: RIGHT JOIN requires SQLite >= 3.39. \
                 If your SQLite is older the query will error at execute time; \
                 Postgres is unaffected. Prefer left_/inner_join_related on SQLite \
                 unless you've confirmed the engine version."
            );
        });
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
    /// types aren't part of umbral's public surface); a `(sql, values)`
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
            for jr in &self.join_related {
                // Nested paths only need the FIRST hop's FK column at
                // the parent level; deeper hops join off the prior
                // level's alias, not the parent subquery.
                let join_field = jr.path.split("__").next().unwrap_or(jr.path.as_str());
                if parent_field_names.contains(join_field) {
                    needed.insert(join_field.to_string());
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
        // Set when any emitted hop is a RIGHT JOIN — drives the
        // once-per-process old-SQLite advisory after the emit loop.
        let mut emitted_right = false;
        for jr in &self.join_related {
            let field_name = &jr.path;
            // FK chain branch first. A (possibly nested) FK path splits
            // on `__` into ordered hops; each hop joins onto the prior
            // level's alias, and the DEEPEST hop's child columns are
            // aliased by the full dotted path so hydration can rebuild
            // the nested relation graph. The single-hop case is
            // byte-identical in child-column aliases to the pre-nesting
            // path (`<field>__<col>`); only the internal join alias
            // gains an `_h{idx}` suffix, which no test asserts.
            if let Some(hops) = resolve_join_hops::<T>(field_name) {
                let mut prev_alias = parent_alias.clone();
                let last = hops.len() - 1;
                // Cumulative dotted prefix per hop so EVERY level's own
                // columns ride along, aliased by its path-so-far. Hop 0
                // of `plugin__author` is `plugin`, hop 1 is
                // `plugin__author`. Selecting every level (not just the
                // leaf) is what lets hydration rebuild a FULL nested
                // object — the intermediate `plugin` row needs its own
                // `id`/`name` to deserialise into `ForeignKey<Plugin>`
                // before `author` nests inside it.
                let segs: Vec<&str> = field_name.split("__").collect();
                for (idx, hop) in hops.iter().enumerate() {
                    let hop_alias = Alias::new(format!("__j_{field_name}_h{idx}"));
                    // Last hop: explicit request, else infer from THIS
                    // hop's nullability. Intermediate hops always infer
                    // per-hop (an INNER can nest inside an outer
                    // LEFT etc.); an explicit kind only pins the leaf.
                    let kind = if idx == last {
                        jr.kind.unwrap_or(if hop.nullable {
                            JoinKind::Left
                        } else {
                            JoinKind::Inner
                        })
                    } else if hop.nullable {
                        JoinKind::Left
                    } else {
                        JoinKind::Inner
                    };
                    emitted_right |= kind == JoinKind::Right;
                    outer.join_as(
                        kind.sea(),
                        crate::db::router::schema_qualified_table(hop.child_table.as_str()),
                        hop_alias.clone(),
                        Expr::col((prev_alias.clone(), Alias::new(hop.fk_col.as_str())))
                            .equals((hop_alias.clone(), Alias::new(hop.child_pk.as_str()))),
                    );
                    if let Some(meta) = registered.iter().find(|m| m.table == hop.child_table) {
                        // Cumulative dotted prefix for this hop's columns.
                        let prefix = segs[..=idx].join("__");
                        for col in &meta.fields {
                            let alias = format!("{}__{}", prefix, col.name);
                            outer.expr_as(
                                Expr::col((hop_alias.clone(), Alias::new(col.name.as_str()))),
                                Alias::new(alias),
                            );
                        }
                    }
                    prev_alias = hop_alias;
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
            // The M2M field is the FIRST segment of the path; a nested
            // path like `"tags__category"` passes THROUGH the M2M hop
            // and continues with an onward FK chain off the child.
            let m2m_seg = field_name.split("__").next().unwrap_or(field_name.as_str());
            if let Some(m2m_rel) = T::M2M_RELATIONS.iter().find(|r| r.field_name == m2m_seg)
                && let Some(parent_pk) = T::FIELDS.iter().find(|f| f.primary_key)
                && let Some(child_meta) =
                    registered.iter().find(|m| m.table == m2m_rel.target_table)
                && let Some(child_pk) = child_meta.fields.iter().find(|c| c.primary_key)
            {
                // Junction table + aliases key off the M2M field name
                // (segs[0]), NOT the full dotted path.
                let junction_table = format!("{}_{}", T::TABLE, m2m_seg);
                let junction_alias = Alias::new(format!("__jm_{m2m_seg}"));
                let child_alias = Alias::new(format!("__j_{m2m_seg}"));
                // The junction hop stays LEFT so a parent with zero
                // junction rows isn't dropped by the join to the
                // junction table itself — the CHILD hop's kind is what
                // decides drop/keep. Plain `join_related` (kind None)
                // leaves the child LEFT too, preserving the shipped
                // double-LEFT-JOIN M2M behavior (a tag-less parent
                // survives with an empty M2M slot). An explicit
                // inner_join_related drops parents whose relation is
                // absent: the junction-LEFT miss yields a NULL child_id,
                // then the child INNER on NULL has no match -> the parent
                // is dropped, which is the INNER contract.
                let child_kind = jr.kind.unwrap_or(JoinKind::Left);
                emitted_right |= child_kind == JoinKind::Right;
                outer.join_as(
                    sea_query::JoinType::LeftJoin,
                    crate::db::router::schema_qualified_table(&junction_table),
                    junction_alias.clone(),
                    Expr::col((parent_alias.clone(), Alias::new(parent_pk.name)))
                        .equals((junction_alias.clone(), Alias::new("parent_id"))),
                );
                outer.join_as(
                    child_kind.sea(),
                    crate::db::router::schema_qualified_table(m2m_rel.target_table),
                    child_alias.clone(),
                    Expr::col((junction_alias.clone(), Alias::new("child_id")))
                        .equals((child_alias.clone(), Alias::new(child_pk.name.as_str()))),
                );
                // Child columns aliased by the M2M field name so the
                // M2M decode path (`<m2m_field>__<col>`) reads them.
                for col in &child_meta.fields {
                    let alias = format!("{}__{}", m2m_seg, col.name);
                    outer.expr_as(
                        Expr::col((child_alias.clone(), Alias::new(col.name.as_str()))),
                        Alias::new(alias),
                    );
                }
                // Onward FK chain off the child (segs[1..]). Each hop
                // joins onto the prior level's alias and aliases its
                // columns by the cumulative dotted path
                // (`tags__category__name`) so the M2M decode path can
                // nest the onward object into each child row.
                if let Some((_child_table, _child_pk, onward)) = resolve_m2m_chain::<T>(field_name)
                {
                    let segs: Vec<&str> = field_name.split("__").collect();
                    let mut prev_alias = child_alias.clone();
                    for (i, hop) in onward.iter().enumerate() {
                        // segs index for this hop: segs[0] is the M2M
                        // field, segs[1] is onward[0], etc.
                        let seg_idx = i + 1;
                        let hop_alias = Alias::new(format!("__j_{m2m_seg}_o{i}"));
                        let kind = if hop.nullable {
                            JoinKind::Left
                        } else {
                            JoinKind::Inner
                        };
                        outer.join_as(
                            kind.sea(),
                            crate::db::router::schema_qualified_table(hop.child_table.as_str()),
                            hop_alias.clone(),
                            Expr::col((prev_alias.clone(), Alias::new(hop.fk_col.as_str())))
                                .equals((hop_alias.clone(), Alias::new(hop.child_pk.as_str()))),
                        );
                        if let Some(meta) = registered.iter().find(|m| m.table == hop.child_table) {
                            let prefix = segs[..=seg_idx].join("__");
                            for col in &meta.fields {
                                let alias = format!("{}__{}", prefix, col.name);
                                outer.expr_as(
                                    Expr::col((hop_alias.clone(), Alias::new(col.name.as_str()))),
                                    Alias::new(alias),
                                );
                            }
                        }
                        prev_alias = hop_alias;
                    }
                }
                continue;
            }
        }
        // A RIGHT JOIN against SQLite needs >= 3.39; warn once per
        // process. Postgres / no-pool builds stay silent.
        if emitted_right {
            warn_right_join_on_sqlite();
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
        let join_reqs = self.join_related.clone();
        let join_fields: Vec<String> = join_reqs.iter().map(|j| j.path.clone()).collect();
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
        // A path whose FIRST segment is an M2M field routes to the M2M
        // group even when it continues with an onward FK chain
        // (`"tags__category"`): the junction double-join + parent dedup
        // live on the M2M side, and the onward FK nests into each child.
        let (m2m_join_fields, fk_join_fields): (Vec<String>, Vec<String>) =
            join_fields.iter().cloned().partition(|f| {
                let first = f.split("__").next().unwrap_or(f.as_str());
                T::M2M_RELATIONS.iter().any(|r| r.field_name == first)
            });
        let has_m2m_join = !m2m_join_fields.is_empty();

        let mut rows = match resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read)
        {
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
            let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
            hydrate_select_related::<T>(&mut rows, &sr_fields, &pool).await?;
        }
        if !prefetch_fields.is_empty() {
            let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
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
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
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
        // Review #2: delegate to `fetch()` with LIMIT 1 so select_related,
        // prefetch_related, AND join_related are all hydrated. `first()`
        // used to build a plain query and hydrate only select_related, so
        // `.prefetch_related("tags").first()` returned an unprefetched row
        // and `.join_related("author").first()` an unresolved join — both
        // silently. (For a to-many `join_related`, LIMIT 1 truncates the
        // joined children the same way `fetch()` does with `.limit(1)`;
        // prefer `prefetch_related` there.)
        self.query.limit(1);
        let rows = self.fetch().await?;
        Ok(rows.into_iter().next())
    }

    /// Return the row with the smallest value in `col_name`. Sugar
    /// for `order_by(col.asc()).first()`. The `earliest('created_at')`
    /// terminal.
    ///
    /// Takes a `&'static str` column name (same shape as
    /// `select_related`) so the call site stays terse:
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
    /// for `order_by(col.desc()).first()`. The `latest('created_at')`
    /// terminal.
    pub async fn latest(self, col_name: &'static str) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        self.order_by(OrderExpr::new(col_name, true)).first().await
    }

    /// Fetch many rows by their primary keys and return a
    /// `HashMap<T::PrimaryKey, T>` keyed by PK. The everyday companion to
    /// a cached list of ids — `User::objects().in_bulk(user_ids)` gives
    /// you direct lookup access without a second `.iter().find(...)` pass
    /// per id.
    ///
    /// Missing ids are silently absent from the map; callers that
    /// need the existence check can compare `map.len()` to
    /// `pks.len()`. Empty input is a no-op (returns the empty map).
    ///
    /// PK-agnostic (PK lift — was `Vec<i64>` / `HashMap<i64, T>`): the key
    /// is the model's `PrimaryKey` type, so i64-, String/slug-, and
    /// Uuid-keyed models all work. The map key requires `Hash + Eq`,
    /// which every standard PK type (integers, `String`, `Uuid`) satisfies.
    pub async fn in_bulk(
        self,
        pks: Vec<T::PrimaryKey>,
    ) -> Result<HashMap<T::PrimaryKey, T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
        T::PrimaryKey: std::hash::Hash + Eq,
    {
        if pks.is_empty() {
            return Ok(HashMap::new());
        }
        let pk_name = pk_field::<T>().map(|f| f.name).unwrap_or("id");
        // Each PK converts to a `sea_query::Value` (the `PrimaryKey` trait
        // bounds `Into<sea_query::Value>`); wrap as a `SimpleExpr` for the
        // IN-list so any PK shape binds correctly.
        let pk_pred: Predicate<T> = Predicate::new(
            Expr::col(Alias::new(pk_name)).is_in(
                pks.into_iter()
                    .map(|p| sea_query::SimpleExpr::Value(p.into())),
            ),
        );
        let rows = self.filter(pk_pred).fetch().await?;
        let mut out: HashMap<T::PrimaryKey, T> = HashMap::with_capacity(rows.len());
        for row in rows {
            out.insert(row.primary_key(), row);
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
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
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
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
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

    /// `.get()` — the exactly-one terminal.
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
    /// `T` instances. The columns-projection terminal: `values('id', 'title')`.
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
                        "umbral::orm::values: unknown column `{}` on model `{}`",
                        name,
                        T::NAME
                    ))
                })?;
            chosen.push(col);
        }
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
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
                    "umbral::orm::values: nested `{raw}` is not supported in v1 \
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
                    "umbral::orm::values: unknown column `{name}` on model `{}`",
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
                    "umbral::orm::values: unknown relation `{rel_name}` on model `{}` \
                     (used in `{rel_name}__...`)",
                    T::NAME
                )));
            };
            let Some(related_table) = fk_field.fk_target.as_deref() else {
                return Err(sqlx::Error::Protocol(format!(
                    "umbral::orm::values: field `{rel_name}` on `{}` is not a foreign \
                     key — `__` traversal only works through FK fields",
                    T::NAME
                )));
            };
            let Some(related_meta) = registered.iter().find(|m| m.table == related_table) else {
                return Err(sqlx::Error::Protocol(format!(
                    "umbral::orm::values: related model for table `{related_table}` \
                     is not registered"
                )));
            };
            let Some(related_pk) = related_meta.fields.iter().find(|c| c.primary_key) else {
                return Err(sqlx::Error::Protocol(format!(
                    "umbral::orm::values: related model `{related_table}` has no \
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
                            "umbral::orm::values: unknown child column `{name}` on \
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
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
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
                crate::db::router::schema_qualified_table(info.related_table),
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
                            backend_sqlite::joined_pk_is_null(row, &info.related_pk, &pk_alias);
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
                            backend_pg::joined_pk_is_null(row, &info.related_pk, &pk_alias);
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
    /// use umbral::orm::Aggregate;
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
                    "umbral::orm::aggregate: unknown column `{}` on model `{}` for aggregate `{}`",
                    col,
                    T::NAME,
                    name
                )));
            }
        }
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
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
    /// [`Self::annotate`], deserialized into a struct of your own (gaps3 #29).
    ///
    /// `annotate` returns `Vec<serde_json::Value>` — a GROUP BY rollup you then
    /// hand-decode. This gives you the rows typed:
    ///
    /// ```ignore
    /// #[derive(Deserialize)]
    /// struct ByAuthor { author_id: i64, posts: i64 }
    ///
    /// let rows: Vec<ByAuthor> = Post::objects()
    ///     .annotate_as::<ByAuthor>(&["author_id"], &[("posts", Aggregate::count())])
    ///     .await?;
    /// ```
    pub async fn annotate_as<R>(
        self,
        group_cols: &[&str],
        aggs: &[(&str, crate::orm::Aggregate)],
    ) -> Result<Vec<R>, sqlx::Error>
    where
        R: serde::de::DeserializeOwned,
    {
        let rows = self.annotate(group_cols, aggs).await?;
        rows.into_iter()
            .map(|v| {
                serde_json::from_value::<R>(v)
                    .map_err(|e| sqlx::Error::Protocol(format!("annotate_as: {e}")))
            })
            .collect()
    }

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
                        "umbral::orm::annotate: unknown group column `{}` on model `{}`",
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
                    "umbral::orm::annotate: unknown column `{}` on model `{}` for aggregate `{}`",
                    col,
                    T::NAME,
                    name
                )));
            }
        }
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
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

    /// The chainable `annotate(alias=Agg("relation"))`: attach a
    /// related-aggregate annotation to this QuerySet. The annotation
    /// is **query-builder state** — it renders as a correlated scalar
    /// subquery inside the one SELECT every terminal builds, so it
    /// composes with `.filter` / `.order_by` / `.limit`, stacks with
    /// further annotations, and shows up in [`Self::explain`] /
    /// [`Self::to_sql`] out of the box. Never a side query, never an
    /// N+1.
    ///
    /// `relation` names a `ReverseSet` relation on the model
    /// (`#[umbral(reverse_fk = "...")]`), the same names
    /// `prefetch_related` accepts. When no declared relation matches,
    /// the resolver AUTO-DISCOVERS the relation (gaps2 #45): it scans
    /// the model registry for any child whose FK targets this parent's
    /// table and matches `relation` against the child's conventional
    /// name forms (table name, `snake_case` / lowercase struct name,
    /// any of those with a `_set` suffix). Declared relations always
    /// take precedence; an ambiguous auto-match (two children, or a
    /// child with two FKs to this parent) poisons the annotation with
    /// an error that names the candidates and points at the
    /// `#[umbral(reverse_fk = "...")]` escape hatch. Any
    /// [`crate::orm::Aggregate`] works; non-count aggregates name a
    /// column on the CHILD model:
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
    /// child-side predicate (a filtered count)
    /// and child soft-delete awareness are tracked follow-ups
    /// (gaps2 #39).
    pub fn annotate_related(
        mut self,
        alias: &str,
        relation: &str,
        agg: crate::orm::Aggregate,
    ) -> Self {
        let rev_spec = T::REVERSE_FK_RELATIONS
            .iter()
            .find(|r| r.field_name == relation);
        let m2m_spec = T::M2M_RELATIONS.iter().find(|r| r.field_name == relation);

        let pk = T::FIELDS
            .iter()
            .find(|f| f.primary_key)
            .map(|f| f.name)
            .unwrap_or("id");

        let mut child_soft_delete = false;
        let mut m2m_junction: Option<String> = None;

        let resolved = if let Some(spec) = rev_spec {
            child_soft_delete = spec.soft_delete;
            Ok((
                spec.target_table.to_string(),
                spec.fk_column.to_string(),
                T::TABLE.to_string(),
                pk.to_string(),
            ))
        } else if let Some(spec) = m2m_spec {
            // M2M count: junction table = "<parent>_<field>", columns
            // parent_id / child_id. The subquery counts junction rows.
            m2m_junction = Some(format!("{}_{}", T::TABLE, spec.field_name));
            // child_table / fk_column are unused for the M2M shape, but
            // the tuple still carries parent_table + parent_pk for the
            // correlation in build_query_for.
            Ok((
                spec.target_table.to_string(),
                "child_id".to_string(),
                T::TABLE.to_string(),
                pk.to_string(),
            ))
        } else {
            // No DECLARED relation matched. Fall back to auto-discovery
            // (gaps2 #45): scan the model registry for any child whose
            // FK points back at this parent's table, and match `relation`
            // against the conventional name forms. Declared relations
            // always win above; this only runs as a fallback.
            match discover_reverse_relation::<T>(relation) {
                AutoDiscovery::Resolved {
                    child_table,
                    fk_column,
                    soft_delete,
                } => {
                    child_soft_delete = soft_delete;
                    Ok((child_table, fk_column, T::TABLE.to_string(), pk.to_string()))
                }
                AutoDiscovery::Ambiguous(candidates) => Err(format!(
                    "umbral::orm::annotate_related: ambiguous reverse relation `{relation}` on `{}` — candidates: [{}]; declare a `#[umbral(reverse_fk = \"<fk>\")] ReverseSet<Child>` field to disambiguate",
                    T::NAME,
                    candidates.join(", "),
                )),
                AutoDiscovery::NotFound(discoverable) => {
                    let declared = T::REVERSE_FK_RELATIONS
                        .iter()
                        .map(|r| r.field_name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let m2m = T::M2M_RELATIONS
                        .iter()
                        .map(|r| r.field_name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    Err(format!(
                        "umbral::orm::annotate_related: `{relation}` is not a reverse-FK or M2M relation on `{}` — reverse-FK relations: [{declared}], M2M relations: [{m2m}], auto-discoverable children: [{}]",
                        T::NAME,
                        discoverable.join(", "),
                    ))
                }
            }
        };

        self.annotations.push(RelatedAnnotation {
            alias: alias.to_string(),
            agg,
            resolved,
            child_soft_delete,
            child_filter: None,
            m2m_junction,
        });
        self
    }

    /// Sugar for the overwhelmingly common annotation:
    /// `annotate_related("<relation>_count", relation, Aggregate::count())`.
    /// `.annotate_count("comment_set")` exposes the value under the
    /// `comment_set_count` alias in [`Self::fetch_annotated`].
    /// `ORDER BY <annotation alias>` — sort by a value you annotated, not by a
    /// column (gaps3 #29).
    ///
    /// This is what a leaderboard needs and what the ORM could not express:
    /// `order_by` is typed against the model's own columns, so it cannot name a
    /// computed alias, and without this there was no way to ask for "the top 20
    /// authors *by post count*". A live consumer worked around it by pulling
    /// whole tables into a `HashMap` and sorting in Rust — the aggregation engine
    /// was already there, it just wasn't reachable.
    ///
    /// ```ignore
    /// AuthUser::objects()
    ///     .annotate_count("post_set")
    ///     .order_by_annotation("post_set__count", true)   // desc
    ///     .limit(20)
    ///     .fetch_annotated()
    ///     .await?
    /// ```
    ///
    /// An alias that was never annotated is a loud error at query time, not
    /// silently-wrong SQL — see [`Self::check_annotations`].
    pub fn order_by_annotation(mut self, alias: &str, desc: bool) -> Self {
        self.annotation_order.push((alias.to_string(), desc));
        self.explicit_order = true;
        self
    }

    pub fn annotate_count(self, relation: &str) -> Self {
        let alias = format!("{relation}_count");
        self.annotate_related(&alias, relation, crate::orm::Aggregate::count())
    }

    /// Like [`Self::annotate_count`] but counts only the children
    /// matching `pred` — a filtered count over `"comments"`.
    /// `C` is the CHILD model, so the predicate is typed against the
    /// child's columns (`comment::MODERATION.eq("visible")`). The
    /// predicate renders into the correlated count subquery's WHERE
    /// alongside the FK correlation and the auto soft-delete filter.
    ///
    /// ```rust,ignore
    /// Plugin::objects()
    ///     .annotate_count_where::<PluginComment>(
    ///         "visible_comments",
    ///         "comment_set",
    ///         plugin_comment::MODERATION.eq("visible"),
    ///     )
    /// ```
    pub fn annotate_count_where<C: crate::orm::Model>(
        self,
        alias: &str,
        relation: &str,
        pred: crate::orm::Predicate<C>,
    ) -> Self {
        // Render the child predicate to a backend-default SimpleExpr.
        // The count subquery embeds one expression; the equality /
        // comparison predicates used for child filters render the same
        // on both backends, so the default `cond` is correct.
        let child_filter = pred.cond_for("postgres");
        let mut queryset = self.annotate_related(alias, relation, crate::orm::Aggregate::count());
        // The just-pushed annotation is the last one; attach the filter.
        if let Some(last) = queryset.annotations.last_mut() {
            last.child_filter = Some(child_filter);
        }
        queryset
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
        // gaps3 #29: `order_by_annotation("typo")` must fail loudly here, not
        // emit SQL that orders by a column the database has never heard of (or,
        // worse, silently matches a real column and returns confidently wrong
        // rows).
        for (alias, _) in &self.annotation_order {
            if !self.annotations.iter().any(|a| &a.alias == alias) {
                let known: Vec<&str> = self.annotations.iter().map(|a| a.alias.as_str()).collect();
                return Err(sqlx::Error::Protocol(format!(
                    "order_by_annotation(\"{alias}\") names an annotation that was never added; \
                     annotated aliases on this queryset: {known:?}"
                )));
            }
        }
        Ok(())
    }

    /// [`Self::fetch_annotated`], deserialized into one struct of your own
    /// (gaps3 #29).
    ///
    /// `fetch_annotated` hands back `(T, Map<String, JsonValue>)`, so a handler
    /// that wants a flat row still writes `map["post_count"].as_i64().unwrap_or(0)`
    /// per field. That JSON-poking is plausibly *why* a live consumer skipped the
    /// aggregation engine entirely and rebuilt GROUP BY with `HashMap`s in Rust —
    /// the feature existed but wasn't reachable from a handler.
    ///
    /// The model's own columns and its annotations are merged into one object and
    /// deserialized, so the target struct just names what it wants:
    ///
    /// ```ignore
    /// #[derive(Deserialize)]
    /// struct Leader { id: i64, username: String, post_set__count: i64 }
    ///
    /// let top: Vec<Leader> = AuthUser::objects()
    ///     .annotate_count("post_set")
    ///     .order_by_annotation("post_set__count", true)
    ///     .limit(20)
    ///     .fetch_annotated_as::<Leader>()
    ///     .await?;
    /// ```
    ///
    /// A field the query didn't produce is a deserialize error, not a silent
    /// default — a typo in the struct should not read as "zero posts".
    pub async fn fetch_annotated_as<R>(self) -> Result<Vec<R>, sqlx::Error>
    where
        R: serde::de::DeserializeOwned,
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let rows = self.fetch_annotated().await?;
        let mut out = Vec::with_capacity(rows.len());
        for (model, annotations) in rows {
            let mut obj = match serde_json::to_value(&model) {
                Ok(JsonValue::Object(o)) => o,
                _ => serde_json::Map::new(),
            };
            // Annotations win on a name clash: you asked for the computed value.
            for (k, v) in annotations {
                obj.insert(k, v);
            }
            out.push(
                serde_json::from_value::<R>(JsonValue::Object(obj))
                    .map_err(|e| sqlx::Error::Protocol(format!("fetch_annotated_as: {e}")))?,
            );
        }
        Ok(out)
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

        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Read);
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
    /// Feature #72: for `#[umbral(soft_delete)]` models this rewrites
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
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Write);
        let backend = pool.backend_name();
        // gaps3 #54: the pre-image, read before the write lands. Free unless audited.
        let audit_before = self.audit_pre(backend).await;
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
            Self::audit_post(audit_before, &ids, crate::orm::audit::DELETE).await;

            // gaps3 #29: also emit the PER-ROW `post_delete:<table>`, not only the
            // bulk signal. `save()` and `update_or_create()` emit per-row signals;
            // `delete()` emitted only the bulk one, so `RealtimePlugin::on_model`
            // — which subscribes per-row — never saw a delete. A live consumer
            // hand-pushed a realtime event after every delete because of this.
            //
            // The payload is the primary key, not the whole row: the row is gone
            // by now, and re-reading it beforehand would cost a SELECT on every
            // delete in the app whether or not anything is listening. The id is
            // what a delete event is *for* — invalidate that row — and it is
            // exactly what realtime's default projection carries anyway.
            if let Some(pk) = pk_field::<T>() {
                for id in &ids {
                    let payload = serde_json::json!({ pk.name: id });
                    crate::signals::emit_post_delete_by_table(T::TABLE, payload).await;
                }
            }
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
    /// use umbral::orm::F;
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
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Write);
        let backend = pool.backend_name();
        // gaps3 #54: the pre-image, read before the write lands. Free unless audited.
        let audit_before = self.audit_pre(backend).await;

        let mut stmt = sea_query::Query::update();
        stmt.table(crate::db::router::schema_qualified_table(T::TABLE));
        stmt.value(Alias::new(field.name), expr.to_simple_expr());
        for p in &self.predicates {
            stmt.and_where(p.cond_for(backend));
        }
        if self.soft_delete_active {
            if self.only_deleted {
                stmt.and_where(Expr::col(Alias::new("deleted_at")).is_not_null());
            } else if !self.with_deleted {
                stmt.and_where(Expr::col(Alias::new("deleted_at")).is_null());
            }
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
            Self::audit_post(audit_before, &ids, crate::orm::audit::UPDATE).await;
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
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Write);
        let backend = pool.backend_name();
        // gaps3 #54: the pre-image, read before the write lands. Free unless audited.
        let audit_before = self.audit_pre(backend).await;
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
            Self::audit_post(audit_before, &ids, crate::orm::audit::UPDATE).await;
            crate::signals::emit_bulk_post_save::<T>(ids, false).await;
        }
        Ok(count)
    }

    /// Helper: build the DELETE statement for the active backend.
    /// Public-by-virtue-of-being-pub(crate) so the `_pg` and (future)
    /// `_sqlite` explicit-pool variants can share the SQL builder.
    fn build_delete_for(&self, backend_name: &str) -> sea_query::DeleteStatement {
        let mut stmt = Query::delete();
        stmt.from_table(crate::db::router::schema_qualified_table(T::TABLE));
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
        let pool = resolve_pool::<T>(self.explicit_pool.clone(), crate::db::RouteOp::Write);
        let backend = pool.backend_name();
        // gaps3 #54: the pre-image, read before the write lands. Free unless audited.
        let audit_before = self.audit_pre(backend).await;
        let now = chrono::Utc::now();
        let mut stmt = sea_query::Query::update();
        stmt.table(crate::db::router::schema_qualified_table(T::TABLE));
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
        // The cascade must find the children BEFORE the parent is stamped: it
        // locates them through the parent's still-matching live-rows predicate
        // (same predicates + `deleted_at IS NULL` guard as the UPDATE above).
        let meta = crate::migrate::ModelMeta::for_::<T>();
        let cascade_sel = pk.map(|pkf| {
            let mut sel = sea_query::Query::select();
            sel.column(Alias::new(pkf.name))
                .from(crate::db::router::schema_qualified_table(T::TABLE));
            for p in &self.predicates {
                sel.and_where(p.cond_for(backend));
            }
            sel.and_where(sea_query::Expr::col(Alias::new("deleted_at")).is_null());
            sel
        });

        // Parent + cascade in ONE transaction, ALWAYS (gaps3 #53) — not gated on
        // `should_atomic_wrap`. A half-applied cascade leaves exactly the orphaned
        // live children this fix exists to prevent.
        let ids: Vec<JsonValue> = match pool {
            DbPool::Sqlite(pool) => {
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let mut tx = pool.begin().await?;
                if let Some(sel) = cascade_sel {
                    let mut conn = crate::orm::soft_delete_cascade::CascadeConn::Sqlite(&mut tx);
                    crate::orm::soft_delete_cascade::cascade_soft_delete(
                        &mut conn, &meta, sel, now,
                    )
                    .await?;
                }
                let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .fetch_all(&mut *tx)
                    .await?;
                tx.commit().await?;
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
                let mut tx = pool.begin().await?;
                if let Some(sel) = cascade_sel {
                    let mut conn = crate::orm::soft_delete_cascade::CascadeConn::Pg(&mut tx);
                    crate::orm::soft_delete_cascade::cascade_soft_delete(
                        &mut conn, &meta, sel, now,
                    )
                    .await?;
                }
                let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .fetch_all(&mut *tx)
                    .await?;
                tx.commit().await?;
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
            Self::audit_post(audit_before, &ids, crate::orm::audit::DELETE).await;
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
        stmt.table(crate::db::router::schema_qualified_table(T::TABLE));
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
        for implicit in self.implicit_predicates() {
            stmt.and_where(implicit);
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
            .from(crate::db::router::schema_qualified_table(T::TABLE))
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

    /// A bare [`QuerySet`] over every row — the `Model::objects().all()` form.
    ///
    /// The entry point when you need a `QuerySet` terminal without a
    /// filter: a grouped aggregate over the whole table, an unfiltered
    /// `aggregate`, or just an explicit "all rows" for readability.
    pub fn all(&self) -> QuerySet<T> {
        self.queryset()
    }

    /// See [`QuerySet::aggregate`] — single-row aggregate over every row.
    ///
    /// Forwards from the manager so `Model::objects().aggregate(...)`
    /// works without an intervening `.filter(...)` / `.on(...)`.
    pub async fn aggregate(
        &self,
        aggs: &[(&str, crate::orm::Aggregate)],
    ) -> Result<JsonValue, sqlx::Error> {
        self.queryset().aggregate(aggs).await
    }

    /// See [`QuerySet::annotate`] — grouped aggregate (`GROUP BY <group_cols>`).
    ///
    /// Forwards from the manager so the documented
    /// `Model::objects().annotate(&["status"], &[("count", Aggregate::count())])`
    /// (a grouped count over `"status"`) compiles
    /// directly, without a filter first.
    pub async fn annotate(
        &self,
        group_cols: &[&str],
        aggs: &[(&str, crate::orm::Aggregate)],
    ) -> Result<Vec<JsonValue>, sqlx::Error> {
        self.queryset().annotate(group_cols, aggs).await
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

    /// See [`QuerySet::left_join_related`].
    pub fn left_join_related(&self, path: impl Into<String>) -> QuerySet<T> {
        self.queryset().left_join_related(path)
    }

    /// See [`QuerySet::inner_join_related`].
    pub fn inner_join_related(&self, path: impl Into<String>) -> QuerySet<T> {
        self.queryset().inner_join_related(path)
    }

    /// See [`QuerySet::right_join_related`].
    pub fn right_join_related(&self, path: impl Into<String>) -> QuerySet<T> {
        self.queryset().right_join_related(path)
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

    /// See [`QuerySet::order_by_annotation`].
    pub fn order_by_annotation(&self, alias: &str, desc: bool) -> QuerySet<T> {
        self.queryset().order_by_annotation(alias, desc)
    }

    /// See [`QuerySet::annotate_as`] — the typed GROUP BY rollup.
    pub async fn annotate_as<R>(
        &self,
        group_cols: &[&str],
        aggs: &[(&str, crate::orm::Aggregate)],
    ) -> Result<Vec<R>, sqlx::Error>
    where
        R: serde::de::DeserializeOwned,
    {
        self.queryset().annotate_as::<R>(group_cols, aggs).await
    }

    /// See [`QuerySet::annotate_count_where`] — starts a filtered
    /// annotated chain from the manager.
    pub fn annotate_count_where<C: crate::orm::Model>(
        &self,
        alias: &str,
        relation: &str,
        pred: crate::orm::Predicate<C>,
    ) -> QuerySet<T> {
        self.queryset()
            .annotate_count_where::<C>(alias, relation, pred)
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
    /// The one-liner: `User::objects().get(user::ID.eq(1))`.
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

        let pool = resolve_pool::<T>(None, crate::db::RouteOp::Write);
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
                // gaps3 #29: `create()` emitted NOTHING, while `save()` and
                // `update_or_create()` both emit per-row `post_save`. So every
                // `.create()` was invisible to signals — and therefore to
                // `RealtimePlugin::on_model`, which is why a live consumer
                // hand-pushed a realtime event after all 13 of its writes.
                crate::signals::emit_post_save::<T>(&row, true).await;
                // gaps3 #54: the created row IS the after-image.
                crate::orm::audit::record(
                    &crate::migrate::ModelMeta::for_::<T>(),
                    &serde_json::to_value(&row)
                        .ok()
                        .and_then(|v| {
                            v.as_object().map(|o| {
                                crate::orm::audit::pk_of(&crate::migrate::ModelMeta::for_::<T>(), o)
                            })
                        })
                        .unwrap_or_default(),
                    crate::orm::audit::CREATE,
                    None,
                    serde_json::to_value(&row)
                        .ok()
                        .and_then(|v| v.as_object().cloned())
                        .as_ref(),
                )
                .await;
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
                // gaps3 #29: `create()` emitted NOTHING, while `save()` and
                // `update_or_create()` both emit per-row `post_save`. So every
                // `.create()` was invisible to signals — and therefore to
                // `RealtimePlugin::on_model`, which is why a live consumer
                // hand-pushed a realtime event after all 13 of its writes.
                crate::signals::emit_post_save::<T>(&row, true).await;
                // gaps3 #54: the created row IS the after-image.
                crate::orm::audit::record(
                    &crate::migrate::ModelMeta::for_::<T>(),
                    &serde_json::to_value(&row)
                        .ok()
                        .and_then(|v| {
                            v.as_object().map(|o| {
                                crate::orm::audit::pk_of(&crate::migrate::ModelMeta::for_::<T>(), o)
                            })
                        })
                        .unwrap_or_default(),
                    crate::orm::audit::CREATE,
                    None,
                    serde_json::to_value(&row)
                        .ok()
                        .and_then(|v| v.as_object().cloned())
                        .as_ref(),
                )
                .await;
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
        let pool = resolve_pool::<T>(None, crate::db::RouteOp::Write);
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
            QuerySet::<T>::audit_post(Vec::new(), &ids, crate::orm::audit::CREATE).await;
            crate::signals::emit_bulk_post_save::<T>(ids, true).await;
        }
        Ok(count)
    }

    /// The `get_or_create` terminal: fetch the first row matching `predicate`;
    /// if none exists, insert `defaults` and return it. Returns
    /// `(row, created)` so the caller can branch on whether the write
    /// happened. Two queries on the miss path (filter+first then create),
    /// one query on the hit path.
    ///
    /// ## Concurrency
    ///
    /// Convergent under concurrent callers: if two callers both miss the
    /// SELECT and race to INSERT, the one that loses gets a
    /// `UniqueViolation`; that error is caught here and the existing row
    /// is re-fetched, so both callers return the same row with
    /// `created = false` for the loser. A UNIQUE constraint on the
    /// predicate columns is required for true at-most-one semantics — the
    /// constraint is what makes the convergence deterministic.
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
        use crate::orm::write::WriteError;

        // Read-your-writes: probe for the existing row on the WRITE database,
        // not a (possibly lagging) read replica — otherwise a read/write-split
        // router could miss a just-written row and insert a duplicate. The
        // following `create()` already resolves the same write target.
        let write_pool = resolve_pool::<T>(None, crate::db::RouteOp::Write);
        if let Some(existing) = pin_to_pool(self.filter(predicate.clone()), &write_pool)
            .first()
            .await
            .map_err(WriteError::Sqlx)?
        {
            return Ok((existing, false));
        }

        // Attempt the INSERT. On a UNIQUE violation (concurrent writer won the
        // race between our SELECT and this INSERT), catch the error and
        // re-SELECT to return the now-existing row with created=false.
        // This is the standard try-insert-then-fetch convergence pattern.
        //
        // Note on transaction semantics: a plain INSERT is atomic at the
        // statement level. Wrapping SELECT+INSERT in a serialisable transaction
        // would be stricter but requires SAVEPOINT support to recover from the
        // Postgres "aborted transaction" state after a constraint violation —
        // a per-operation SAVEPOINT would add two extra round-trips on every
        // write for marginal gain. The UNIQUE-constraint backstop plus this
        // re-fetch gives the same observable guarantee: callers always converge
        // on the same row and never see a spurious UniqueViolation.
        match self.create(defaults).await {
            Ok(created) => Ok((created, true)),
            Err(WriteError::UniqueViolation { .. }) => {
                // A concurrent writer inserted the row between our SELECT and
                // our INSERT. Re-fetch the now-existing row.
                let existing = pin_to_pool(self.filter(predicate), &write_pool)
                    .first()
                    .await
                    .map_err(WriteError::Sqlx)?
                    .ok_or_else(|| {
                        WriteError::Sqlx(sqlx::Error::Protocol(
                            "get_or_create: row vanished after UniqueViolation re-fetch"
                                .to_string(),
                        ))
                    })?;
                Ok((existing, false))
            }
            Err(e) => Err(e),
        }
    }

    /// The `update_or_create` terminal: fetch the first row matching
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
    /// ## Concurrency
    ///
    /// Convergent under concurrent callers: if two callers both miss the
    /// SELECT and race to INSERT, the loser gets a `UniqueViolation`; that
    /// error is caught here and the existing row is re-fetched, then the
    /// update is applied to it. Both callers converge on the same row, with
    /// `created = false` for the loser. A UNIQUE constraint on the predicate
    /// columns is required for deterministic convergence.
    ///
    /// Implementation: 2 queries on the hit path (`first` + UPDATE + re-fetch),
    /// 2 queries on the miss+create path (`first` + `create`), or
    /// 4 queries on the miss+race path (`first` + failed-INSERT + `first`
    /// + UPDATE + re-fetch).
    pub async fn update_or_create(
        &self,
        predicate: Predicate<T>,
        defaults: T,
    ) -> Result<(T, bool), crate::orm::write::WriteError>
    where
        T: serde::Serialize
            + Clone
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

        // Read-your-writes: the existence probe and the post-update re-fetch
        // run on the WRITE database, so a read/write-split router doesn't miss
        // a just-written row (duplicate insert) or read a stale row back. The
        // intervening UPDATE already routes to the same write target.
        let write_pool = resolve_pool::<T>(None, crate::db::RouteOp::Write);

        // Shared helper: given the existing row (already fetched) and the
        // defaults instance, apply the non-PK column update and return the
        // re-fetched row. Used by both the direct-hit path and the
        // UniqueViolation-convergence path so the update logic is in one place.
        macro_rules! do_update {
            ($existing:expr, $defaults:expr) => {{
                let existing: T = $existing;
                let defaults: T = $defaults;

                // Serialize defaults, drop the PK so the matched row's PK is
                // preserved, then UPDATE WHERE <pk_col> = <existing_pk>.
                let mut update_map = serialize_to_map(&defaults)?;
                update_map.remove(pk_name);

                // Build a PK predicate from the existing row's serialized PK
                // value. Goes through serde_json so any PK type (i64, String,
                // Uuid) round-trips correctly through sea-query.
                let existing_json =
                    serde_json::to_value(&existing).map_err(WriteError::SerializeFailed)?;
                let pk_value_json = existing_json
                    .get(pk_name)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let pk_sea = crate::orm::write::json_to_sea_value(
                    pk.ty,
                    &pk_value_json,
                    false,
                    pk_name,
                    None,
                )?;
                let pk_pred: Predicate<T> =
                    Predicate::new(sea_query::Expr::col(sea_query::Alias::new(pk_name)).eq(pk_sea));

                // Run the UPDATE.
                self.filter(pk_pred).update_values(update_map).await?;

                // Re-fetch to return the populated row.
                let pk_sea2 = crate::orm::write::json_to_sea_value(
                    pk.ty,
                    &pk_value_json,
                    false,
                    pk_name,
                    None,
                )?;
                let refetch_pred: Predicate<T> = Predicate::new(
                    sea_query::Expr::col(sea_query::Alias::new(pk_name)).eq(pk_sea2),
                );
                let updated_row: T = pin_to_pool(self.filter(refetch_pred), &write_pool)
                    .first()
                    .await
                    .map_err(WriteError::Sqlx)?
                    .ok_or_else(|| {
                        WriteError::Sqlx(sqlx::Error::Protocol(
                            "update_or_create: row vanished between UPDATE and re-fetch"
                                .to_string(),
                        ))
                    })?;

                // `update_values` above fires `bulk_post_save`; ALSO fire the
                // per-row `post_save` so signal / realtime `on_model` consumers
                // (which subscribe to the per-row event) see this upsert-update.
                // Without it, the CREATE branch (via `self.create()`) emits
                // `post_save` but the UPDATE branch was silent to those
                // consumers — an asymmetry that's very hard to reason about
                // from the call site (gaps3 #14). `bulk_post_save` and
                // `post_save` are distinct signal names, so no single consumer
                // fires twice.
                crate::signals::emit_post_save::<T>(&updated_row, false).await;
                updated_row
            }};
        }

        if let Some(existing) = pin_to_pool(self.filter(predicate.clone()), &write_pool)
            .first()
            .await
            .map_err(WriteError::Sqlx)?
        {
            let updated = do_update!(existing, defaults);
            return Ok((updated, false));
        }

        // Attempt the INSERT. On a UNIQUE violation (concurrent writer won the
        // race between our SELECT and this INSERT), catch the error, re-fetch
        // the now-existing row, and apply the update to it — same convergence
        // as get_or_create but with an extra UPDATE step.
        match self.create(defaults.clone()).await {
            Ok(created) => {
                // `create()` now fires `post_save` and records the audit row
                // itself (gaps3 #29), so this branch must NOT do either again —
                // it delegates to `create()` above. Before #29, `create()` was
                // signal-free and gaps3 #14 patched the gap here; that patch is
                // now a double-emit (and, since gaps3 #54, a double audit row).
                Ok((created, true))
            }
            Err(WriteError::UniqueViolation { .. }) => {
                // A concurrent writer inserted the row between our SELECT and
                // our INSERT. Re-fetch then update, same as the direct-hit path.
                let existing = pin_to_pool(self.filter(predicate), &write_pool)
                    .first()
                    .await
                    .map_err(WriteError::Sqlx)?
                    .ok_or_else(|| {
                        WriteError::Sqlx(sqlx::Error::Protocol(
                            "update_or_create: row vanished after UniqueViolation re-fetch"
                                .to_string(),
                        ))
                    })?;
                let updated = do_update!(existing, defaults);
                Ok((updated, false))
            }
            Err(e) => Err(e),
        }
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
        let pool = resolve_pool::<T>(None, crate::db::RouteOp::Write);
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
    /// A `bulk_update(objs, fields)` that
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
        stmt.table(crate::db::router::schema_qualified_table(T::TABLE));

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

        let pool = resolve_pool::<T>(None, crate::db::RouteOp::Write);
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
        // Routing: `raw` resolves to the READ database (most raw statements
        // are SELECTs, which this returns as `Vec<T>`). Under a read/write-
        // split router, a raw statement that WRITES must pin the write pool
        // explicitly via `.on(&pool)` / `.on_pg(&pool)`, since the router
        // cannot inspect arbitrary SQL to know it mutates.
        let pool = resolve_pool::<T>(None, crate::db::RouteOp::Read);
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
    /// umbral::db::transaction(|tx| async move {
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
    /// umbral::db::transaction(|tx| async move {
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
    // `QuerySet::delete`) remain signal-free by design:
    // bulk operations bypass signals for performance.
    //
    // Signal name format: `<event>:<table>` — e.g. `post_save:post`.
    // Payload shapes:
    //   save:   `{ "instance": <M as JSON>, "created": bool }`
    //   delete: `{ "instance": <M as JSON> }`
    //
    // The `created` flag on save follows the convention:
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

        let pool = resolve_pool::<T>(None, crate::db::RouteOp::Write);
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
            crate::orm::audit::record(
                &crate::migrate::ModelMeta::for_::<T>(),
                &serde_json::to_value(&row)
                    .ok()
                    .and_then(|v| {
                        v.as_object().map(|o| {
                            crate::orm::audit::pk_of(&crate::migrate::ModelMeta::for_::<T>(), o)
                        })
                    })
                    .unwrap_or_default(),
                crate::orm::audit::CREATE,
                None,
                serde_json::to_value(&row)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .as_ref(),
            )
            .await;
            crate::signals::emit_post_save::<T>(&row, true).await;
            Ok(row)
        } else {
            // UPDATE path: UPDATE ... WHERE <pk> = <value> RETURNING *.
            use sea_query::{Alias, Expr, Query};

            // gaps2 #92 — snapshot the pre-UPDATE row for `pre_update` /
            // `post_update` subscribers, but ONLY when one exists. The
            // extra SELECT-by-PK is gated on `has_subscribers` so the
            // common UPDATE path (no `*_update` listener) pays nothing.
            // Best-effort TOCTOU: the snapshot reads the row before the
            // UPDATE; a concurrent writer between the two is accepted.
            let pre_table = T::TABLE;
            let want_pre = crate::signals::has_subscribers(&format!("pre_update:{pre_table}"));
            let want_post = crate::signals::has_subscribers(&format!("post_update:{pre_table}"));
            let previous: Option<T> = if want_pre || want_post {
                let mut sel = Query::select();
                sel.from(crate::db::router::schema_qualified_table(T::TABLE));
                for field in T::FIELDS {
                    sel.column(Alias::new(field.name));
                }
                let pk_sea_sel = crate::orm::write::json_to_sea_value(
                    pk_field.ty,
                    &pk_val,
                    false,
                    pk_field.name,
                    None,
                )
                .map_err(SaveError::Write)?;
                sel.and_where(Expr::col(Alias::new(pk_field.name)).eq(pk_sea_sel));
                sel.limit(1);
                match pool {
                    DbPool::Sqlite(ref pool) => {
                        let (sql, values) = sel.build_sqlx(SqliteQueryBuilder);
                        sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                            .fetch_optional(pool)
                            .await
                            .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                    }
                    DbPool::Postgres(ref pool) => {
                        let (sql, values) = sel.build_sqlx(PostgresQueryBuilder);
                        sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                            .fetch_optional(pool)
                            .await
                            .map_err(|e| SaveError::Write(crate::orm::write::WriteError::Sqlx(e)))?
                    }
                }
            } else {
                None
            };
            // Fire pre_update before the UPDATE when both a snapshot exists
            // and a subscriber wants it.
            if want_pre {
                if let Some(prev) = &previous {
                    crate::signals::emit_pre_update::<T>(prev, &instance).await;
                }
            }

            let mut stmt = Query::update();
            stmt.table(crate::db::router::schema_qualified_table(T::TABLE));
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
            // gaps2 #92 — post_update carries the pre-UPDATE snapshot (old)
            // and the freshly-written row (new). Only when a subscriber
            // exists and the snapshot was captured.
            if want_post {
                if let Some(prev) = &previous {
                    crate::signals::emit_post_update::<T>(prev, &row).await;
                }
            }
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
        // `#[umbral(soft_delete)]`, set deleted_at instead of issuing
        // DELETE. Pre/post_delete signals still fire because the
        // logical contract ("this row is gone from the visible
        // table") is preserved — only the physical SQL changed.
        // Hard-delete is not exposed through delete_instance (it's
        // a typed per-row helper); call `QuerySet::filter(pk =
        // instance.id).hard_delete().delete()` when you need it.
        let stmt_sql = if T::SOFT_DELETE {
            let now = chrono::Utc::now();
            let mut up = Query::update();
            up.table(crate::db::router::schema_qualified_table(T::TABLE));
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
            stmt.from_table(crate::db::router::schema_qualified_table(T::TABLE));
            stmt.and_where(Expr::col(Alias::new(pk_field.name)).eq(pk_sea));
            SoftOrHardStatement::Delete(stmt)
        };

        let pool = resolve_pool::<T>(None, crate::db::RouteOp::Write);
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
    // PK-agnostic dedup: read the parent PK back through the shape-aware
    // decoder (using the parent model's PK SqlType) and key by `pk_key`,
    // so i64 / String / Uuid parents all dedup correctly.
    let parent_pk_col = crate::migrate::ModelMeta::for_::<T>()
        .fields
        .into_iter()
        .find(|c| c.primary_key)
        .ok_or_else(|| {
            sqlx::Error::Protocol(format!(
                "umbral::orm::join_related: model `{}` has no primary key, M2M JOIN \
                 dedup requires one",
                T::NAME
            ))
        })?;
    let registered = crate::migrate::registered_models();
    let mut typed: Vec<T> = Vec::new();
    let mut idx_by_pk: HashMap<String, usize> = HashMap::new();
    // (parent_pk_key, field) → Vec<JsonValue> + a Set of seen child PK keys.
    let mut buckets: HashMap<(String, String), Vec<JsonValue>> = HashMap::new();
    let mut seen_children: HashMap<(String, String), std::collections::HashSet<String>> =
        HashMap::new();
    for row in raw_rows {
        let Ok(parent_json) = crate::orm::dynamic::decode_to_json(row, &parent_pk_col) else {
            continue;
        };
        let parent_key = crate::orm::pk_key(&parent_json);
        if let std::collections::hash_map::Entry::Vacant(e) = idx_by_pk.entry(parent_key.clone()) {
            let mut t = <T as sqlx::FromRow<_>>::from_row(row)?;
            backend_sqlite::hydrate_joined_rels::<T>(&mut t, row, fk_join_fields)?;
            e.insert(typed.len());
            typed.push(t);
        }
        for m2m_field in m2m_join_fields {
            // The M2M slot is keyed by the FIRST segment (the M2M field
            // name); the full `m2m_field` path may carry an onward FK
            // chain (`"tags__category"`) the child decoder nests.
            let m2m_seg = m2m_field.split("__").next().unwrap_or(m2m_field.as_str());
            let Some(rel) = T::M2M_RELATIONS.iter().find(|r| r.field_name == m2m_seg) else {
                continue;
            };
            let Some(child_meta) = registered.iter().find(|m| m.table == rel.target_table) else {
                continue;
            };
            let Some(child_json) =
                backend_sqlite::extract_m2m_child_json::<T>(row, m2m_field, child_meta)?
            else {
                continue;
            };
            // Dedup by child PK (PK-agnostic) so multi-M2M cartesian
            // doesn't duplicate this field's children.
            let child_key = child_json
                .as_object()
                .and_then(|m| {
                    let pk_col = child_meta.fields.iter().find(|c| c.primary_key)?;
                    m.get(&pk_col.name).map(crate::orm::pk_key)
                })
                .unwrap_or_default();
            let key = (parent_key.clone(), m2m_seg.to_string());
            let seen = seen_children.entry(key.clone()).or_default();
            if seen.insert(child_key) {
                buckets.entry(key).or_default().push(child_json);
            }
        }
    }
    for ((parent_key, field), children) in buckets {
        if let Some(&idx) = idx_by_pk.get(&parent_key) {
            typed[idx].set_m2m_resolved_json(&field, children);
        }
    }
    // LEFT JOIN miss handling: walk every (parent, field) pair
    // we expected to populate and zero-init any slot that never
    // got a hit. Without this a parent with no matching M2M
    // children would leave its slot None — distinguishable from
    // "loaded, empty" only by callers checking the absence.
    for (parent_key, &idx) in idx_by_pk.iter() {
        for field in m2m_join_fields {
            let seg = field.split("__").next().unwrap_or(field.as_str());
            if !seen_children.contains_key(&(parent_key.clone(), seg.to_string())) {
                typed[idx].set_m2m_resolved_json(seg, Vec::new());
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
    // PK-agnostic dedup — see the SQLite variant.
    let parent_pk_col = crate::migrate::ModelMeta::for_::<T>()
        .fields
        .into_iter()
        .find(|c| c.primary_key)
        .ok_or_else(|| {
            sqlx::Error::Protocol(format!(
                "umbral::orm::join_related: model `{}` has no primary key, M2M JOIN \
                 dedup requires one",
                T::NAME
            ))
        })?;
    let registered = crate::migrate::registered_models();
    let mut typed: Vec<T> = Vec::new();
    let mut idx_by_pk: HashMap<String, usize> = HashMap::new();
    let mut buckets: HashMap<(String, String), Vec<JsonValue>> = HashMap::new();
    let mut seen_children: HashMap<(String, String), std::collections::HashSet<String>> =
        HashMap::new();
    for row in raw_rows {
        let Ok(parent_json) = crate::orm::dynamic::decode_pg_to_json(row, &parent_pk_col) else {
            continue;
        };
        let parent_key = crate::orm::pk_key(&parent_json);
        if let std::collections::hash_map::Entry::Vacant(e) = idx_by_pk.entry(parent_key.clone()) {
            let mut t = <T as sqlx::FromRow<_>>::from_row(row)?;
            backend_pg::hydrate_joined_rels::<T>(&mut t, row, fk_join_fields)?;
            e.insert(typed.len());
            typed.push(t);
        }
        for m2m_field in m2m_join_fields {
            let m2m_seg = m2m_field.split("__").next().unwrap_or(m2m_field.as_str());
            let Some(rel) = T::M2M_RELATIONS.iter().find(|r| r.field_name == m2m_seg) else {
                continue;
            };
            let Some(child_meta) = registered.iter().find(|m| m.table == rel.target_table) else {
                continue;
            };
            let Some(child_json) =
                backend_pg::extract_m2m_child_json::<T>(row, m2m_field, child_meta)?
            else {
                continue;
            };
            let child_key = child_json
                .as_object()
                .and_then(|m| {
                    let pk_col = child_meta.fields.iter().find(|c| c.primary_key)?;
                    m.get(&pk_col.name).map(crate::orm::pk_key)
                })
                .unwrap_or_default();
            let key = (parent_key.clone(), m2m_seg.to_string());
            let seen = seen_children.entry(key.clone()).or_default();
            if seen.insert(child_key) {
                buckets.entry(key).or_default().push(child_json);
            }
        }
    }
    for ((parent_key, field), children) in buckets {
        if let Some(&idx) = idx_by_pk.get(&parent_key) {
            typed[idx].set_m2m_resolved_json(&field, children);
        }
    }
    // Same LEFT JOIN miss zero-init as the SQLite path.
    for (parent_key, &idx) in idx_by_pk.iter() {
        for field in m2m_join_fields {
            let seg = field.split("__").next().unwrap_or(field.as_str());
            if !seen_children.contains_key(&(parent_key.clone(), seg.to_string())) {
                typed[idx].set_m2m_resolved_json(seg, Vec::new());
            }
        }
    }
    Ok(typed)
}
