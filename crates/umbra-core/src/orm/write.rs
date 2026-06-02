//! Model write-side primitives — INSERT, UPDATE, DELETE.
//!
//! This module owns the conversion path **JSON value → sea_query::Value**
//! that lets the write methods on `Manager` and `QuerySet` accept
//! either a serialized model instance (for create / bulk_create) or a
//! `serde_json::Map<String, Value>` of column-name → value pairs (for
//! `update_values`). Both shapes converge on the same per-SqlType
//! dispatch and the same SQL generation through sea-query.
//!
//! ## Why JSON in the middle
//!
//! Users derive `serde::Serialize` on their models for REST anyway,
//! so `Manager::create(instance)` can call `serde_json::to_value`
//! once and then dispatch each field against its column's `SqlType`.
//! No second derive macro or custom trait method is required.
//!
//! For `QuerySet::update_values(map)` the caller is already producing
//! a `Map<String, Value>` (often from request bodies — admin form
//! posts, REST PATCH payloads), so accepting that shape directly is
//! the least-friction surface.
//!
//! Both paths share [`json_to_sea_value`], so the per-type
//! conversion is written once.
//!
//! ## Why not just bind through sqlx directly
//!
//! The existing `umbra-rest` plugin binds JSON values straight to
//! `sqlx::query::Query` via [`bind_json_value`] (in `plugins/umbra-
//! rest/src/lib.rs`). That works only against `sqlx::Sqlite`. The
//! umbra-core write methods support both backends (SQLite +
//! Postgres), so they go through sea-query's typed Value enum, which
//! `build_sqlx` then binds against whichever backend the resolved
//! pool dictates. REST keeps its sqlite-only path until a future
//! consolidation lifts it through here.

use sea_query::Value as SeaValue;
use serde_json::Value as JsonValue;

use crate::orm::SqlType;

/// Errors that can surface when converting JSON values to bindable
/// sea-query values, or when the write itself fails.
#[derive(Debug)]
pub enum WriteError {
    /// A non-nullable field received a JSON `null` (or was absent on
    /// create). Names the offending field.
    RequiredFieldMissing { field: String },
    /// The JSON value couldn't be coerced to the column's SqlType.
    /// e.g. a string body where an integer was expected.
    TypeMismatch {
        field: String,
        expected: SqlType,
        got: String,
    },
    /// `serde_json` couldn't serialize the instance to a JSON
    /// object (the only shape `Manager::create` accepts).
    NotAnObject,
    /// The model isn't `Serialize`. Surfaced by the trait bound on
    /// `Manager::create`; not actually constructable from runtime.
    /// Kept here for completeness so the variant exists in the docs.
    SerializeFailed(serde_json::Error),
    /// sqlx error during the write. Wraps the driver-level cause.
    Sqlx(sqlx::Error),
    /// `update_values` received a column name that doesn't exist on
    /// the model. Caught early before SQL is built.
    UnknownColumn { field: String },
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::RequiredFieldMissing { field } => {
                write!(
                    f,
                    "umbra::orm::write: required field `{field}` is missing or null"
                )
            }
            WriteError::TypeMismatch {
                field,
                expected,
                got,
            } => write!(
                f,
                "umbra::orm::write: field `{field}` expected `{expected:?}`, got `{got}`",
            ),
            WriteError::NotAnObject => write!(
                f,
                "umbra::orm::write: model didn't serialize to a JSON object — make sure your struct uses a flat field layout",
            ),
            WriteError::SerializeFailed(e) => write!(f, "umbra::orm::write: serialize: {e}"),
            WriteError::Sqlx(e) => write!(f, "umbra::orm::write: sqlx: {e}"),
            WriteError::UnknownColumn { field } => {
                write!(f, "umbra::orm::write: unknown column `{field}` on model")
            }
        }
    }
}

impl std::error::Error for WriteError {}

