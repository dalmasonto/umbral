//! All-to-one multi-hop relation resolution (Phase 1, Task 2).
//!
//! Task 1 resolved a *single* forward-FK hop through a subquery. This module
//! resolves a **deep** to-one chain — `Post.author → Author.company →
//! Company.owner` — in ONE flat query:
//!
//! ```sql
//! SELECT <leaf.*>
//! FROM   <root>            AS __rel_0
//! JOIN   <hop1.to_table>   AS __rel_1 ON …
//! JOIN   <hop2.to_table>   AS __rel_2 ON …
//! JOIN   <leaf>            AS __rel_N ON …
//! WHERE  __rel_0.<root_pk> = ?
//! LIMIT  1
//! ```
//!
//! Every JOIN is an INNER JOIN, which gives the right shape for both return
//! flavors: a NULL (or dangling) link anywhere along the chain drops the row,
//! so `resolve` returns `Ok(None)` — the caller's `get_opt()` sees `None`, and
//! `get()` turns that absence into [`sqlx::Error::RowNotFound`]. The two join
//! *directions* are driven by [`HopSpec::fk_on_from`]:
//!
//! - `fk_on_from = true` (forward FK / forward O2O): the FK column lives on the
//!   NEAR table and points at the FAR table's PK — `ON near.<fk> = far.<pk>`.
//! - `fk_on_from = false` (reverse O2O, parent side): the FK column lives on
//!   the FAR table and points back at the NEAR row's PK — `ON near.<pk> =
//!   far.<fk>`.
//!
//! All SQL is built with sea-query; nothing here hand-rolls a SQL string.
//!
//! See `docs/specs/orm-relation-traversal.md`.

// The JOIN walk is unified across every hop kind (forward FK/O2O, reverse
// O2O, reverse-FK, M2M — see `walk_joins` below) AND across every consumer:
// `build_to_one_select` (deep to-one), `build_prefix_pivot_subquery` (the
// to-one prefix of a crossing-to-many chain), and — as of the
// heavy-relations epic's Task 445 — `apply_join_related`
// (`queryset/mod.rs`, the `join_related`/`select_related` JOIN path) all
// call this single `walk_joins` helper. No JOIN-builder duplication
// remains; `resolve_join_hops`/`resolve_m2m_chain` (`queryset/mod.rs`)
// still exist, but only to resolve a path's hop TABLES for
// `backend_sqlite`/`backend_pg`'s post-fetch column decode — a job
// unrelated to how the JOIN SQL itself is built.

use sea_query::{
    Alias, Expr, JoinType, PostgresQueryBuilder, Query, SelectStatement, SimpleExpr,
    SqliteQueryBuilder,
};
use sea_query_binder::SqlxBinder;

use crate::db::DbPool;
use crate::migrate::{ModelMeta, registered_models_opt};
use crate::orm::queryset::{Manager, QuerySet};
use crate::orm::relation::{HopKind, HopSpec, NullJoinPolicy, PathBase, RelPath};
use crate::orm::{Model, Predicate};

/// Per-level table alias (`__rel_0` is the root, `__rel_1` the first hop's
/// target, …). Distinct from `join_related`'s `__j_*` aliases so the two
/// JOIN builders never clash if one path ever nests inside the other.
fn level_alias(level: usize) -> Alias {
    Alias::new(format!("__rel_{level}"))
}

/// The primary-key column name of a registered table, looked up the same way
/// [`super::resolve_join_hops`] does — from the migrate registry.
pub(crate) fn pk_of<'a>(registered: &'a [ModelMeta], table: &str) -> Option<&'a str> {
    registered
        .iter()
        .find(|m| m.table == table)?
        .fields
        .iter()
        .find(|c| c.primary_key)
        .map(|c| c.name.as_str())
}

