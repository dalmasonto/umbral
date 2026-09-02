//! Phase 2 — forward FK / O2O `__` traversal on the **filter (WHERE) side**.
//!
//! Phase 1 taught the *read* side (`select_related` / `join_related` / the
//! chainable relation accessors) to walk `__` FK hops. The filter side still
//! treated a `__` key as a literal column: `Predicate::col_eq("user__username",
//! v)` built `Expr::col(Alias::new("user__username"))` — a column that does not
//! exist, so the query errored (a 500, not a filter). This module closes gaps4
//! #76 by teaching the filter path the same `__` resolution the join path has.
//!
//! # Mechanism — a self-contained `IN (SELECT …)` chain, no queryset changes
//!
//! A `Predicate<T>` is *only* a `sea_query::SimpleExpr`; it can't tell
//! `filter()` to add a JOIN to the outer query. Rather than thread a join list
//! through `Predicate` and every terminal that reads `self.predicates` (there
//! are ~8: `build_select`, `count`, `delete`, `update_*`, the soft-delete
//! paths), we resolve a forward FK/O2O traversal to a **nested correlated
//! `IN (SELECT …)` subquery** that lives entirely inside the predicate's own
//! `SimpleExpr`. Every existing terminal already does
//! `q.and_where(p.cond_for(backend))`, so it emits the subquery for free — one
//! statement, always parameterized, no aliasing, and no row-multiplication
//! (unlike a JOIN, a to-one `IN` never fans out the outer rows).
//!
//! For a path `a__b__leaf` the shape is (root table `R`, hops `R.a → A`,
//! `A.b → B`, leaf on `B`):
//!
//! ```sql
//! R.a IN (SELECT A.pk FROM A WHERE A.b IN (SELECT B.pk FROM B WHERE B.leaf = ?))
//! ```
//!
//! Built inner-to-outer: the innermost subquery carries the leaf comparison;
//! each outer wrap selects the previous target's PK and constrains the next
//! hop's FK column with `IN (inner)`; the outermost hop's FK column is the
//! predicate's own left-hand column on the root table `R`.
//!
//! # Two surfaces (locked design decision)
//!
//! - **String form** — [`Predicate::related`], Django-style:
//!   `Predicate::<Developer>::related("user__username", "ada")?`. Resolves the
//!   hop fields against the model registry (reusing the same field-name walk
//!   `resolve_join_hops` uses) and validates the leaf column; a bad field is a
//!   clear `Err`, never a literal-column 500. Supports arbitrary forward depth.
//! - **Typed form** — [`ForeignKeyCol::to`] /
//!   [`NullableForeignKeyCol::to`](super::super::column::NullableForeignKeyCol::to):
//!   `Developer::USER.to(AuthUser::USERNAME.eq("ada"))`. The leaf predicate is
//!   built from the target model's own typed columns, so its value type is
//!   compile-checked. Deeper chains nest: `A::FK.to(B::FK.to(C::LEAF.eq(v)))`.
//!
//! # Scope
//!
//! Forward FK / O2O hops to arbitrary depth (the gap's core, `user__username`).
//! Reverse-FK / M2M `__` in WHERE are a documented follow-up (they widen to a
//! to-many `EXISTS`, a different shape) — a to-many segment in the string form
//! errors clearly rather than emitting a wrong query.

use sea_query::{Alias, Expr, ExprTrait, Query, SelectStatement, SimpleExpr, Value};

use crate::migrate::registered_models_opt;
use crate::orm::{Model, Predicate};

/// One resolved forward hop: the FK column on the *previous* table, the table
/// it targets, and that target's primary-key column.
#[derive(Debug, Clone)]
pub(crate) struct RelHop {
    /// FK column on the previous level's table (the root table for hop 0).
    pub(crate) fk_column: String,
    /// Table this hop targets.
    pub(crate) target_table: String,
    /// Primary-key column on `target_table`.
    pub(crate) target_pk: String,
}

/// Wrap a leaf condition (a bare-column `SimpleExpr` on the innermost target
/// table) in the nested `IN (SELECT …)` chain across `hops`.
///
/// `hops` is ordered root→leaf. With no hops the leaf condition IS the
/// predicate (a plain local-column filter — the degenerate no-`__` case).
pub(crate) fn build_forward_in_subquery(hops: &[RelHop], leaf_cond: SimpleExpr) -> SimpleExpr {
    let Some(last) = hops.last() else {
        return leaf_cond;
    };

    // Innermost subquery: SELECT <lastTarget.pk> FROM <lastTarget> WHERE <leaf>.
    let mut sub: SelectStatement = Query::select();
    sub.column(Alias::new(last.target_pk.clone()))
        .from(crate::db::router::schema_qualified_table(
            &last.target_table,
        ))
        .and_where(leaf_cond);

    // Wrap outward. Hop `i`'s target table carries hop `i+1`'s FK column, so
    // the wrap constrains `<hop[i+1].fk_column> IN (inner)` and selects
    // `hop[i].target_pk` for the NEXT wrap (or the outer predicate).
    for i in (0..hops.len() - 1).rev() {
        let next_fk = &hops[i + 1].fk_column;
        let cond = Expr::col(Alias::new(next_fk.clone())).in_subquery(sub);
        let mut outer: SelectStatement = Query::select();
        outer
            .column(Alias::new(hops[i].target_pk.clone()))
            .from(crate::db::router::schema_qualified_table(
                &hops[i].target_table,
            ))
            .and_where(cond);
        sub = outer;
    }

    // Outermost: the root-table FK column of hop 0 IN (the assembled subquery).
    Expr::col(Alias::new(hops[0].fk_column.clone())).in_subquery(sub)
}

