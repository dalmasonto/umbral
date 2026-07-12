//! Soft-delete cascade (gaps3 #53).
//!
//! `on_delete = "cascade"` is a **DDL** clause: the database fires it when a row
//! is really `DELETE`d. A soft delete is an `UPDATE` that stamps `deleted_at`, so
//! the database never fires anything — and a soft-deleted parent used to leave
//! its children behind as live rows.
//!
//! That was easy to miss because reads *through* the parent hide them (the join
//! and prefetch paths fold `AND child.deleted_at IS NULL` into their subqueries).
//! The children were invisible via the parent while still answering their own
//! queries, still counting, still holding unique constraints. This module closes
//! that: soft-deleting a parent soft-deletes every `on_delete = "cascade"`
//! descendant, and restoring the parent brings back exactly those rows.
//!
//! # How it works
//!
//! Two ideas carry the whole implementation.
//!
//! **1. Subqueries, not primary keys.** The cascade never marshals PK values. A
//! child is selected with `child.fk IN (SELECT pk FROM parent WHERE …)`. That
//! sidesteps PK-type conversion (i64 / String / Uuid all just work) and has no
//! IN-list size limit, so deleting a parent with a million children is one
//! statement, not a million bindings.
//!
//! **2. One timestamp identifies one cascade.** Every row the cascade touches —
//! parent, child, grandchild — is stamped with the *same* `deleted_at`. That is
//! what makes restore correct: restoring the parent clears only descendants whose
//! `deleted_at` equals the parent's, so a child that was *independently* trashed
//! last week stays trashed. Without this, restore would resurrect rows the user
//! deliberately deleted.
//!
//! # Ordering
//!
//! Delete cascades **children first, parent last**. The parent's own predicate
//! (`… AND deleted_at IS NULL`) is what identifies the rows being deleted, so it
//! has to keep matching while the children are being found. Stamp the parent
//! first and the selector goes empty mid-cascade.
//!
//! Restore is the mirror: **deepest first**. A grandchild is located through its
//! child, so the child must still carry `deleted_at` when the grandchild is
//! restored.
//!
//! Everything runs inside the caller's transaction. A half-applied cascade would
//! be its own corruption bug.

use std::collections::HashSet;

use sea_query::{Alias, Expr, PostgresQueryBuilder, Query, SelectStatement, SqliteQueryBuilder};
use sea_query_binder::SqlxBinder;

use crate::db::router::schema_qualified_table;