/// Walk a hop chain of ANY [`HopKind`], appending one JOIN (two, for an
/// `M2M` hop — parent-to-junction, junction-to-target) per hop onto `select`
/// and returning the final (leaf-most) table alias.
///
/// The single JOIN builder shared by every hop→SQL consumer:
/// [`build_to_one_select`] (the deep to-one resolver) and
/// [`build_prefix_pivot_subquery`] (the to-one prefix of a crossing-to-many
/// chain) today; `apply_join_related` (`queryset/mod.rs`) in a later task.
/// The caller must already have added the root table to `select` under
/// `root_alias`; the caller owns the final projection and `WHERE` afterward.
///
/// Join direction per hop:
/// - `fk_on_from = true` (forward FK / O2O): `near.<fk> = far.<pk>`.
/// - `fk_on_from = false` (reverse O2O parent-side, or reverse-FK): the FK
///   column lives on the FAR table — `near.<pk> = far.<fk>`.
/// - `M2M`: two joins through [`HopSpec::junction`] —
///   `near.<pk> = junction.<parent_column>` then
///   `junction.<target_column> = far.<pk>`.
///
/// `policy` decides the JOIN TYPE per hop: [`NullJoinPolicy::Inner`] always
/// INNER JOINs (a NULL/absent link drops the row — traversal's shape);
/// [`NullJoinPolicy::LeftForNullable`] LEFT JOINs when `!hop.required` (keep
/// the parent row even when the link is absent — hydration's shape);
/// [`NullJoinPolicy::Right`] unconditionally RIGHT JOINs, ignoring
/// `hop.required` — `apply_join_related`'s `.right_join_related(...)`
/// override, applied one hop at a time (never the whole chain at once).
///
/// EVERY joined table (intermediate targets AND the M2M junction) is routed
/// through [`crate::db::router::schema_qualified_table`], so the intent is
/// that the multi-tenant isolation guarantee holds for every hop kind, not
/// just the forward case. Today only the forward-FK/O2O and reverse-O2O
/// arms are exercised end-to-end under an installed schema router
/// (`walk_joins_schema_qualified.rs`); the `M2M` and `ReverseFk` arms'
/// qualification is correct BY INSPECTION only — no current caller reaches
/// them through `RelPath::from_path` far enough to build a schema-qualified
/// M2M/reverse-FK JOIN and execute it. The heavy-relations epic's aggregate
/// consumer (Plan A/B Task 3) is expected to be the first caller that does;
/// that's where those two arms' schema-qualification should get their own
/// executed proof.
// TODO(orm-heavy-relations): once the aggregate consumer (epic Task 3)
// drives an M2M/reverse-FK hop through `walk_joins` under a schema router,
// add a `walk_joins_schema_qualified.rs` case for it alongside the existing
// forward-FK/O2O one.
pub(crate) fn walk_joins(
    select: &mut SelectStatement,
    root_alias: Alias,
    hops: &[HopSpec],
    policy: NullJoinPolicy,
    prefix: &str,
    registered: &[ModelMeta],
) -> Result<Alias, sqlx::Error> {
    let mut near_alias = root_alias;
    for (idx, hop) in hops.iter().enumerate() {
        let far_alias = Alias::new(format!("{prefix}{}", idx + 1));
        let jt = match policy {
            NullJoinPolicy::LeftForNullable if !hop.required => JoinType::LeftJoin,
            NullJoinPolicy::Right => JoinType::RightJoin,
            _ => JoinType::InnerJoin,
        };
        match hop.kind {
            HopKind::M2M => {
                let junction = hop
                    .junction
                    .ok_or_else(|| protocol_error("M2M hop is missing its JunctionSpec"))?;
                let near_pk = pk_of(registered, hop.from_table).ok_or_else(|| {
                    protocol_error(&format!(
                        "cannot resolve primary key of `{}` (is the model registered?)",
                        hop.from_table
                    ))
                })?;
                let far_pk = pk_of(registered, hop.to_table).ok_or_else(|| {
                    protocol_error(&format!(
                        "cannot resolve primary key of `{}` (is the model registered?)",
                        hop.to_table
                    ))
                })?;
                let junction_alias = Alias::new(format!("{prefix}j{idx}"));
                select.join_as(
                    jt,
                    crate::db::router::schema_qualified_table(junction.table),
                    junction_alias.clone(),
                    Expr::col((near_alias.clone(), Alias::new(near_pk)))
                        .equals((junction_alias.clone(), Alias::new(junction.parent_column))),
                );
                select.join_as(
                    jt,
                    crate::db::router::schema_qualified_table(hop.to_table),
                    far_alias.clone(),
                    Expr::col((junction_alias, Alias::new(junction.target_column)))
                        .equals((far_alias.clone(), Alias::new(far_pk))),
                );
            }
            HopKind::Fk | HopKind::O2OForward | HopKind::O2OReverse | HopKind::ReverseFk => {
                let on = if hop.fk_on_from {
                    // Forward FK / O2O: FK column on the NEAR table -> FAR pk.
                    let far_pk = pk_of(registered, hop.to_table).ok_or_else(|| {
                        protocol_error(&format!(
                            "cannot resolve primary key of `{}` (is the model registered?)",
                            hop.to_table
                        ))
                    })?;
                    Expr::col((near_alias.clone(), Alias::new(hop.fk_column)))
                        .equals((far_alias.clone(), Alias::new(far_pk)))
                } else {
                    // Reverse O2O (parent side) / reverse-FK: FK column on the
                    // FAR table -> NEAR pk.
                    let near_pk = pk_of(registered, hop.from_table).ok_or_else(|| {
                        protocol_error(&format!(
                            "cannot resolve primary key of `{}` (is the model registered?)",
                            hop.from_table
                        ))
                    })?;
                    Expr::col((near_alias.clone(), Alias::new(near_pk)))
                        .equals((far_alias.clone(), Alias::new(hop.fk_column)))
                };
                select.join_as(
                    jt,
                    crate::db::router::schema_qualified_table(hop.to_table),
                    far_alias.clone(),
                    on,
                );
            }
        }
        near_alias = far_alias;
    }
    Ok(near_alias)
}