/// Build a `Predicate<Owner>` for a single typed forward FK/O2O hop whose leaf
/// is `leaf` (a `Predicate<Target>` built from the target model's own typed
/// columns). The engine behind [`ForeignKeyCol::to`].
///
/// `Target` supplies its table and PK, so the typed form needs **no** model
/// registry — it resolves in a bare `.on(&pool)` test without a booted `App`.
/// Deeper chains nest naturally: `leaf` may itself be a `.to(...)` predicate,
/// whose subquery becomes the inner condition here.
pub(crate) fn typed_forward_hop<Owner, Target: Model>(
    fk_column: &'static str,
    leaf: Predicate<Target>,
) -> Predicate<Owner> {
    let hop = RelHop {
        fk_column: fk_column.to_string(),
        target_table: Target::TABLE.to_string(),
        target_pk: pk_column_of::<Target>().to_string(),
    };
    let hops = std::slice::from_ref(&hop);
    let cond = build_forward_in_subquery(hops, leaf.cond.clone());
    match &leaf.cond_sqlite {
        Some(sqlite) => {
            let cond_sqlite = build_forward_in_subquery(hops, sqlite.clone());
            Predicate::new_with_sqlite(cond, cond_sqlite)
        }
        None => Predicate::new(cond),
    }
}

/// Resolve a Django-style `__` path on `T` into a forward-traversal predicate.
/// The engine behind [`Predicate::related`].
pub(crate) fn build_string_relation<T: Model>(
    path: &str,
    value: Value,
) -> Result<Predicate<T>, sqlx::Error> {
    let raw: Vec<&str> = path.split("__").filter(|s| !s.is_empty()).collect();
    if raw.is_empty() {
        return Err(protocol("umbral::orm::related: empty relation path"));
    }

    // A trailing recognized lookup (`__icontains`) is the operator; otherwise
    // the whole tail is the leaf column and the op is `=`. A single segment is
    // always the leaf (never a lookup), so a lone `"name"` filters locally.
    let (col_segs, lookup): (&[&str], Lookup) = match raw.split_last() {
        Some((last, head)) if !head.is_empty() && Lookup::parse(last).is_some() => {
            (head, Lookup::parse(last).unwrap())
        }
        _ => (&raw[..], Lookup::Exact),
    };
    // After stripping the lookup there must still be a leaf column.
    let (leaf_col, hop_segs) = col_segs
        .split_last()
        .ok_or_else(|| protocol("umbral::orm::related: relation path has no leaf column"))?;

    // No hops → a plain local-column filter (still validated against T::FIELDS).
    if hop_segs.is_empty() {
        if !T::FIELDS.iter().any(|f| f.name == *leaf_col) {
            return Err(protocol(&format!(
                "umbral::orm::related: unknown column `{leaf_col}` on model `{}`",
                T::NAME
            )));
        }
        let leaf_cond = build_leaf_cond(leaf_col, lookup, value)?;
        return Ok(Predicate::new(leaf_cond));
    }

    let registered = registered_models_opt().ok_or_else(|| {
        protocol(
            "umbral::orm::related: no model registry available to resolve a `__` relation \
             filter — build an App (which registers models) first, or use the typed \
             `FkCol::to(...)` form (which needs no registry)",
        )
    })?;

    // Walk the FK hops. Hop 0 reads `T::FIELDS` (parity with resolve_join_hops);
    // deeper hops read the registry's columns for the prior target table.
    let mut hops: Vec<RelHop> = Vec::with_capacity(hop_segs.len());
    let mut current_table = T::TABLE.to_string();
    for (i, seg) in hop_segs.iter().enumerate() {
        let (fk_col, target_table): (String, String) = if i == 0 {
            let f = T::FIELDS.iter().find(|f| f.name == *seg).ok_or_else(|| {
                protocol(&format!(
                    "umbral::orm::related: unknown field `{seg}` on model `{}` (in path `{path}`)",
                    T::NAME
                ))
            })?;
            let tgt = f.fk_target.ok_or_else(|| {
                protocol(&format!(
                    "umbral::orm::related: field `{seg}` on `{}` is not a forward relation \
                     (FK / O2O) — only forward FK/O2O hops are supported on the filter side; \
                     reverse-FK / M2M `__` filters are a documented follow-up",
                    T::NAME
                ))
            })?;
            (f.name.to_string(), tgt.to_string())
        } else {
            let meta = registered
                .iter()
                .find(|m| m.table == current_table)
                .ok_or_else(|| {
                    protocol(&format!(
                        "umbral::orm::related: intermediate table `{current_table}` is not \
                         registered (in path `{path}`)"
                    ))
                })?;
            let col = meta.fields.iter().find(|c| c.name == *seg).ok_or_else(|| {
                protocol(&format!(
                    "umbral::orm::related: unknown field `{seg}` on `{current_table}` \
                     (in path `{path}`)"
                ))
            })?;
            let tgt = col.fk_target.clone().ok_or_else(|| {
                protocol(&format!(
                    "umbral::orm::related: field `{seg}` on `{current_table}` is not a forward \
                     relation (FK / O2O) — reverse-FK / M2M `__` filters are a documented \
                     follow-up (in path `{path}`)"
                ))
            })?;
            (col.name.clone(), tgt)
        };

        let tmeta = registered
            .iter()
            .find(|m| m.table == target_table)
            .ok_or_else(|| {
                protocol(&format!(
                    "umbral::orm::related: target model `{target_table}` is not registered \
                     (in path `{path}`)"
                ))
            })?;
        let pk = tmeta.fields.iter().find(|c| c.primary_key).ok_or_else(|| {
            protocol(&format!(
                "umbral::orm::related: target model `{target_table}` has no primary key"
            ))
        })?;
        hops.push(RelHop {
            fk_column: fk_col,
            target_table: target_table.clone(),
            target_pk: pk.name.clone(),
        });
        current_table = target_table;
    }

    // Validate the leaf column exists on the innermost target table.
    let leaf_ok = registered
        .iter()
        .find(|m| m.table == current_table)
        .map(|m| m.fields.iter().any(|c| c.name == *leaf_col))
        .unwrap_or(false);
    if !leaf_ok {
        return Err(protocol(&format!(
            "umbral::orm::related: leaf column `{leaf_col}` not found on `{current_table}` \
             (in path `{path}`)"
        )));
    }

    let leaf_cond = build_leaf_cond(leaf_col, lookup, value)?;
    let cond = build_forward_in_subquery(&hops, leaf_cond);
    Ok(Predicate::new(cond))
}

