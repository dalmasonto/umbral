//! Heavy-relations epic, Plan B Task 3 — multi-hop aggregate subqueries.
//!
//! `annotate_count("posts__comments")` / `annotate_sum("total", "posts__price")`
//! each compile to ONE correlated scalar subquery:
//!
//! ```sql
//! SELECT <AGG>(<leaf-or-DISTINCT-leaf-pk>)
//! FROM   <root>          AS __agg_0
//! JOIN   …(walk_joins)…  AS __agg_N
//! WHERE  __agg_0.<pk> = <outer row>.<pk>
//! ```
//!
//! The JOIN chain is built by [`walk_joins`] (Plan A) — the SAME walker
//! `build_to_one_select` and `build_prefix_pivot_subquery` already use for
//! to-one traversal — so this module owns exactly one thing: resolving a
//! path to hops via [`RelPath::from_path`], then wrapping the walk in a
//! correlated `SELECT <agg>`. It is the first caller to drive `walk_joins`'
//! `M2M` and `ReverseFk` arms end-to-end (see
//! `crates/umbral-core/tests/annotate_relation_path.rs`).
//!
//! **No shared JOIN, no GROUP BY on the base** — each annotation gets its
//! own independent subquery, so stacking N annotations can never inflate
//! one against another (the classic Django multi-aggregate JOIN bug is
//! impossible by construction: there is nothing to multiply against).
//!
//! See `docs/specs/orm-heavy-relations-epic.md` ("Security & Performance").

use sea_query::{Alias, Expr, Func, Query, SelectStatement, SimpleExpr};

use crate::migrate::registered_models_opt;
use crate::orm::AggregateKind;
use crate::orm::Model;
use crate::orm::relation::{NullJoinPolicy, RelPath};

use super::relation_resolve::{pk_of, walk_joins};