/// Build the single flat `SELECT <leaf.*> FROM <root> JOIN …` statement for
/// an all-to-one [`RelPath`] — `WHERE root.pk = ? LIMIT 1` anchored when
/// `path.base` is [`PathBase::SinglePk`] (the object-rooted `Relation<T>`
/// terminal shape), or a bare table-rooted JOIN with no `WHERE`/`LIMIT` when
/// it is [`PathBase::TableRoot`] (the [`RelPath::from_path`] shape a later
/// `select_related`/aggregate consumer hangs its own projection/predicates
/// off of — every root row, not one).
///
/// Errors (loudly, never a wrong query) when the path has no hops, when a hop
/// is to-many (M2M / reverse-FK belong to a later task), or when an
/// intermediate table's PK can't be resolved from the registry.
pub(crate) fn build_to_one_select<Leaf: Model>(
    path: &RelPath,
) -> Result<SelectStatement, sqlx::Error> {
    if path.hops.is_empty() {
        return Err(protocol_error(
            "relation path has no hops — nothing to resolve",
        ));
    }
    if !path.hops.iter().all(|h| h.kind.is_to_one()) {
        return Err(protocol_error(
            "a to-many hop (M2M / reverse-FK) cannot resolve to a single row \
             via the all-to-one JOIN resolver; those widen to a QuerySet in a \
             later task",
        ));
    }

    // Pre-boot-safe: a >=2-hop chain needs the registry for intermediate PKs.
    // Use the non-panicking accessor so a deep path resolved with `.on(&pool)`
    // and no booted App surfaces as a clean `Err`, matching this module's
    // "errors loudly, never a wrong query / panic" contract.
    let registered = registered_models_opt().ok_or_else(|| {
        protocol_error(
            "no model registry available to resolve a multi-hop relation — build \
             an App (which registers models) before resolving a deep chain, or \
             use a single-hop relation (which needs no registry)",
        )
    })?;

    let mut q = Query::select();
    let root_alias = level_alias(0);
    let root_table: &str = match &path.base {
        PathBase::SinglePk { table, .. } => table,
        PathBase::TableRoot { table } => table,
    };
    q.from_as(
        crate::db::router::schema_qualified_table(root_table),
        root_alias.clone(),
    );

    // Walk the hops, joining each target onto the previous level's alias.
    let near_alias = walk_joins(
        &mut q,
        root_alias.clone(),
        &path.hops,
        NullJoinPolicy::Inner,
        "__rel_",
        &registered,
    )?;

    // Project the leaf's own columns, aliased to their bare names so `Leaf`'s
    // `FromRow` reads them by field name regardless of the JOIN aliasing.
    for f in Leaf::FIELDS {
        q.expr_as(
            Expr::col((near_alias.clone(), Alias::new(f.name))),
            Alias::new(f.name),
        );
    }

    // `SinglePk` anchors at the one root row and wants exactly it back;
    // `TableRoot` has no single row to anchor on — it enumerates every root
    // row the JOIN chain reaches (the later `select_related`/aggregate
    // consumer adds its own predicates/limit on top of this base query).
    if let PathBase::SinglePk {
        pk_column,
        pk_value,
        ..
    } = &path.base
    {
        q.and_where(
            Expr::col((root_alias, Alias::new(*pk_column))).eq(SimpleExpr::Value(pk_value.clone())),
        );
        q.limit(1);
    }

    Ok(q)
}

