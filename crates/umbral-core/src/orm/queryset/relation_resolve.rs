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

use sea_query::{
    Alias, Expr, JoinType, PostgresQueryBuilder, Query, SelectStatement, SimpleExpr,
    SqliteQueryBuilder,
};
use sea_query_binder::SqlxBinder;

use crate::db::DbPool;
use crate::migrate::{ModelMeta, registered_models};
use crate::orm::Model;
use crate::orm::relation::{PathBase, RelPath};

/// Per-level table alias (`__rel_0` is the root, `__rel_1` the first hop's
/// target, …). Distinct from `join_related`'s `__j_*` aliases so the two
/// JOIN builders never clash if one path ever nests inside the other.
fn level_alias(level: usize) -> Alias {
    Alias::new(format!("__rel_{level}"))
}

/// The primary-key column name of a registered table, looked up the same way
/// [`super::resolve_join_hops`] does — from the migrate registry.
fn pk_of<'a>(registered: &'a [ModelMeta], table: &str) -> Option<&'a str> {
    registered
        .iter()
        .find(|m| m.table == table)?
        .fields
        .iter()
        .find(|c| c.primary_key)
        .map(|c| c.name.as_str())
}

/// Build the single flat `SELECT <leaf.*> FROM <root> JOIN … WHERE root.pk = ?`
/// statement for an all-to-one [`RelPath`].
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

    let registered = registered_models();
    let PathBase::SinglePk {
        table: base_table,
        pk_column: base_pk_column,
        pk_value,
    } = &path.base;

    let mut q = Query::select();
    let root_alias = level_alias(0);
    q.from_as(
        crate::db::router::schema_qualified_table(base_table),
        root_alias.clone(),
    );

    // Walk the hops, joining each target onto the previous level's alias.
    let mut near_alias = root_alias.clone();
    for (idx, hop) in path.hops.iter().enumerate() {
        let far_alias = level_alias(idx + 1);
        let on = if hop.fk_on_from {
            // Forward FK / O2O: FK column on the NEAR table -> FAR pk.
            let far_pk = pk_of(&registered, hop.to_table).ok_or_else(|| {
                protocol_error(&format!(
                    "cannot resolve primary key of `{}` (is the model registered?)",
                    hop.to_table
                ))
            })?;
            Expr::col((near_alias.clone(), Alias::new(hop.fk_column)))
                .equals((far_alias.clone(), Alias::new(far_pk)))
        } else {
            // Reverse O2O (parent side): FK column on the FAR table -> NEAR pk.
            let near_pk = pk_of(&registered, hop.from_table).ok_or_else(|| {
                protocol_error(&format!(
                    "cannot resolve primary key of `{}` (is the model registered?)",
                    hop.from_table
                ))
            })?;
            Expr::col((near_alias.clone(), Alias::new(near_pk)))
                .equals((far_alias.clone(), Alias::new(hop.fk_column)))
        };
        q.join_as(
            JoinType::InnerJoin,
            crate::db::router::schema_qualified_table(hop.to_table),
            far_alias.clone(),
            on,
        );
        near_alias = far_alias;
    }

    // Project the leaf's own columns, aliased to their bare names so `Leaf`'s
    // `FromRow` reads them by field name regardless of the JOIN aliasing.
    for f in Leaf::FIELDS {
        q.expr_as(
            Expr::col((near_alias.clone(), Alias::new(f.name))),
            Alias::new(f.name),
        );
    }

    q.and_where(
        Expr::col((root_alias, Alias::new(*base_pk_column)))
            .eq(SimpleExpr::Value(pk_value.clone())),
    );
    q.limit(1);

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
