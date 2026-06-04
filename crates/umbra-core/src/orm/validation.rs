//! Pre- and post-DB validation helpers used by
//! [`DynQuerySet::insert_json`] and [`DynQuerySet::update_json`].
//!
//! Three jobs:
//!
//! 1. **Required-field check.** Walks the model's columns and
//!    rejects any required column that's missing or blank in the
//!    body. Empty strings on Text columns count as blank
//!    (Django's CharField with `blank=False`); JSON `{}` does
//!    not (legitimate JSONB value). FK numeric `0` does NOT —
//!    that's the FK existence check's job, where the truthful
//!    message ("row not found") replaces a synthetic "required."
//!
//! 2. **Foreign-key existence check.** For every FK column in the
//!    body, queries the target table to verify the referenced row
//!    exists. Emits a [`WriteError::ForeignKeyNotFound`] per bad
//!    reference so the REST / admin layer can render an inline
//!    "Referenced X row with id=Y not found" message keyed under
//!    the FK column.
//!
//! 3. **SQL constraint classification.** After the INSERT /
//!    UPDATE runs, any [`sqlx::Error::Database`] gets walked
//!    through the SQLite / Postgres error-code tables and turned
//!    into one of `UniqueViolation` / `NotNullViolation` /
//!    `CheckViolation` / `ForeignKeyViolation`. The body-aware
//!    paths thread the original JSON value into the message so
//!    the response says "slug='widget'" rather than the generic
//!    "this value."
//!
//! The REST plugin used to own all of these; centralising them
//! here means the admin plugin and any third-party caller get the
//! same structured errors without re-implementing the wheel.

use serde_json::{Map, Value};

use crate::migrate::{Column, ModelMeta};
use crate::orm::model::SqlType;
use crate::orm::write::WriteError;

/// Run every pre-DB check the ORM owns for a create-shaped body.
/// Returns the merged list of errors so the caller's response can
/// surface required + FK problems in the same round-trip.
///
/// The FK check is async because it queries the target tables;
/// the required check is sync. Callers that want only one or the
/// other reach for the individual helpers.
pub async fn validate_on_create(
    meta: &ModelMeta,
    body: &Map<String, Value>,
) -> Vec<WriteError> {
    let mut errors = validate_required_create(meta, body);
    let mut fk_errors = validate_fk_references(meta, body).await;
    let fk_fields: std::collections::HashSet<String> = fk_errors
        .iter()
        .filter_map(|e| match e {
            WriteError::ForeignKeyNotFound { field, .. } => Some(field.clone()),
            _ => None,
        })
        .collect();
    // When a field has both a "required" and an "FK not found"
    // error, drop the "required" — the FK message is the more
    // specific failure mode. The client knew to send a value;
    // the value just doesn't reference a real row.
    errors.retain(|e| match e {
        WriteError::RequiredFieldMissing { field } => !fk_fields.contains(field),
        WriteError::BlankNotAllowed { field } => !fk_fields.contains(field),
        _ => true,
    });
    errors.append(&mut fk_errors);
    errors
}

/// Update-shaped equivalent. Required-field check only fires on
/// fields the client EXPLICITLY sent as blank — missing keys are
/// fine per the partial-update contract.
pub async fn validate_on_update(
    meta: &ModelMeta,
    body: &Map<String, Value>,
) -> Vec<WriteError> {
    let mut errors = validate_required_update(meta, body);
    let mut fk_errors = validate_fk_references(meta, body).await;
    let fk_fields: std::collections::HashSet<String> = fk_errors
        .iter()
        .filter_map(|e| match e {
            WriteError::ForeignKeyNotFound { field, .. } => Some(field.clone()),
            _ => None,
        })
        .collect();
    errors.retain(|e| match e {
        WriteError::RequiredFieldMissing { field }
        | WriteError::BlankNotAllowed { field } => !fk_fields.contains(field),
        _ => true,
    });
    errors.append(&mut fk_errors);
    errors
}