impl From<sqlx::Error> for WriteError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl From<serde_json::Error> for WriteError {
    fn from(e: serde_json::Error) -> Self {
        Self::SerializeFailed(e)
    }
}

/// Convert a `serde_json::Value` to a `sea_query::Value` per the
/// column's declared `SqlType`. The `nullable` flag controls how
/// `JsonValue::Null` is handled:
///
/// - `nullable = true`: NULL is bound (the right SeaValue variant
///   with `None`).
/// - `nullable = false`: NULL produces `RequiredFieldMissing`.
///
/// String / number coercions follow the HTML-form-and-REST norms:
/// `"true"` / `"false"` strings coerce to booleans, `"123"` strings
/// coerce to numbers. RFC 3339 timestamps come through as strings on
/// JSON inputs (serde_json doesn't have a native datetime).
pub fn json_to_sea_value(
    sql_type: SqlType,
    value: &JsonValue,
    nullable: bool,
    field_name: &str,
) -> Result<SeaValue, WriteError> {
    // null handling first — applies regardless of expected type.
    if value.is_null() {
        if !nullable {
            return Err(WriteError::RequiredFieldMissing {
                field: field_name.to_string(),
            });
        }
        return Ok(null_for(sql_type));
    }

    match sql_type {
        SqlType::Boolean => coerce_bool(value, field_name),
        SqlType::SmallInt | SqlType::Integer => {
            coerce_i32(value, field_name).map(|v| SeaValue::Int(Some(v)))
        }
        // ForeignKey columns store i64 — same path as BigInt.
        SqlType::BigInt | SqlType::ForeignKey => {
            coerce_i64(value, field_name).map(|v| SeaValue::BigInt(Some(v)))
        }
        SqlType::Real => coerce_f32(value, field_name).map(|v| SeaValue::Float(Some(v))),
        SqlType::Double => coerce_f64(value, field_name).map(|v| SeaValue::Double(Some(v))),
        SqlType::Text => {
            coerce_string(value, field_name).map(|s| SeaValue::String(Some(Box::new(s))))
        }
        SqlType::Date => {
            let s = coerce_string(value, field_name)?;
            let d = chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").map_err(|_| {
                WriteError::TypeMismatch {
                    field: field_name.to_string(),
                    expected: sql_type,
                    got: format!("{value:?}"),
                }
            })?;
            Ok(SeaValue::ChronoDate(Some(Box::new(d))))
        }
        SqlType::Time => {
            let s = coerce_string(value, field_name)?;
            let t = chrono::NaiveTime::parse_from_str(&s, "%H:%M:%S")
                .or_else(|_| chrono::NaiveTime::parse_from_str(&s, "%H:%M"))
                .map_err(|_| WriteError::TypeMismatch {
                    field: field_name.to_string(),
                    expected: sql_type,
                    got: format!("{value:?}"),
                })?;
            Ok(SeaValue::ChronoTime(Some(Box::new(t))))
        }
        SqlType::Timestamptz => {
            let s = coerce_string(value, field_name)?;
            let dt = chrono::DateTime::parse_from_rfc3339(&s)
                .map_err(|_| WriteError::TypeMismatch {
                    field: field_name.to_string(),
                    expected: sql_type,
                    got: format!("{value:?}"),
                })?
                .with_timezone(&chrono::Utc);
            Ok(SeaValue::ChronoDateTimeUtc(Some(Box::new(dt))))
        }
        SqlType::Uuid => {
            let s = coerce_string(value, field_name)?;
            let u = uuid::Uuid::parse_str(&s).map_err(|_| WriteError::TypeMismatch {
                field: field_name.to_string(),
                expected: sql_type,
                got: format!("{value:?}"),
            })?;
            Ok(SeaValue::Uuid(Some(Box::new(u))))
        }
        SqlType::Json => {
            // Store the JSON as-is — sqlx-sqlite will TEXT it, sqlx-pg
            // will jsonb-encode it. sea-query has a Json variant when
            // its `with-json` feature is on; we're going through the
            // string path for portability.
            Ok(SeaValue::String(Some(Box::new(value.to_string()))))
        }
        // Postgres-only catalogue. Returned as a serialized string;
        // the per-backend bind layer downstream handles the cast.
        // These paths are only reachable for PG-bound models (the
        // field.backend check at App::build blocks SQLite).
        SqlType::Array(_)
        | SqlType::Inet
        | SqlType::Cidr
        | SqlType::MacAddr
        | SqlType::FullText => Ok(SeaValue::String(Some(Box::new(coerce_string(
            value, field_name,
        )?)))),
        // BLOB / BYTEA. JSON wire shape: an array of u8 numbers, the
        // natural way to encode a byte string in JSON without picking
        // a base16/base64 convention at the framework level.
        // Hex-encoded JSON strings also accepted as a convenience for
        // human-readable test fixtures.
        SqlType::Bytes => coerce_bytes(value, field_name)
            .map(|b| SeaValue::Bytes(Some(Box::new(b)))),
    }
}

