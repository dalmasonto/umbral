//! Insert-statement builders + PK lookup. Backend-agnostic — every
//! function here takes a `&serde_json::Map` and produces a
//! `sea_query::InsertStatement` (the actual `build_sqlx` to per-
//! backend SQL happens at the call site in `queryset/mod.rs`).
//!
//! Split out of mod.rs purely for size; the SQL build logic only
//! depends on `T::FIELDS` and the write-helper functions in
//! `crate::orm::write`.

use sea_query::{Alias, Query};

use crate::orm::Model;

/// Resolve the target PK's `SqlType` for a `ForeignKey` field so the
/// typed `create` / `bulk_create` path binds the FK id against the
/// parent PK type, not raw TEXT (gaps2 #42). `FieldSpec.fk_target`
/// names the target table; `pk_meta_for_table` returns its PK column +
/// type. `None` for non-FK fields (and FK targets not yet registered,
/// where the write side defaults to BigInt — the common i64-PK case).
pub(crate) fn fk_pk_hint(field: &crate::orm::FieldSpec) -> Option<crate::orm::SqlType> {
    field
        .fk_target
        .and_then(|t| crate::migrate::pk_meta_for_table(t).map(|(_, ty)| ty))
}

/// Reject a non-nullable, non-PK foreign-key column left at the i64-0
/// unset placeholder (`ForeignKey::default()` / `..Default::default()`
/// with the FK never set). The `impl Default for ForeignKey<T>` removed
/// the old compile-time tripwire that forced callers to supply the FK;
/// this restores the safety at the write path so a `Model { real_field,
/// ..Default::default() }` that forgets a required FK errors loudly
/// instead of silently INSERTing `FK = 0` (a dangling row).
///
/// Scope (v1): the i64-PK FK case only — the placeholder is a JSON
/// number equal to 0 (matching the `BigInt` arm of
/// [`crate::orm::write::is_default_pk`]). Uuid/Text-PK FKs are out of
/// scope and pass through untouched. Nullable FKs (`Option<ForeignKey>`
/// → `None`) are legitimate and never reach here as a 0 (they're
/// `null`); the PK column has its own `is_default_pk` guard.
fn reject_unset_fk_placeholder(
    field: &crate::orm::FieldSpec,
    val: &serde_json::Value,
) -> Result<(), crate::orm::write::WriteError> {
    let is_unset_fk = field.fk_target.is_some()
        && !field.nullable
        && !field.primary_key
        && val
            .as_i64()
            .map(|n| n == 0)
            .or_else(|| val.as_u64().map(|n| n == 0))
            .unwrap_or(false);
    if is_unset_fk {
        let target = field.fk_target.unwrap_or("the target");
        return Err(crate::orm::write::WriteError::Validator {
            field: field.name.to_string(),
            message: format!(
                "foreign key `{}` is the unset id-0 placeholder — set it to a real {} id before create",
                field.name, target
            ),
        });
    }
    Ok(())
}

/// Convert a `T: Serialize` instance to a `Map<String, Value>` for
/// the insert path. Errors out if the instance doesn't serialize to a
/// JSON object (only flat structs and HashMap-like shapes do).
pub(super) fn serialize_to_map<T: serde::Serialize>(
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
pub(super) fn build_insert_one_for<T: Model>(
    _backend_name: &str,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<sea_query::InsertStatement, crate::orm::write::WriteError> {
    use crate::orm::write::{is_default_pk, json_to_sea_value, now_for_column};
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
        // gaps2 #19 follow-up: `auto_now_add` / `auto_now` columns
        // are framework-managed timestamps. Django's behavior: the
        // INSERT path always writes `now()` regardless of what the
        // struct carries (the user can't override via Model.save()).
        // Without this overwrite, `Manager::create(instance)` where
        // `instance.created_at` was filled by `Default::default()`
        // (gaps2 #19's Form-derive `..Default::default()` tail) would
        // persist the epoch sentinel. The dynamic insert paths
        // (`insert_form` / `insert_json`) already do this; the typed
        // path was lagging.
        if field.auto_now_add || field.auto_now {
            columns.push(Alias::new(field.name));
            values.push(now_for_column(field.ty).into());
            continue;
        }
        // Skip absent fields when nullable (caller didn't supply them).
        if val.is_null() && field.nullable && !map.contains_key(field.name) {
            continue;
        }
        // Reject a non-nullable FK left at the id-0 unset placeholder
        // (ForeignKey::default) before binding it — see
        // [`reject_unset_fk_placeholder`].
        reject_unset_fk_placeholder(field, &val)?;
        let sea_value = json_to_sea_value(
            field.ty,
            &val,
            field.nullable,
            field.name,
            fk_pk_hint(field),
        )?;
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
pub(super) fn build_insert_many_for<T: Model>(
    _backend_name: &str,
    maps: &[serde_json::Map<String, serde_json::Value>],
) -> Result<sea_query::InsertStatement, crate::orm::write::WriteError> {
    use crate::orm::write::{is_default_pk, json_to_sea_value, now_for_column};
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
                // gaps2 #19 follow-up: bulk-insert path honors
                // `auto_now_add` / `auto_now` the same way the single-
                // row path does — every row's timestamp column gets
                // `now()` regardless of what the source struct carries.
                if field.auto_now_add || field.auto_now {
                    return Ok(sea_query::SimpleExpr::from(now_for_column(field.ty)));
                }
                let val = map
                    .get(field.name)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                // Reject a non-nullable FK left at the id-0 unset
                // placeholder before binding it (per-row, so one bad row
                // in the batch fails the whole bulk_create — no partial
                // insert of dangling FK rows). See
                // [`reject_unset_fk_placeholder`].
                reject_unset_fk_placeholder(field, &val)?;
                json_to_sea_value(
                    field.ty,
                    &val,
                    field.nullable,
                    field.name,
                    fk_pk_hint(field),
                )
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

/// Locate the primary-key FieldSpec for a model. Returns `None` if the
/// model has no PK (pathological — every macro-generated Model has one).
pub(super) fn pk_field<T: Model>() -> Option<&'static crate::orm::FieldSpec> {
    T::FIELDS.iter().find(|f| f.primary_key)
}