/// Classify a `sqlx::Error::Database` constraint failure into a
/// structured `WriteError`. The original body is used to lift the
/// offending value into the response message. Returns `None`
/// when the error doesn't match a known constraint code; the
/// caller's `WriteError::Sqlx(e)` fallback handles that case
/// (and surfaces as a 500 at the REST boundary).
pub fn classify_sql_error(e: &sqlx::Error, body: &Map<String, Value>) -> Option<WriteError> {
    let db_err = e.as_database_error()?;
    let sql_code = db_err.code()?;
    let sql_code = sql_code.as_ref();
    let message = db_err.message();
    let pg_column = db_err
        .constraint()
        .and_then(parse_pg_column_from_constraint);

    match sql_code {
        // ---- SQLite ----
        "787" => Some(WriteError::ForeignKeyViolation { field: None }),
        "2067" => {
            // "UNIQUE constraint failed: tbl.col[, tbl.col]"
            let cols = sqlite_columns_from_message(message, "UNIQUE constraint failed:");
            // For a single-column UNIQUE we can name a value; for
            // composite uniques we report the first column with
            // no value (multiple ambiguity).
            if cols.len() == 1 {
                let col = cols.into_iter().next().unwrap();
                let value = body.get(&col).cloned();
                Some(WriteError::UniqueViolation { field: Some(col), value })
            } else if !cols.is_empty() {
                Some(WriteError::UniqueViolation {
                    field: Some(cols.into_iter().next().unwrap()),
                    value: None,
                })
            } else {
                Some(WriteError::UniqueViolation { field: None, value: None })
            }
        }
        "1299" => {
            let cols = sqlite_columns_from_message(message, "NOT NULL constraint failed:");
            Some(WriteError::NotNullViolation {
                field: cols.into_iter().next(),
            })
        }
        "275" => Some(WriteError::CheckViolation { constraint: None }),

        // ---- Postgres ----
        "23503" => Some(WriteError::ForeignKeyViolation { field: pg_column }),
        "23505" => {
            let value = pg_column
                .as_ref()
                .and_then(|c| body.get(c))
                .cloned();
            Some(WriteError::UniqueViolation { field: pg_column, value })
        }
        "23502" => Some(WriteError::NotNullViolation { field: pg_column }),
        "23514" => Some(WriteError::CheckViolation {
            constraint: db_err.constraint().map(String::from),
        }),
        _ => None,
    }
}

// =========================================================================
// Internals.
// =========================================================================