/// Coerce a `serde_json::Value` to `Vec<u8>`. Accepts:
///   - `[1, 2, 3, ...]` — JSON array of u8-shaped numbers.
///   - `"deadbeef"` — lowercase hex string of even length.
fn coerce_bytes(value: &JsonValue, field_name: &str) -> Result<Vec<u8>, WriteError> {
    if let Some(arr) = value.as_array() {
        let mut out = Vec::with_capacity(arr.len());
        for v in arr {
            let n = v.as_u64().ok_or_else(|| WriteError::TypeMismatch {
                field: field_name.to_string(),
                expected: SqlType::Bytes,
                got: format!("{v:?}"),
            })?;
            if n > 255 {
                return Err(WriteError::TypeMismatch {
                    field: field_name.to_string(),
                    expected: SqlType::Bytes,
                    got: format!("element {v} out of u8 range"),
                });
            }
            out.push(n as u8);
        }
        return Ok(out);
    }
    if let Some(s) = value.as_str() {
        if s.len() % 2 != 0 {
            return Err(WriteError::TypeMismatch {
                field: field_name.to_string(),
                expected: SqlType::Bytes,
                got: "hex string has odd length".to_string(),
            });
        }
        let mut out = Vec::with_capacity(s.len() / 2);
        for chunk in s.as_bytes().chunks(2) {
            let high = hex_nibble(chunk[0]).ok_or_else(|| WriteError::TypeMismatch {
                field: field_name.to_string(),
                expected: SqlType::Bytes,
                got: format!("non-hex char `{}`", chunk[0] as char),
            })?;
            let low = hex_nibble(chunk[1]).ok_or_else(|| WriteError::TypeMismatch {
                field: field_name.to_string(),
                expected: SqlType::Bytes,
                got: format!("non-hex char `{}`", chunk[1] as char),
            })?;
            out.push((high << 4) | low);
        }
        return Ok(out);
    }
    Err(WriteError::TypeMismatch {
        field: field_name.to_string(),
        expected: SqlType::Bytes,
        got: format!("{value:?}"),
    })
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + b - b'a'),
        b'A'..=b'F' => Some(10 + b - b'A'),
        _ => None,
    }
}

/// Sea-query value representing SQL NULL for the given SqlType. The
/// variant tag matters for sea-query's encoding even when the inner
/// option is `None`.
pub(crate) fn null_for(sql_type: SqlType) -> SeaValue {
    match sql_type {
        SqlType::Boolean => SeaValue::Bool(None),
        SqlType::SmallInt | SqlType::Integer => SeaValue::Int(None),
        SqlType::BigInt | SqlType::ForeignKey => SeaValue::BigInt(None),
        SqlType::Real => SeaValue::Float(None),
        SqlType::Double => SeaValue::Double(None),
        SqlType::Text | SqlType::Json => SeaValue::String(None),
        SqlType::Date => SeaValue::ChronoDate(None),
        SqlType::Time => SeaValue::ChronoTime(None),
        SqlType::Timestamptz => SeaValue::ChronoDateTimeUtc(None),
        SqlType::Uuid => SeaValue::Uuid(None),
        SqlType::Array(_)
        | SqlType::Inet
        | SqlType::Cidr
        | SqlType::MacAddr
        | SqlType::FullText => SeaValue::String(None),
        SqlType::Bytes => SeaValue::Bytes(None),
    }
}