/// Render the resolver's SQL for the given backend — the probe surface behind
/// `Relation::to_sql`. Returns the SQLite string by default (the required
/// tested backend); pass `is_postgres = true` for the Postgres builder.
pub(crate) fn to_one_sql<Leaf: Model>(
    path: &RelPath,
    is_postgres: bool,
) -> Result<String, sqlx::Error> {
    let stmt = build_to_one_select::<Leaf>(path)?;
    let sql = if is_postgres {
        stmt.to_string(PostgresQueryBuilder)
    } else {
        stmt.to_string(SqliteQueryBuilder)
    };
    Ok(sql)
}

/// Resolve an all-to-one [`RelPath`] against `pool`, decoding the leaf row via
/// its `FromRow` impl. `Ok(None)` when any link in the chain is NULL / absent.
pub(crate) async fn resolve_to_one_path<Leaf>(
    path: &RelPath,
    pool: &DbPool,
) -> Result<Option<Leaf>, sqlx::Error>
where
    Leaf: Model
        + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
        + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
{
    let stmt = build_to_one_select::<Leaf>(path)?;
    match pool {
        DbPool::Sqlite(p) => {
            let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
            sqlx::query_as_with::<sqlx::Sqlite, Leaf, _>(&sql, values)
                .fetch_optional(p)
                .await
        }
        DbPool::Postgres(p) => {
            let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
            sqlx::query_as_with::<sqlx::Postgres, Leaf, _>(&sql, values)
                .fetch_optional(p)
                .await
        }
    }
}

/// A loud protocol error for an unsupported / unresolvable path shape.
fn protocol_error(msg: &str) -> sqlx::Error {
    sqlx::Error::Protocol(msg.to_string())
}

// =========================================================================
// Phase 1, Task 4 — crossing-to-many leaf resolution.
// =========================================================================

/// The primary-key column name of a `Model` from its `FIELDS`. Falls back to
/// `"id"` for the (derive-impossible) no-PK case, matching `relation.rs`.
fn leaf_pk_col<M: Model>() -> &'static str {
    M::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.name)
        .unwrap_or("id")
}

