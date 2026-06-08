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
pub(super) fn build_insert_many_for<T: Model>(
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

/// Locate the primary-key FieldSpec for a model. Returns `None` if the
/// model has no PK (pathological — every macro-generated Model has one).
pub(super) fn pk_field<T: Model>() -> Option<&'static crate::orm::FieldSpec> {
    T::FIELDS.iter().find(|f| f.primary_key)
}