fn coerce_bool(value: &JsonValue, field_name: &str) -> Result<SeaValue, WriteError> {
    match value {
        JsonValue::Bool(b) => Ok(SeaValue::Bool(Some(*b))),
        JsonValue::String(s) => match s.as_str() {
            "true" | "1" | "yes" | "on" => Ok(SeaValue::Bool(Some(true))),
            "false" | "0" | "no" | "off" | "" => Ok(SeaValue::Bool(Some(false))),
            _ => Err(WriteError::TypeMismatch {
                field: field_name.to_string(),
                expected: SqlType::Boolean,
                got: format!("{value:?}"),
            }),
        },
        JsonValue::Number(n) => Ok(SeaValue::Bool(Some(n.as_i64() != Some(0)))),
        _ => Err(WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::Boolean,
            got: format!("{value:?}"),
        }),
    }
}

fn coerce_i32(value: &JsonValue, field_name: &str) -> Result<i32, WriteError> {
    match value {
        JsonValue::Number(n) => n
            .as_i64()
            .and_then(|i| i32::try_from(i).ok())
            .ok_or_else(|| WriteError::TypeMismatch {
                field: field_name.to_string(),
                expected: SqlType::Integer,
                got: format!("{value:?}"),
            }),
        JsonValue::String(s) => s.parse::<i32>().map_err(|_| WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::Integer,
            got: s.clone(),
        }),
        _ => Err(WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::Integer,
            got: format!("{value:?}"),
        }),
    }
}

fn coerce_i64(value: &JsonValue, field_name: &str) -> Result<i64, WriteError> {
    match value {
        JsonValue::Number(n) => n.as_i64().ok_or_else(|| WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::BigInt,
            got: format!("{value:?}"),
        }),
        JsonValue::String(s) => s.parse::<i64>().map_err(|_| WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::BigInt,
            got: s.clone(),
        }),
        _ => Err(WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::BigInt,
            got: format!("{value:?}"),
        }),
    }
}

fn coerce_f32(value: &JsonValue, field_name: &str) -> Result<f32, WriteError> {
    coerce_f64(value, field_name).map(|v| v as f32)
}

fn coerce_f64(value: &JsonValue, field_name: &str) -> Result<f64, WriteError> {
    match value {
        JsonValue::Number(n) => n.as_f64().ok_or_else(|| WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::Double,
            got: format!("{value:?}"),
        }),
        JsonValue::String(s) => s.parse::<f64>().map_err(|_| WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::Double,
            got: s.clone(),
        }),
        _ => Err(WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::Double,
            got: format!("{value:?}"),
        }),
    }
}

fn coerce_string(value: &JsonValue, field_name: &str) -> Result<String, WriteError> {
    match value {
        JsonValue::String(s) => Ok(s.clone()),
        JsonValue::Number(n) => Ok(n.to_string()),
        JsonValue::Bool(b) => Ok(b.to_string()),
        _ => Err(WriteError::TypeMismatch {
            field: field_name.to_string(),
            expected: SqlType::Text,
            got: format!("{value:?}"),
        }),
    }
}