/// The supported trailing lookups for the string form. Comparison lookups take
/// the value as-is; the LIKE family needs a string value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    Exact,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    IContains,
    StartsWith,
    EndsWith,
}

impl Lookup {
    fn parse(s: &str) -> Option<Lookup> {
        Some(match s {
            "exact" | "eq" => Lookup::Exact,
            "ne" => Lookup::Ne,
            "gt" => Lookup::Gt,
            "gte" => Lookup::Gte,
            "lt" => Lookup::Lt,
            "lte" => Lookup::Lte,
            "contains" => Lookup::Contains,
            "icontains" => Lookup::IContains,
            "startswith" => Lookup::StartsWith,
            "endswith" => Lookup::EndsWith,
            _ => return None,
        })
    }
}

/// Build the innermost leaf comparison on a bare column. Inside the subquery's
/// single-table FROM the bare column resolves unambiguously to that table.
fn build_leaf_cond(col: &str, lookup: Lookup, value: Value) -> Result<SimpleExpr, sqlx::Error> {
    let c = || Expr::col(Alias::new(col.to_string()));
    let expr = match lookup {
        Lookup::Exact => c().eq(value),
        Lookup::Ne => c().ne(value),
        Lookup::Gt => c().gt(value),
        Lookup::Gte => c().gte(value),
        Lookup::Lt => c().lt(value),
        Lookup::Lte => c().lte(value),
        Lookup::Contains => c().like(format!("%{}%", string_value(col, value)?)),
        Lookup::IContains => {
            let pat = format!("%{}%", string_value(col, value)?.to_uppercase());
            sea_query::Func::upper(c()).like(pat)
        }
        Lookup::StartsWith => c().like(format!("{}%", string_value(col, value)?)),
        Lookup::EndsWith => c().like(format!("%{}", string_value(col, value)?)),
    };
    Ok(expr)
}

/// Extract a `String` from a `Value` for the LIKE-family lookups; a clear error
/// when a text lookup is handed a non-text value.
fn string_value(col: &str, value: Value) -> Result<String, sqlx::Error> {
    match value {
        Value::String(Some(s)) => Ok(*s),
        Value::Char(Some(ch)) => Ok(ch.to_string()),
        other => Err(protocol(&format!(
            "umbral::orm::related: a text lookup on `{col}` needs a string value, got {other:?}"
        ))),
    }
}

/// The primary-key column name of a model, from its `FIELDS` metadata. Falls
/// back to `"id"` for the (derive-impossible) no-PK case, matching `relation`.
fn pk_column_of<M: Model>() -> &'static str {
    M::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.name)
        .unwrap_or("id")
}

fn protocol(msg: &str) -> sqlx::Error {
    sqlx::Error::Protocol(msg.to_string())
}