fn validate_required_create(meta: &ModelMeta, body: &Map<String, Value>) -> Vec<WriteError> {
    let mut out = Vec::new();
    for col in &meta.fields {
        if !column_is_required(col) {
            continue;
        }
        match body.get(&col.name) {
            None | Some(Value::Null) => {
                out.push(WriteError::RequiredFieldMissing {
                    field: col.name.clone(),
                });
            }
            Some(Value::String(s)) if value_is_blank_for_type(s, col.ty) => {
                if matches!(col.ty, SqlType::Text) {
                    out.push(WriteError::BlankNotAllowed {
                        field: col.name.clone(),
                    });
                } else {
                    // Empty-string placeholder where a typed value
                    // belongs — caller didn't fill the form in.
                    out.push(WriteError::RequiredFieldMissing {
                        field: col.name.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn validate_required_update(meta: &ModelMeta, body: &Map<String, Value>) -> Vec<WriteError> {
    let mut out = Vec::new();
    for col in &meta.fields {
        if !column_is_required(col) {
            continue;
        }
        // Update: only complain when the client EXPLICITLY sent a
        // blank value. Missing keys preserve the existing row.
        let Some(value) = body.get(&col.name) else {
            continue;
        };
        match value {
            Value::Null => {
                out.push(WriteError::RequiredFieldMissing {
                    field: col.name.clone(),
                });
            }
            Value::String(s) if value_is_blank_for_type(s, col.ty) => {
                if matches!(col.ty, SqlType::Text) {
                    out.push(WriteError::BlankNotAllowed {
                        field: col.name.clone(),
                    });
                } else {
                    out.push(WriteError::RequiredFieldMissing {
                        field: col.name.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

async fn validate_fk_references(
    meta: &ModelMeta,
    body: &Map<String, Value>,
) -> Vec<WriteError> {
    let mut out = Vec::new();
    for col in &meta.fields {
        let Some(target_table) = col.fk_target.as_deref() else {
            continue;
        };
        let Some(value) = body.get(&col.name) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let Some(target_meta) = model_meta_by_table(target_table) else {
            // Target not registered with the migration engine;
            // fall back to DB-side enforcement.
            continue;
        };
        if !check_fk_row_exists(&target_meta, value).await {
            out.push(WriteError::ForeignKeyNotFound {
                field: col.name.clone(),
                target_table: target_table.to_string(),
                value: value.clone(),
            });
        }
    }
    out
}

fn column_is_required(col: &Column) -> bool {
    !col.primary_key
        && !col.noform
        && !col.nullable
        && col.default.is_empty()
        && !col.auto_now
        && !col.auto_now_add
}

/// `true` when the JSON string is "blank" in the sense of "the
/// caller sent a placeholder where a value should be." Empty
/// strings on Text columns count; whitespace-only on typed
/// columns counts; everything else is a real value.
fn value_is_blank_for_type(s: &str, ty: SqlType) -> bool {
    if s.is_empty() {
        return true;
    }
    matches!(
        ty,
        SqlType::SmallInt
            | SqlType::Integer
            | SqlType::BigInt
            | SqlType::Real
            | SqlType::Double
            | SqlType::Boolean
            | SqlType::Date
            | SqlType::Time
            | SqlType::Timestamptz
            | SqlType::Uuid
            | SqlType::ForeignKey,
    ) && s.trim().is_empty()
}

/// Look up a [`ModelMeta`] by SQL table name. Walks the migration
/// registry — same source the REST plugin's allow/deny check uses.
fn model_meta_by_table(table: &str) -> Option<ModelMeta> {
    for plugin in crate::migrate::registered_plugins() {
        for meta in crate::migrate::models_for_plugin(&plugin) {
            if meta.table == table {
                return Some(meta);
            }
        }
    }
    None
}

/// Single-row existence check against the FK target table. Uses
/// `DynQuerySet::count` so both SQLite and Postgres work without
/// raw SQL.
async fn check_fk_row_exists(target: &ModelMeta, value: &Value) -> bool {
    let Some(pk) = target.fields.iter().find(|c| c.primary_key) else {
        return true;
    };
    let pk_repr = match value {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        _ => return true,
    };
    let count = crate::orm::dynamic::DynQuerySet::for_meta(target)
        .filter_eq_string(&pk.name, &pk_repr)
        .count()
        .await
        .unwrap_or(0);
    count > 0
}

/// Parse SQLite constraint messages of the form
/// `<prefix> tbl.col1[, tbl.col2 ...]` into a `Vec<col>`.
fn sqlite_columns_from_message(message: &str, prefix: &str) -> Vec<String> {
    let trimmed_prefix = prefix.trim();
    let Some(suffix) = message
        .strip_prefix(trimmed_prefix)
        .or_else(|| message.strip_prefix(prefix))
    else {
        return Vec::new();
    };
    suffix
        .split(',')
        .map(str::trim)
        .filter_map(|seg| seg.split('.').nth(1))
        .map(str::to_string)
        .collect()
}

/// Pull the column name out of a Postgres constraint identifier
/// shaped like `<table>_<col>_<kind>` where `<kind>` is one of
/// `fkey` / `key` (unique) / `check`. Returns `None` when the
/// shape doesn't match.
fn parse_pg_column_from_constraint(constraint: &str) -> Option<String> {
    for suffix in ["_fkey", "_key", "_check"] {
        if let Some(rest) = constraint.strip_suffix(suffix) {
            return rest.split_once('_').map(|(_, col)| col.to_string());
        }
    }
    None
}