/// Error type for the signal-firing per-instance write methods
/// ([`Manager::save`] and [`Manager::delete_instance`]).
///
/// Wraps [`WriteError`] for the underlying SQL errors and adds one
/// framework-level variant for models with no primary key declared.
#[derive(Debug)]
pub enum SaveError {
    /// The model has no field with `primary_key = true`. Returned
    /// by `save` and `delete_instance` which need the PK to build
    /// the WHERE clause for UPDATE / DELETE.
    NoPrimaryKey,
    /// An underlying write-layer error (type mismatch, sqlx error,
    /// etc.). See [`WriteError`] for the full variant list.
    Write(WriteError),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::NoPrimaryKey => write!(
                f,
                "umbra::orm::save: model has no primary key — cannot determine INSERT vs UPDATE"
            ),
            SaveError::Write(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SaveError::Write(e) => Some(e),
            _ => None,
        }
    }
}

impl From<WriteError> for SaveError {
    fn from(e: WriteError) -> Self {
        Self::Write(e)
    }
}

/// True when this JSON value represents the "default" PK that should
/// trigger autoincrement rather than be bound as an explicit value.
///
/// Conventions:
/// - Integer PK: 0 is the autoincrement sentinel (matches Django's
///   default, matches SQLite's `INTEGER PRIMARY KEY AUTOINCREMENT`).
/// - UUID PK: nil / all-zeros UUID is the sentinel.
/// - String PK: empty string. Users with non-empty string PKs always
///   supply them; an empty string makes no sense as a real PK.
pub fn is_default_pk(sql_type: SqlType, value: &JsonValue) -> bool {
    match (sql_type, value) {
        (SqlType::SmallInt | SqlType::Integer | SqlType::BigInt, JsonValue::Number(n)) => {
            n.as_i64() == Some(0) || n.as_u64() == Some(0)
        }
        (SqlType::Uuid, JsonValue::String(s)) => {
            s == "00000000-0000-0000-0000-000000000000" || s.is_empty()
        }
        (SqlType::Text, JsonValue::String(s)) => s.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_to_sea_value_passes_basic_types() {
        let v = json_to_sea_value(SqlType::Integer, &json!(42), false, "x").unwrap();
        assert!(matches!(v, SeaValue::Int(Some(42))));
        let v = json_to_sea_value(SqlType::BigInt, &json!(42), false, "x").unwrap();
        assert!(matches!(v, SeaValue::BigInt(Some(42))));
        let v = json_to_sea_value(SqlType::Text, &json!("hi"), false, "x").unwrap();
        assert!(matches!(v, SeaValue::String(Some(_))));
        let v = json_to_sea_value(SqlType::Boolean, &json!(true), false, "x").unwrap();
        assert!(matches!(v, SeaValue::Bool(Some(true))));
    }

    #[test]
    fn json_to_sea_value_coerces_string_booleans() {
        let v = json_to_sea_value(SqlType::Boolean, &json!("true"), false, "x").unwrap();
        assert!(matches!(v, SeaValue::Bool(Some(true))));
        let v = json_to_sea_value(SqlType::Boolean, &json!("0"), false, "x").unwrap();
        assert!(matches!(v, SeaValue::Bool(Some(false))));
    }

    #[test]
    fn json_to_sea_value_rejects_null_on_required_field() {
        let err = json_to_sea_value(SqlType::Integer, &json!(null), false, "x").unwrap_err();
        assert!(matches!(err, WriteError::RequiredFieldMissing { .. }));
    }

    #[test]
    fn json_to_sea_value_accepts_null_on_nullable_field() {
        let v = json_to_sea_value(SqlType::Integer, &json!(null), true, "x").unwrap();
        assert!(matches!(v, SeaValue::Int(None)));
    }

    #[test]
    fn is_default_pk_recognises_zero_int_and_nil_uuid() {
        assert!(is_default_pk(SqlType::Integer, &json!(0)));
        assert!(is_default_pk(SqlType::BigInt, &json!(0)));
        assert!(!is_default_pk(SqlType::BigInt, &json!(42)));
        assert!(is_default_pk(
            SqlType::Uuid,
            &json!("00000000-0000-0000-0000-000000000000")
        ));
        assert!(!is_default_pk(SqlType::Uuid, &json!("not-zero")));
    }
}