/// Resolve a [`RelPath`] that crosses a to-many hop into a chainable
/// `QuerySet<Leaf>` whose base query expresses the whole traversal as
/// junction JOINs rooted at the starting PK.
///
/// # Supported shapes (Phase 1)
///
/// An all-to-one prefix (0+ forward-FK / O2O hops) followed by one or more
/// to-many hops (M2M, or a single reverse-FK as the leaf hop) to the leaf:
///
/// ```text
/// SELECT [DISTINCT] <leaf.*>
/// FROM   <leaf>
/// JOIN   <junction_k> ON junction_k.child = leaf.pk
/// JOIN   <junction_{k-1}> ON junction_{k-1}.child = junction_k.parent
/// …
/// WHERE  <outermost link> = <pivot pk>          -- pivot = root pk, or an
///                                               -- IN (…) subquery over the
///                                               -- to-one prefix
/// ```
///
/// The leaf is the query root (its own table, unaliased) so a later
/// `.filter(leaf::COL.eq(…))` binds unambiguously to the leaf — every other
/// table in the query is either a junction (only `parent`/`child` id columns)
/// or lives inside a subquery, so no data column collides with the leaf's.
/// `SELECT DISTINCT` on the leaf columns dedupes a leaf reachable via
/// multiple paths (spec decision 5); `.with_duplicates()` drops it.
///
/// # Deferred shapes (returned POISONED, fail loudly at the terminal)
///
/// - A to-one hop AFTER a to-many hop (the per-row-JOIN fan-out the spec
///   defers past Phase 1).
/// - A reverse-FK hop that is not the leaf hop (reading its FK column would
///   force an intermediate data table into the FROM, risking predicate
///   ambiguity) — deferred.
///
/// Infallible (returns a `QuerySet`, never `Result`) to match
/// [`crate::orm::relation::to_many_hop`]'s builder contract: an unsupported
/// or unresolvable shape becomes a **poisoned** `QuerySet` that errors at
/// every fallible terminal instead of running a wrong query.
pub(crate) fn resolve_leaf_queryset<Leaf: Model>(path: &RelPath) -> QuerySet<Leaf> {
    match build_leaf_select::<Leaf>(path) {
        Ok((query, leaf_pk_col)) => {
            let mut qs = QuerySet::new(query);
            qs.default_ordering = Leaf::ORDERING.to_vec();
            qs.soft_delete_active = Leaf::SOFT_DELETE;
            qs.leaf_distinct = Some((Leaf::TABLE.to_string(), leaf_pk_col));
            qs
        }
        Err(msg) => Manager::<Leaf>::new()
            .filter(Predicate::new(Expr::cust("1 = 1")))
            .poisoned(msg),
    }
}

/// Build the leaf `SELECT` for a crossing-to-many [`RelPath`]. Returns the
/// statement plus the leaf PK column name (for the `COUNT(DISTINCT pk)` path).
/// `Err(msg)` for an unsupported / unresolvable shape — the caller poisons.
fn build_leaf_select<Leaf: Model>(path: &RelPath) -> Result<(SelectStatement, String), String> {
    if path.hops.is_empty() {
        return Err("relation path has no hops — nothing to resolve".to_string());
    }
    // Split into the all-to-one prefix and the to-many segment.
    let first_to_many = path
        .hops
        .iter()
        .position(|h| !h.kind.is_to_one())
        .ok_or_else(|| {
            "an all-to-one path resolves to a single row via the to-one resolver, \
             not the crossing-to-many leaf resolver"
                .to_string()
        })?;
    let prefix = &path.hops[..first_to_many];
    let to_many = &path.hops[first_to_many..];

    // Every hop in the to-many segment must be to-many; a to-one hop AFTER a
    // to-many is the deferred per-row-JOIN fan-out.
    if to_many.iter().any(|h| h.kind.is_to_one()) {
        return Err(
            "a to-one hop after a to-many hop (per-row JOIN fan-out) is not \
                    supported in Phase 1 — this deep to-many chain shape is deferred \
                    (docs/specs/orm-relation-traversal.md, crossing semantics)"
                .to_string(),
        );
    }
    // A reverse-FK hop is only supported as the leaf (last) hop; an inner
    // reverse-FK would force its child data table into the FROM.
    for (i, h) in to_many.iter().enumerate() {
        if h.kind == HopKind::ReverseFk && i != to_many.len() - 1 {
            return Err(
                "an inner reverse-FK hop in a deep to-many chain is not supported \
                        in Phase 1 (only a reverse-FK as the final leaf hop is) — this \
                        deep to-many chain shape is deferred"
                    .to_string(),
            );
        }
    }

    let registered = registered_models_opt().ok_or_else(|| {
        "no model registry available to resolve a deep relation — build an App \
         (which registers models) before resolving a crossing-to-many chain"
            .to_string()
    })?;

    let (base_table, base_pk_column, pk_value) = match &path.base {
        PathBase::SinglePk {
            table,
            pk_column,
            pk_value,
        } => (table, pk_column, pk_value),
        PathBase::TableRoot { .. } => {
            return Err("TableRoot base has no pk to anchor a single-object traversal".to_string());
        }
    };

    let leaf_table = Leaf::TABLE;
    let leaf_pk = leaf_pk_col::<Leaf>();

    let mut q = Query::select();
    q.from(crate::db::router::schema_qualified_table(leaf_table));
    // Project the leaf's own columns (bare names) so `Leaf`'s `FromRow` reads
    // them; never pull a junction's id columns into the row.
    for f in Leaf::FIELDS {
        q.expr_as(
            Expr::col((Alias::new(leaf_table), Alias::new(f.name))),
            Alias::new(f.name),
        );
    }

    // Backward walk the to-many segment (leaf-adjacent hop first). `link_expr`
    // is the PK expression the current hop's child side must equal; it starts
    // as the leaf PK and, after each hop, becomes the parent-side PK the NEXT
    // (inner) hop connects to.
    let mut link_expr: Expr = Expr::col((Alias::new(leaf_table), Alias::new(leaf_pk)));
    for (rev_i, hop) in to_many.iter().enumerate().rev() {
        match hop.kind {
            HopKind::M2M => {
                let junction = hop
                    .junction
                    .ok_or_else(|| "M2M hop is missing its JunctionSpec".to_string())?;
                let jalias = Alias::new(format!("__ldm_{rev_i}"));
                q.join_as(
                    JoinType::InnerJoin,
                    crate::db::router::schema_qualified_table(junction.table),
                    jalias.clone(),
                    Expr::col((jalias.clone(), Alias::new(junction.target_column)))
                        .eq(link_expr.clone()),
                );
                link_expr = Expr::col((jalias, Alias::new(junction.parent_column)));
            }
            HopKind::ReverseFk => {
                // Guaranteed the leaf hop by the position check above: the FK
                // column lives on the leaf and points back at the parent PK,
                // so the parent-side link is simply `leaf.<fk_column>`.
                link_expr = Expr::col((Alias::new(leaf_table), Alias::new(hop.fk_column)));
            }
            // The segment was validated to be all to-many above.
            _ => unreachable!("to-one hop in the to-many segment was rejected"),
        }
    }

    // Anchor the outermost link at the pivot: the root PK directly (no prefix)
    // or an `IN (…)` subquery resolving the all-to-one prefix to the pivot PK.
    if prefix.is_empty() {
        q.and_where(link_expr.eq(SimpleExpr::Value(pk_value.clone())));
    } else {
        let pivot_sub =
            build_prefix_pivot_subquery(&registered, base_table, base_pk_column, pk_value, prefix)?;
        q.and_where(link_expr.in_subquery(pivot_sub));
    }

    Ok((q, leaf_pk.to_string()))
}