/// Build the correlated-subquery aggregate for `path` (a `__`-separated
/// relation path resolved off `T`) — see the module docs for the SQL shape.
///
/// `column` is required (and used) for every kind except [`AggregateKind::Count`],
/// which always counts `DISTINCT <leaf>.<leaf_pk>` (dedupes a leaf reachable via
/// more than one join, e.g. an M2M junction row per shared tag) and ignores
/// `column` entirely.
///
/// Returns the built subquery plus the LEAF table name (the caller needs the
/// latter so `fetch_annotated`'s existing per-column `SqlType` lookup — keyed
/// by `(child_table, column)` — resolves the aggregated column's type without
/// any new decode path).
///
/// Every table in the JOIN chain (including the root) is schema-qualified via
/// [`crate::db::router::schema_qualified_table`] — `walk_joins` does this for
/// every hop it appends, and this function does it for the root `FROM` the
/// same way [`super::relation_resolve::build_to_one_select`] does.
pub(crate) fn build_aggregate_subquery<T: Model>(
    path: &str,
    agg: AggregateKind,
    column: Option<&str>,
) -> Result<(SelectStatement, String), sqlx::Error> {
    let rel = RelPath::from_path::<T>(path)?;
    if rel.hops.is_empty() {
        return Err(protocol_error(
            "relation path has no hops — nothing to aggregate",
        ));
    }

    // `RelPath::from_path` itself only reaches for the registry when the
    // path has more than one segment; an aggregate ALWAYS needs it (to
    // resolve every intermediate/leaf PK via `walk_joins`/`pk_of`), so ask
    // up front with the same clear, non-panicking error every other
    // registry-dependent resolver in this epic gives pre-boot.
    let registered = registered_models_opt().ok_or_else(|| {
        protocol_error(
            "no model registry available to build a relation-path aggregate — build \
             an App (which registers models) before calling annotate_count/annotate_sum/\
             annotate_avg/annotate_min/annotate_max with a relation path",
        )
    })?;

    let root_alias = Alias::new("__agg_0");
    let mut q = Query::select();
    q.from_as(
        crate::db::router::schema_qualified_table(T::TABLE),
        root_alias.clone(),
    );
    let leaf_alias = walk_joins(
        &mut q,
        root_alias.clone(),
        &rel.hops,
        NullJoinPolicy::Inner,
        "__agg_",
        &registered,
    )?;

    // Correlate: the subquery's own root row must be the SAME row as the
    // outer query's current row. The outer main SELECT's FROM has no alias
    // (`Manager::queryset` does `.from(schema_qualified_table(T::TABLE))`),
    // so the outer row is addressed by the bare table name — exactly the
    // convention the pre-existing single-hop `annotate_related` correlation
    // already relies on (`Alias::new(parent_table)`).
    let root_pk = root_pk_column::<T>();
    q.and_where(
        Expr::col((root_alias, Alias::new(root_pk)))
            .equals((Alias::new(T::TABLE), Alias::new(root_pk))),
    );

    let leaf_table = rel
        .hops
        .last()
        .expect("checked non-empty above")
        .to_table
        .to_string();
    let leaf_meta = registered.iter().find(|m| m.table == leaf_table);

    // Soft-delete scoping (Task 3 review, IMPORTANT #2): the single-hop
    // `annotate_related`/`annotate_count` path folds `AND
    // <child>.deleted_at IS NULL` into its correlated subquery when the
    // child model is `#[umbral(soft_delete)]` (`child_soft_delete` in
    // `queryset/mod.rs`'s `build_query_for`) — a trashed child must not
    // silently inflate the count/sum/etc. A deep relation-path aggregate
    // must not regress that: exclude a soft-deleted LEAF row here the same
    // way. (Scoped to the leaf only, not every intermediate hop along the
    // path — see this module's `annotate_relation_path.rs` test file /
    // the Task 3 report for the narrower-than-ideal remaining gap.)
    if leaf_meta.is_some_and(|m| m.soft_delete) {
        q.and_where(Expr::col((leaf_alias.clone(), Alias::new("deleted_at"))).is_null());
    }

    let expr: SimpleExpr = match agg {
        AggregateKind::Count => {
            let leaf_pk = pk_of(&registered, &leaf_table).ok_or_else(|| {
                protocol_error(&format!(
                    "cannot resolve primary key of `{leaf_table}` (is the model registered?)"
                ))
            })?;
            Func::count_distinct(Expr::col((leaf_alias, Alias::new(leaf_pk)))).into()
        }
        AggregateKind::Sum | AggregateKind::Avg | AggregateKind::Min | AggregateKind::Max => {
            let col = column.ok_or_else(|| {
                protocol_error(
                    "sum/avg/min/max require a column path (e.g. \"posts__price\") — the \
                     LAST `__` segment names the column on the related model, the prefix \
                     names the relation path to it",
                )
            })?;
            // Security & Performance (docs/specs/orm-heavy-relations-epic.md):
            // an aggregate must never let a `Masked`/hard-denied column's
            // plaintext leak out through SUM/MIN/MAX — check the LEAF
            // model's own column metadata, the same secrecy gate the read
            // path (`values`/serialization) already enforces elsewhere.
            if let Some(leaf_col) = leaf_meta.and_then(|m| m.fields.iter().find(|c| c.name == col))
            {
                if crate::orm::secrets::is_secret_column(leaf_col) {
                    return Err(protocol_error(&format!(
                        "cannot aggregate `{col}` on `{leaf_table}` — it is a masked/secret \
                         column and may never leave the database, including through SUM/AVG/\
                         MIN/MAX"
                    )));
                }
            }
            let c = Expr::col((leaf_alias, Alias::new(col)));
            match agg {
                AggregateKind::Sum => Func::sum(c).into(),
                AggregateKind::Avg => Func::avg(c).into(),
                AggregateKind::Min => Func::min(c).into(),
                AggregateKind::Max => Func::max(c).into(),
                AggregateKind::Count => unreachable!("matched above"),
            }
        }
    };
    q.expr(expr);
    Ok((q, leaf_table))
}

/// `T`'s own primary-key column name, from its static `FIELDS` — the same
/// fallback-to-`"id"` convention `annotate_related`'s pk lookup uses.
fn root_pk_column<T: Model>() -> &'static str {
    T::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.name)
        .unwrap_or("id")
}

fn protocol_error(msg: &str) -> sqlx::Error {
    sqlx::Error::Protocol(msg.to_string())
}