/// A live connection inside someone's transaction.
///
/// The typed QuerySet opens a raw `sqlx` transaction; the dynamic one carries the
/// framework's `Transaction` enum. Both borrow down to a plain connection, so the
/// cascade is written once against this instead of twice against them.
pub(crate) enum CascadeConn<'a> {
    Sqlite(&'a mut sqlx::SqliteConnection),
    Pg(&'a mut sqlx::PgConnection),
}

impl<'a> CascadeConn<'a> {
    /// Borrow the framework's `Transaction` enum down to a connection.
    pub(crate) fn from_tx(tx: &'a mut crate::db::Transaction) -> Self {
        if tx.backend_name() == "sqlite" {
            CascadeConn::Sqlite(tx.as_sqlite_mut().expect("sqlite backend_name"))
        } else {
            CascadeConn::Pg(tx.as_pg_mut().expect("postgres backend_name"))
        }
    }
}
use crate::migrate::ModelMeta;
use crate::orm::FkAction;

/// How deep a cascade will follow FK chains before giving up. A real schema is
/// 2–3 levels; this only exists so a pathological (or cyclic-looking) graph can't
/// build an unbounded nest of subqueries.
const MAX_CASCADE_DEPTH: usize = 16;

/// Every `(child model, FK column)` that declares `on_delete = "cascade"` to
/// `parent_table`. Read from the live model registry, so it sees models from
/// every plugin, not just the caller's.
fn cascade_children(parent_table: &str) -> Vec<(ModelMeta, String)> {
    let mut out = Vec::new();
    for meta in crate::migrate::registered_models_opt().unwrap_or_default() {
        for col in &meta.fields {
            if col.fk_target.as_deref() == Some(parent_table)
                && matches!(col.on_delete, FkAction::Cascade)
            {
                out.push((meta.clone(), col.name.clone()));
            }
        }
    }
    out
}

/// `SELECT <pk> FROM <table> WHERE <conds>` — the row-set a cascade level acts on.
fn selector(meta: &ModelMeta, conds: Vec<sea_query::SimpleExpr>) -> Option<SelectStatement> {
    let pk = meta.pk_column()?;
    let mut sel = Query::select();
    sel.column(Alias::new(&pk.name))
        .from(schema_qualified_table(&meta.table));
    for c in conds {
        sel.and_where(c);
    }
    Some(sel)
}

async fn exec(
    conn: &mut CascadeConn<'_>,
    stmt: &sea_query::UpdateStatement,
) -> Result<u64, sqlx::Error> {
    match conn {
        CascadeConn::Sqlite(c) => {
            let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
            Ok(sqlx::query_with(&sql, values)
                .execute(&mut **c)
                .await?
                .rows_affected())
        }
        CascadeConn::Pg(c) => {
            let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
            Ok(sqlx::query_with(&sql, values)
                .execute(&mut **c)
                .await?
                .rows_affected())
        }
    }
}

/// Soft-delete every `on_delete = "cascade"` descendant of the rows matched by
/// `parent_sel`, stamping each with `at` — the same instant the parent gets.
///
/// Call this **before** stamping the parent: `parent_sel` carries the parent's
/// live-rows predicate, which stops matching once the parent is stamped.
///
/// `at` must be the identical timestamp the parent is about to receive; that
/// shared value is what [`cascade_restore`] later uses to undo exactly this
/// cascade and nothing else.
pub(crate) async fn cascade_soft_delete(
    conn: &mut CascadeConn<'_>,
    parent: &ModelMeta,
    parent_sel: SelectStatement,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<u64, sqlx::Error> {
    let mut seen = HashSet::from([parent.table.clone()]);
    cascade_delete_level(conn, parent, parent_sel, at, &mut seen, 0).await
}

async fn cascade_delete_level(
    conn: &mut CascadeConn<'_>,
    parent: &ModelMeta,
    parent_sel: SelectStatement,
    at: chrono::DateTime<chrono::Utc>,
    seen: &mut HashSet<String>,
    depth: usize,
) -> Result<u64, sqlx::Error> {
    if depth >= MAX_CASCADE_DEPTH {
        tracing::warn!(
            table = %parent.table,
            "soft-delete cascade hit the depth limit ({MAX_CASCADE_DEPTH}); \
             deeper descendants were not cascaded — is there an FK cycle?",
        );
        return Ok(0);
    }
    let mut total = 0u64;

    for (child, fk) in cascade_children(&parent.table) {
        // A cascade child that isn't itself `soft_delete` has nowhere to record
        // the deletion. We refuse to hard-delete it — that would make the
        // parent's soft delete irreversible, which is the one thing soft delete
        // promises it is not. `check_cascade_targets` reports this at boot.
        if !child.soft_delete || !seen.insert(child.table.clone()) {
            continue;
        }

        // UPDATE child SET deleted_at = at
        //   WHERE deleted_at IS NULL AND fk IN (parent_sel)
        let mut stmt = Query::update();
        stmt.table(schema_qualified_table(&child.table))
            .value(
                Alias::new("deleted_at"),
                sea_query::Value::ChronoDateTimeUtc(Some(Box::new(at))),
            )
            .and_where(Expr::col(Alias::new("deleted_at")).is_null())
            .and_where(Expr::col(Alias::new(fk.as_str())).in_subquery(parent_sel.clone()));
        total += exec(conn, &stmt).await?;

        // Grandchildren hang off the rows we just stamped: they are exactly the
        // child rows carrying THIS cascade's timestamp.
        if let Some(child_sel) = selector(
            &child,
            vec![
                Expr::col(Alias::new("deleted_at")).eq(at),
                Expr::col(Alias::new(fk.as_str())).in_subquery(parent_sel.clone()),
            ],
        ) {
            total += Box::pin(cascade_delete_level(
                conn,
                &child,
                child_sel,
                at,
                seen,
                depth + 1,
            ))
            .await?;
        }
    }
    Ok(total)
}

/// Undo the cascade that [`cascade_soft_delete`] applied: restore descendants of
/// `parent_sel` whose `deleted_at` equals `at` — the parent's own deletion
/// instant — and no others.
///
/// The `deleted_at = at` match is the whole point. A child trashed on its own,
/// before or after the parent, has a different timestamp and stays trashed.
/// Restoring a parent must not resurrect rows someone deliberately deleted.
///
/// Call this **before** clearing the parent: `parent_sel` locates rows by their
/// still-present `deleted_at`.
pub(crate) async fn cascade_restore(
    conn: &mut CascadeConn<'_>,
    parent: &ModelMeta,
    parent_sel: SelectStatement,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<u64, sqlx::Error> {
    let mut seen = HashSet::from([parent.table.clone()]);
    cascade_restore_level(conn, parent, parent_sel, at, &mut seen, 0).await
}

async fn cascade_restore_level(
    conn: &mut CascadeConn<'_>,
    parent: &ModelMeta,
    parent_sel: SelectStatement,
    at: chrono::DateTime<chrono::Utc>,
    seen: &mut HashSet<String>,
    depth: usize,
) -> Result<u64, sqlx::Error> {
    if depth >= MAX_CASCADE_DEPTH {
        return Ok(0);
    }
    let mut total = 0u64;

    for (child, fk) in cascade_children(&parent.table) {
        if !child.soft_delete || !seen.insert(child.table.clone()) {
            continue;
        }
        let Some(child_sel) = selector(
            &child,
            vec![
                Expr::col(Alias::new("deleted_at")).eq(at),
                Expr::col(Alias::new(fk.as_str())).in_subquery(parent_sel.clone()),
            ],
        ) else {
            continue;
        };

        // Deepest first: a grandchild is located THROUGH this child, so the child
        // must still carry `deleted_at` while we go looking. Clear it after.
        total += Box::pin(cascade_restore_level(
            conn,
            &child,
            child_sel.clone(),
            at,
            seen,
            depth + 1,
        ))
        .await?;

        let mut stmt = Query::update();
        stmt.table(schema_qualified_table(&child.table))
            .value(
                Alias::new("deleted_at"),
                sea_query::Value::ChronoDateTimeUtc(None),
            )
            .and_where(Expr::col(Alias::new("deleted_at")).eq(at))
            .and_where(Expr::col(Alias::new(fk.as_str())).in_subquery(parent_sel.clone()));
        total += exec(conn, &stmt).await?;
    }
    Ok(total)
}

/// The distinct `deleted_at` instants among the rows `conds` selects.
///
/// A restore can span several cascades — an admin ticking three trashed parents
/// that were deleted on three different days. Each is undone against its own
/// timestamp, so each brings back exactly its own descendants.
pub(crate) async fn deleted_at_values(
    conn: &mut CascadeConn<'_>,
    meta: &ModelMeta,
    conds: &[sea_query::Condition],
) -> Result<Vec<chrono::DateTime<chrono::Utc>>, sqlx::Error> {
    use sqlx::Row as _;

    let mut sel = Query::select();
    sel.distinct()
        .column(Alias::new("deleted_at"))
        .from(schema_qualified_table(&meta.table));
    for c in conds {
        sel.cond_where(c.clone());
    }
    sel.and_where(Expr::col(Alias::new("deleted_at")).is_not_null());

    match conn {
        CascadeConn::Sqlite(c) => {
            let (sql, values) = sel.build_sqlx(SqliteQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(&mut **c).await?;
            Ok(rows
                .iter()
                .filter_map(|r| r.try_get::<chrono::DateTime<chrono::Utc>, _>(0).ok())
                .collect())
        }
        CascadeConn::Pg(c) => {
            let (sql, values) = sel.build_sqlx(PostgresQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(&mut **c).await?;
            Ok(rows
                .iter()
                .filter_map(|r| r.try_get::<chrono::DateTime<chrono::Utc>, _>(0).ok())
                .collect())
        }
    }
}

/// `SELECT pk FROM <parent> WHERE <conds> AND deleted_at = at` — the parents
/// belonging to one cascade, used to locate that cascade's descendants.
pub(crate) fn selector_at(
    meta: &ModelMeta,
    conds: &[sea_query::Condition],
    at: chrono::DateTime<chrono::Utc>,
) -> Option<SelectStatement> {
    let pk = meta.pk_column()?;
    let mut sel = Query::select();
    sel.column(Alias::new(&pk.name))
        .from(schema_qualified_table(&meta.table));
    for c in conds {
        sel.cond_where(c.clone());
    }
    sel.and_where(Expr::col(Alias::new("deleted_at")).eq(at));
    Some(sel)
}

/// Boot check: a `soft_delete` model must not have a cascade child that cannot
/// itself be soft-deleted.
///
/// The declaration `on_delete = "cascade"` says *"when the parent goes, the child
/// goes"*. If the parent's going is a soft delete and the child has no
/// `deleted_at`, that promise cannot be kept: we will not hard-delete the child
/// (that would make a reversible operation irreversible), so the child would be
/// silently left behind — the exact bug this module exists to fix. Better to say
/// so at boot than to leak orphans in production.
///
/// Returns one message per offending (child, parent) pair.
pub fn check_cascade_targets() -> Vec<String> {
    // Runs during the boot system-check phase; if the registry isn't up there is
    // nothing to check, and a check that panics is a check that never reports.
    let Some(models) = crate::migrate::registered_models_opt() else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    for parent in models.iter().filter(|m| m.soft_delete) {
        for (child, fk) in cascade_children(&parent.table) {
            if !child.soft_delete {
                problems.push(format!(
                    "`{child}.{fk}` declares `on_delete = \"cascade\"` to `{parent}`, but \
                     `{parent}` is `#[umbral(soft_delete)]` and `{child}` is not. A soft delete \
                     is an UPDATE, so the database never cascades, and `{child}` rows would be \
                     left behind pointing at a deleted `{parent}`. Fix by marking `{child}` \
                     `#[umbral(soft_delete)]` too (so the cascade can follow), or by changing \
                     `{child}.{fk}` to `on_delete = \"set_null\"` / `\"restrict\"` if the child \
                     is meant to outlive its parent.",
                    child = child.table,
                    parent = parent.table,
                    fk = fk,
                ));
            }
        }
    }
    problems
}