/// Build the `SELECT <pivot.pk> FROM <root> JOIN … WHERE root.pk = ?`
/// subquery that resolves an all-to-one prefix to the single pivot PK the
/// first to-many hop hangs off. The pivot table is the prefix's last target
/// (= the first to-many hop's `from_table`); only prefix tables appear here,
/// all inside this subquery, so none collide with the outer leaf.
fn build_prefix_pivot_subquery(
    registered: &[ModelMeta],
    base_table: &str,
    base_pk_column: &str,
    pk_value: &sea_query::Value,
    prefix: &[crate::orm::relation::HopSpec],
) -> Result<SelectStatement, String> {
    let mut q = Query::select();
    let root_alias = level_alias(0);
    q.from_as(
        crate::db::router::schema_qualified_table(base_table),
        root_alias.clone(),
    );
    // Shared JOIN walk; map its `sqlx::Error` to this builder's `String`
    // error channel (the `resolve_leaf_queryset` poison text). Every hop here
    // is to-one by construction (the all-to-one prefix), so `walk_joins`
    // behaves exactly as the former forward-only walker did.
    let near_alias = walk_joins(
        &mut q,
        root_alias.clone(),
        prefix,
        NullJoinPolicy::Inner,
        "__rel_",
        registered,
    )
    .map_err(|e| e.to_string())?;
    // Project the pivot's PK (the last prefix target's PK).
    let pivot_table = prefix.last().expect("prefix is non-empty").to_table;
    let pivot_pk = pk_of(registered, pivot_table).ok_or_else(|| {
        format!("cannot resolve primary key of pivot `{pivot_table}` (is the model registered?)")
    })?;
    q.column((near_alias, Alias::new(pivot_pk)));
    q.and_where(
        Expr::col((root_alias, Alias::new(base_pk_column))).eq(SimpleExpr::Value(pk_value.clone())),
    );
    Ok(q)
}
