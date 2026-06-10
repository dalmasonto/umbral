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
pub async fn validate_on_create(meta: &ModelMeta, body: &Map<String, Value>) -> Vec<WriteError> {
    let mut errors = validate_required_create(meta, body);
    errors.extend(validate_choices(meta, body));
    errors.extend(validate_m2m_relations(meta, body).await);
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
/// Typed-create equivalent of [`validate_on_create`]. Skips the
/// blank-string check (a Rust `String` field set to `""` is the
/// caller's deliberate choice on this path, not a form-default
/// leak), but still runs choices + FK existence + M2M shape —
/// the things a compile-time type can't catch.
pub async fn validate_on_typed_create(
    meta: &ModelMeta,
    body: &Map<String, Value>,
) -> Vec<WriteError> {
    let mut errors = Vec::new();
    errors.extend(validate_choices(meta, body));
    errors.extend(validate_m2m_relations(meta, body).await);
    let mut fk_errors = validate_fk_references(meta, body).await;
    let fk_fields: std::collections::HashSet<String> = fk_errors
        .iter()
        .filter_map(|e| match e {
            WriteError::ForeignKeyNotFound { field, .. } => Some(field.clone()),
            _ => None,
        })
        .collect();
    errors.retain(|e| match e {
        WriteError::Validator { field, .. } => !fk_fields.contains(field),
        _ => true,
    });
    errors.append(&mut fk_errors);
    errors
}

pub async fn validate_on_update(meta: &ModelMeta, body: &Map<String, Value>) -> Vec<WriteError> {
    let mut errors = validate_required_update(meta, body);
    errors.extend(validate_choices(meta, body));
    errors.extend(validate_m2m_relations(meta, body).await);
    let mut fk_errors = validate_fk_references(meta, body).await;
    let fk_fields: std::collections::HashSet<String> = fk_errors
        .iter()
        .filter_map(|e| match e {
            WriteError::ForeignKeyNotFound { field, .. } => Some(field.clone()),
            _ => None,
        })
        .collect();
    errors.retain(|e| match e {
        WriteError::RequiredFieldMissing { field } | WriteError::BlankNotAllowed { field } => {
            !fk_fields.contains(field)
        }
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
                Some(WriteError::UniqueViolation {
                    field: Some(col),
                    value,
                })
            } else if !cols.is_empty() {
                Some(WriteError::UniqueViolation {
                    field: Some(cols.into_iter().next().unwrap()),
                    value: None,
                })
            } else {
                Some(WriteError::UniqueViolation {
                    field: None,
                    value: None,
                })
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
            let value = pg_column.as_ref().and_then(|c| body.get(c)).cloned();
            Some(WriteError::UniqueViolation {
                field: pg_column,
                value,
            })
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

/// Reject body values that don't match a column's declared
/// `choices` set. Catches the typo case (`"usdd"` for a column
/// constrained to `["usd","eur","gbp",...]`) before the row
/// reaches the DB's CHECK constraint — the CHECK would still
/// catch it, but with a generic message; here we emit
/// `WriteError::Validator { field, message }` with the offending
/// value and the allowed set spelled out.
///
/// Blank / null / missing values are skipped — those are the
/// required-field check's job. Columns with an empty `choices`
/// list are unrestricted.
fn validate_choices(meta: &ModelMeta, body: &Map<String, Value>) -> Vec<WriteError> {
    let mut out = Vec::new();
    for col in &meta.fields {
        if col.choices.is_empty() {
            continue;
        }
        let Some(value) = body.get(&col.name) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let value_repr = match value {
            Value::String(s) if s.is_empty() => continue,
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => continue, // odd shape — let the DB sort it out
        };
        if col.choices.iter().any(|c| c == &value_repr) {
            continue;
        }
        out.push(WriteError::Validator {
            field: col.name.clone(),
            message: format!(
                "'{value_repr}' is not a valid choice. Allowed: {}.",
                col.choices.join(", "),
            ),
        });
    }
    out
}

/// Validate every M2M relation on the parent.
///
/// `Post.tags: M2M<Tag>` doesn't live on `model.fields` (it has
/// no column on `post`); it lives on `model.m2m_relations`. Two
/// failure modes:
///
/// - **Shape**: the body value must be an array (or null /
///   missing). `tags: 1` is wrong — caught as a structured type
///   error keyed under the M2M field name.
/// - **Existence**: every id in the array must reference a real
///   row in the target table. Missing rows surface keyed under
///   the M2M field with the full list of bad ids.
///
/// Both messages name the offending value so the client knows
/// exactly what to fix.
async fn validate_m2m_relations(meta: &ModelMeta, body: &Map<String, Value>) -> Vec<WriteError> {
    let mut out = Vec::new();
    for rel in &meta.m2m_relations {
        let Some(value) = body.get(&rel.field_name) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        // Shape: must be an array.
        let Some(items) = value.as_array() else {
            out.push(WriteError::Validator {
                field: rel.field_name.clone(),
                message: format!(
                    "Expected an array of `{}` ids, got {}.",
                    rel.target_name,
                    json_kind(value),
                ),
            });
            continue;
        };
        // Skip the registry lookup when there's nothing to check
        // — also avoids touching the registry in unit tests that
        // exercise the shape branch without booting an App.
        let to_check: Vec<&Value> = items.iter().filter(|v| !v.is_null()).collect();
        if to_check.is_empty() {
            continue;
        }
        // Existence: each id in the array must reference a real
        // row in the target table.
        let Some(target_meta) = model_meta_by_table(&rel.target_table) else {
            // Target not registered; fall back to silent skip —
            // the migration engine would have caught a real
            // typo at boot.
            continue;
        };
        let mut missing: Vec<Value> = Vec::new();
        for item in to_check {
            if !check_fk_row_exists(&target_meta, item).await {
                missing.push(item.clone());
            }
        }
        if !missing.is_empty() {
            let listed = missing
                .iter()
                .map(repr_json_value_local)
                .collect::<Vec<_>>()
                .join(", ");
            out.push(WriteError::Validator {
                field: rel.field_name.clone(),
                message: format!(
                    "Some referenced `{}` rows do not exist: {listed}.",
                    rel.target_name,
                ),
            });
        }
    }
    out
}

/// Lightweight string label for the JSON value's kind. Used in
/// shape-mismatch messages so the response reads
/// "got number" / "got object" instead of leaking the full
/// rendered value.
fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Same shape as the `repr_json_value` helper in `write.rs` but
/// usable inside this module without an extra pub. Strings get
/// quoted; numbers / bools / null bare; arrays / objects fall
/// back to compact JSON.
fn repr_json_value_local(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        _ => serde_json::to_string(v).unwrap_or_else(|_| "(?)".to_string()),
    }
}

async fn validate_fk_references(meta: &ModelMeta, body: &Map<String, Value>) -> Vec<WriteError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::Column;

    fn col(name: &str, choices: &[&str]) -> Column {
        Column {
            name: name.to_string(),
            ty: SqlType::Text,
            primary_key: false,
            nullable: false,
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            fk_target: None,
            on_delete: crate::orm::model::FkAction::NoAction,
            on_update: crate::orm::model::FkAction::NoAction,
            unique: false,
            default: String::new(),
            choices: choices.iter().map(|s| s.to_string()).collect(),
            choice_labels: Vec::new(),
            is_multichoice: false,
            min: None,
            max: None,
            text_format: None,
            slug_from: None,
            auto_now: false,
            auto_now_add: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            index: false,
            supported_backends: Vec::new(),
        }
    }

    fn meta_with(cols: Vec<Column>) -> ModelMeta {
        ModelMeta {
            name: "Test".into(),
            table: "test".into(),
            fields: cols,
            display: "Test".into(),
            icon: "database".into(),
            database: None,
            singleton: false,
            unique_together: Vec::new(),
            indexes: Vec::new(),
            ordering: Vec::new(),
            m2m_relations: Vec::new(),
        }
    }

    #[test]
    fn choices_validator_catches_typo_against_allowed_set() {
        let meta = meta_with(vec![col("currency", &["usd", "eur", "gbp"])]);
        let mut body = serde_json::Map::new();
        body.insert("currency".into(), serde_json::Value::String("usdd".into()));
        let errors = validate_choices(&meta, &body);
        assert_eq!(errors.len(), 1, "expected one error, got {errors:?}");
        match &errors[0] {
            WriteError::Validator { field, message } => {
                assert_eq!(field, "currency");
                assert!(
                    message.contains("'usdd'"),
                    "message should name the offending value; got {message:?}",
                );
                assert!(
                    message.contains("usd, eur, gbp"),
                    "message should list the allowed set; got {message:?}",
                );
            }
            other => panic!("expected Validator, got {other:?}"),
        }
    }

    #[test]
    fn choices_validator_accepts_a_known_value() {
        let meta = meta_with(vec![col("status", &["draft", "active", "archived"])]);
        let mut body = serde_json::Map::new();
        body.insert("status".into(), serde_json::Value::String("active".into()));
        assert!(validate_choices(&meta, &body).is_empty());
    }

    #[test]
    fn choices_validator_skips_blank_and_missing() {
        let meta = meta_with(vec![col("status", &["draft", "active"])]);
        // Missing key — required-field check handles it; choices
        // shouldn't double-report.
        assert!(validate_choices(&meta, &serde_json::Map::new()).is_empty());
        // Explicit null — same.
        let mut body = serde_json::Map::new();
        body.insert("status".into(), serde_json::Value::Null);
        assert!(validate_choices(&meta, &body).is_empty());
        // Empty string — required-field check's territory.
        let mut body = serde_json::Map::new();
        body.insert("status".into(), serde_json::Value::String(String::new()));
        assert!(validate_choices(&meta, &body).is_empty());
    }

    #[test]
    fn choices_validator_skips_columns_without_choices() {
        let meta = meta_with(vec![col("name", &[])]);
        let mut body = serde_json::Map::new();
        body.insert("name".into(), serde_json::Value::String("anything".into()));
        assert!(validate_choices(&meta, &body).is_empty());
    }

    // -------------------------------------------------------------
    // M2M shape validation. The existence check needs a registered
    // ModelMeta on the target table — not feasible here without
    // booting an App, so we exercise the shape branch only. The
    // happy-path FK-existence behaviour is covered by the live
    // integration tests in the REST plugin.
    // -------------------------------------------------------------

    fn meta_with_m2m(field_name: &str, target_table: &str, target_name: &str) -> ModelMeta {
        let mut meta = meta_with(vec![]);
        meta.m2m_relations.push(crate::migrate::M2MRelation {
            field_name: field_name.to_string(),
            target_table: target_table.to_string(),
            target_name: target_name.to_string(),
        });
        meta
    }

    #[tokio::test]
    async fn m2m_rejects_a_scalar_where_an_array_was_expected() {
        let meta = meta_with_m2m("tags", "tag", "Tag");
        let mut body = serde_json::Map::new();
        // `tags: 1` is the bug — a scalar where an array of ids
        // belongs. The framework should call this out, not let
        // the value silently disappear.
        body.insert("tags".into(), serde_json::Value::Number(1.into()));
        let errors = validate_m2m_relations(&meta, &body).await;
        assert_eq!(errors.len(), 1, "expected one shape error, got {errors:?}");
        match &errors[0] {
            WriteError::Validator { field, message } => {
                assert_eq!(field, "tags");
                assert!(
                    message.contains("array")
                        && message.contains("Tag")
                        && message.contains("number"),
                    "message should name the expected shape, the target, and \
                     the kind of the bad value; got {message:?}",
                );
            }
            other => panic!("expected Validator, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn m2m_accepts_an_empty_array() {
        let meta = meta_with_m2m("tags", "tag", "Tag");
        let mut body = serde_json::Map::new();
        body.insert("tags".into(), serde_json::Value::Array(Vec::new()));
        // Empty array is the "no children selected" shape — must
        // not produce an error; the framework just won't write any
        // junction rows.
        assert!(validate_m2m_relations(&meta, &body).await.is_empty());
    }

    #[tokio::test]
    async fn m2m_skips_null_and_missing_values() {
        let meta = meta_with_m2m("tags", "tag", "Tag");
        // Missing key — required-field path is the right home for
        // this (and M2M slots aren't required). Skipped here.
        assert!(
            validate_m2m_relations(&meta, &serde_json::Map::new())
                .await
                .is_empty()
        );
        // Explicit null — same.
        let mut body = serde_json::Map::new();
        body.insert("tags".into(), serde_json::Value::Null);
        assert!(validate_m2m_relations(&meta, &body).await.is_empty());
    }

    #[tokio::test]
    async fn m2m_rejects_an_object_too() {
        let meta = meta_with_m2m("tags", "tag", "Tag");
        let mut body = serde_json::Map::new();
        body.insert("tags".into(), serde_json::json!({ "id": 1 }));
        let errors = validate_m2m_relations(&meta, &body).await;
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            WriteError::Validator { field, message } => {
                assert_eq!(field, "tags");
                assert!(message.contains("object"));
            }
            other => panic!("expected Validator, got {other:?}"),
        }
    }
}
