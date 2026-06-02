//! Backup and recovery: dump every registered model's rows to JSON,
//! load them back.
//!
//! The two halves are symmetric. [`dump`] walks
//! `migrate::registered_models()`, runs `SELECT * FROM <table>` for
//! each, and dispatches per column's [`SqlType`] to read every value
//! out as a `serde_json::Value`. [`load`] reads the JSON back and
//! inserts each row through `sqlx::query` with the same per-column
//! dispatch on the binding side.
//!
//! The on-disk format is one JSON document with a small envelope:
//!
//! ```json
//! {
//!   "umbra_dump_version": "1",
//!   "exported_at": "2026-05-30T17:00:00Z",
//!   "models": [
//!     { "table": "post", "rows": [{"id": 1, "title": "..."}] },
//!     { "table": "tag",  "rows": [{"id": 1, "name": "..."}] }
//!   ]
//! }
//! ```
//!
//! ## v1 scope
//!
//! - Every `SqlType` variant in the M3 catalogue: integer widths,
//!   floats, bool, text, date/time/timestamptz, uuid, plus their
//!   nullable forms.
//! - One-shot dump + load. No partial dumps, no streaming.
//! - Order-independent: `load` doesn't assume a particular model
//!   sequence; rows insert into existing tables (the schema must be
//!   present, which is what `umbra-cli migrate` is for).
//!
//! ## Deferred
//!
//! - Schema-snapshot embedding for forward-compat (the dump captures
//!   data only; the receiver needs a compatible schema).
//! - Streaming for very large databases.
//! - Selective dump / load with model filters.

use std::path::Path;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::migrate::{Column, ModelMeta};
use crate::orm::SqlType;

const DUMP_VERSION: &str = "1";

/// The on-disk envelope. `models` order is the order [`dump`] wrote
/// them in (sorted by table name for determinism). `exported_at` is
/// captured at dump time for traceability; [`load`] doesn't read it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dump {
    pub umbra_dump_version: String,
    pub exported_at: String,
    pub models: Vec<ModelDump>,
}

/// One table's worth of rows. The `table` field carries the SQL
/// table name (`Model::TABLE`), not the Rust struct name, so a load
/// against a schema that ran `#[umbra(table = "...")]` overrides
/// still finds the right destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDump {
    pub table: String,
    pub rows: Vec<Map<String, Value>>,
}

/// Errors the dump / load pipeline can produce.
#[derive(Debug)]
pub enum BackupError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Sqlx(sqlx::Error),
    /// Dump version doesn't match what this build knows how to load.
    /// The version string in the file is included for the diagnostic.
    UnsupportedVersion(String),
    /// A column in the loaded JSON doesn't exist on the model's
    /// schema. Surfaced so a forward-incompatible dump fails loudly
    /// instead of silently skipping data.
    UnknownColumn {
        table: String,
        column: String,
    },
    /// A value in the loaded JSON doesn't match the expected
    /// `SqlType` shape (e.g. a string where the schema wants an
    /// integer). Carries the table / column / observed value type
    /// for the diagnostic.
    TypeMismatch {
        table: String,
        column: String,
        expected: SqlType,
        got: String,
    },
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::Io(e) => write!(f, "umbra backup: io: {e}"),
            BackupError::Json(e) => write!(f, "umbra backup: json: {e}"),
            BackupError::Sqlx(e) => write!(f, "umbra backup: sqlx: {e}"),
            BackupError::UnsupportedVersion(v) => write!(
                f,
                "umbra backup: dump version `{v}` is not supported by this build \
                 (this build knows version `{DUMP_VERSION}`)"
            ),
            BackupError::UnknownColumn { table, column } => write!(
                f,
                "umbra backup: column `{table}.{column}` in the dump isn't in the \
                 current schema; run `umbra-cli migrate` first or update the dump"
            ),
            BackupError::TypeMismatch {
                table,
                column,
                expected,
                got,
            } => write!(
                f,
                "umbra backup: column `{table}.{column}` expects {expected:?} but the \
                 dump has {got}"
            ),
        }
    }
}

impl std::error::Error for BackupError {}

impl From<std::io::Error> for BackupError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for BackupError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<sqlx::Error> for BackupError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

/// Dump every registered model's rows to a [`Dump`] value. The
/// ambient SQLite pool (published by `App::build`) is the source.
pub async fn dump() -> Result<Dump, BackupError> {
    let pool = crate::db::pool();
    let mut models = crate::migrate::registered_models();
    models.sort_by(|a, b| a.table.cmp(&b.table));

    let mut out: Vec<ModelDump> = Vec::with_capacity(models.len());
    for model in models {
        out.push(dump_one(&pool, &model).await?);
    }
    Ok(Dump {
        umbra_dump_version: DUMP_VERSION.to_string(),
        exported_at: Utc::now().to_rfc3339(),
        models: out,
    })
}

/// Convenience: dump and write the JSON to `path`.
pub async fn dump_to_path(path: &Path) -> Result<(), BackupError> {
    let dump = dump().await?;
    let json = serde_json::to_string_pretty(&dump)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Load a [`Dump`] back into the database. Schema must already exist
/// (run `umbra-cli migrate` first). Rows insert via `sqlx::query` with
/// per-column type dispatch; the ambient SQLite pool is the target.
pub async fn load(dump: &Dump) -> Result<LoadReport, BackupError> {
    if dump.umbra_dump_version != DUMP_VERSION {
        return Err(BackupError::UnsupportedVersion(
            dump.umbra_dump_version.clone(),
        ));
    }
    let pool = crate::db::pool();
    let registered = crate::migrate::registered_models();
    let mut by_table: std::collections::HashMap<String, ModelMeta> = registered
        .into_iter()
        .map(|m| (m.table.clone(), m))
        .collect();

    let mut report = LoadReport::default();
    for model in &dump.models {
        let Some(meta) = by_table.remove(&model.table) else {
            // Unknown table in dump. Skip with a warning rather than
            // erroring — a dump from a newer schema is still useful
            // for the tables this build does know about.
            report.skipped_tables.push(model.table.clone());
            continue;
        };
        let inserted = load_one(&pool, &meta, &model.rows).await?;
        report.rows_loaded += inserted;
        report.tables_loaded.push(meta.table);
    }
    Ok(report)
}

/// Convenience: read the JSON from `path` and load it.
pub async fn load_from_path(path: &Path) -> Result<LoadReport, BackupError> {
    let text = std::fs::read_to_string(path)?;
    let dump: Dump = serde_json::from_str(&text)?;
    load(&dump).await
}

/// What [`load`] did. Tables present in the dump but not in the
/// current schema land in `skipped_tables` (not an error; the dump
/// might be from a richer schema).
#[derive(Debug, Default, Clone)]
pub struct LoadReport {
    pub tables_loaded: Vec<String>,
    pub skipped_tables: Vec<String>,
    pub rows_loaded: u64,
}

// =========================================================================
// Per-table dispatch.
// =========================================================================

async fn dump_one(pool: &sqlx::SqlitePool, model: &ModelMeta) -> Result<ModelDump, BackupError> {
    let column_list = model
        .fields
        .iter()
        .map(|c| format!("\"{}\"", c.name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {column_list} FROM \"{}\"",
        model.table.replace('"', "\"\"")
    );
    let rows = sqlx::query(&sql).fetch_all(pool).await?;

    let mut out: Vec<Map<String, Value>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut obj = Map::new();
        for col in &model.fields {
            obj.insert(col.name.clone(), column_to_json(&row, col)?);
        }
        out.push(obj);
    }
    Ok(ModelDump {
        table: model.table.clone(),
        rows: out,
    })
}

async fn load_one(
    pool: &sqlx::SqlitePool,
    model: &ModelMeta,
    rows: &[Map<String, Value>],
) -> Result<u64, BackupError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let placeholders = model
        .fields
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let column_list = model
        .fields
        .iter()
        .map(|c| format!("\"{}\"", c.name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO \"{}\" ({column_list}) VALUES ({placeholders})",
        model.table.replace('"', "\"\"")
    );

    let mut count: u64 = 0;
    for row in rows {
        // Surface unknown columns in the dump explicitly so a forward-
        // incompatible dump fails loudly instead of silently dropping data.
        for k in row.keys() {
            if !model.fields.iter().any(|c| &c.name == k) {
                return Err(BackupError::UnknownColumn {
                    table: model.table.clone(),
                    column: k.clone(),
                });
            }
        }
        let mut q = sqlx::query(&sql);
        for col in &model.fields {
            let val = row.get(&col.name).cloned().unwrap_or(Value::Null);
            q = bind_value(q, &model.table, col, val)?;
        }
        q.execute(pool).await?;
        count += 1;
    }
    Ok(count)
}

// =========================================================================
// Column-level dispatch on SqlType. The dump-side reader and the load-side
// binder mirror each other variant-for-variant.
// =========================================================================

fn column_to_json(row: &sqlx::sqlite::SqliteRow, col: &Column) -> Result<Value, BackupError> {
    let name = col.name.as_str();
    // The nullable path always tries Option<T>. SQLite stores NULL
    // explicitly so `try_get::<Option<T>>` is the safe read.
    if col.nullable {
        return Ok(match col.ty {
            SqlType::SmallInt | SqlType::Integer => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::BigInt => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Real => row
                .try_get::<Option<f32>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v as f64)),
            SqlType::Double => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Boolean => row
                .try_get::<Option<bool>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Text => row
                .try_get::<Option<String>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Date => row
                .try_get::<Option<NaiveDate>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Time => row
                .try_get::<Option<NaiveTime>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Timestamptz => row
                .try_get::<Option<DateTime<Utc>>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_rfc3339())),
            SqlType::Uuid => row
                .try_get::<Option<Uuid>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            // The Json column already holds a serde_json::Value; the
            // dump is the value itself (no string-wrapping). Reading via
            // `try_get::<Option<Value>, _>` round-trips JSONB on Postgres
            // and JSON-as-TEXT on SQLite via sqlx's `json` feature.
            SqlType::Json => row
                .try_get::<Option<Value>, _>(name)?
                .unwrap_or(Value::Null),
            // Array fields are Postgres-only and backup runs against
            // the SQLite pool. The field.backend system check gates
            // them at boot; reaching this means the boot path was
            // bypassed.
            SqlType::Array(_) => unreachable_array(&col.name),
            SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => unreachable_network(&col.name),
            SqlType::FullText => unreachable_pg_only(&col.name, "FullText (tsvector)"),
            // ForeignKey stores as i64 — same as BigInt.
            SqlType::ForeignKey => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(Value::Null, Value::from),
            // BLOB / BYTEA. Backup format is a JSON array of u8
            // numbers — exactly the same shape `json_to_sea_value`
            // accepts on load.
            SqlType::Bytes => row.try_get::<Option<Vec<u8>>, _>(name)?.map_or(
                Value::Null,
                |b| Value::Array(b.into_iter().map(Value::from).collect()),
            ),
        });
    }
    // Non-nullable: same dispatch without the Option layer.
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => Value::from(row.try_get::<i32, _>(name)?),
        SqlType::BigInt => Value::from(row.try_get::<i64, _>(name)?),
        SqlType::Real => Value::from(row.try_get::<f32, _>(name)? as f64),
        SqlType::Double => Value::from(row.try_get::<f64, _>(name)?),
        SqlType::Boolean => Value::from(row.try_get::<bool, _>(name)?),
        SqlType::Text => Value::from(row.try_get::<String, _>(name)?),
        SqlType::Date => Value::from(row.try_get::<NaiveDate, _>(name)?.to_string()),
        SqlType::Time => Value::from(row.try_get::<NaiveTime, _>(name)?.to_string()),
        SqlType::Timestamptz => Value::from(row.try_get::<DateTime<Utc>, _>(name)?.to_rfc3339()),
        SqlType::Uuid => Value::from(row.try_get::<Uuid, _>(name)?.to_string()),
        SqlType::Json => row.try_get::<Value, _>(name)?,
        SqlType::Array(_) => unreachable_array(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => unreachable_network(&col.name),
        SqlType::FullText => unreachable_pg_only(&col.name, "FullText (tsvector)"),
        // ForeignKey stores as i64 — same as BigInt.
        SqlType::ForeignKey => Value::from(row.try_get::<i64, _>(name)?),
        SqlType::Bytes => {
            let bytes: Vec<u8> = row.try_get(name)?;
            Value::Array(bytes.into_iter().map(Value::from).collect())
        }
    })
}

/// Boot-path-bypassed sentinel. Array fields are Postgres-only — the
/// field.backend system check fires at App::build before any dump or
/// load runs against the SQLite pool. If we reach here, the boot path
/// was bypassed.
fn unreachable_array(column: &str) -> ! {
    panic!(
        "umbra backup: column `{column}` is a Postgres-only Array; \
         the field.backend system check should have failed boot. \
         For portable list storage use SqlType::Json instead."
    )
}

/// Phase 4.4 counterpart for Inet/Cidr/MacAddr — same gating story.
fn unreachable_network(column: &str) -> ! {
    panic!(
        "umbra backup: column `{column}` is a Postgres-only network \
         address type (Inet/Cidr/MacAddr); the field.backend system \
         check should have failed boot."
    )
}

/// Phase 4.3 generic sentinel for Postgres-only types (FullText today).
fn unreachable_pg_only(column: &str, type_name: &str) -> ! {
    panic!(
        "umbra backup: column `{column}` is a Postgres-only {type_name} \
         type; the field.backend system check should have failed boot."
    )
}

fn bind_value<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    table: &str,
    col: &Column,
    val: Value,
) -> Result<sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>, BackupError> {
    // Null binding is the same shape regardless of SqlType — SQLite
    // accepts a typed NULL on any column whose schema allows it.
    if matches!(val, Value::Null) {
        return Ok(match col.ty {
            SqlType::SmallInt | SqlType::Integer => q.bind(None::<i32>),
            SqlType::BigInt => q.bind(None::<i64>),
            SqlType::Real => q.bind(None::<f32>),
            SqlType::Double => q.bind(None::<f64>),
            SqlType::Boolean => q.bind(None::<bool>),
            SqlType::Text => q.bind(None::<String>),
            SqlType::Date => q.bind(None::<NaiveDate>),
            SqlType::Time => q.bind(None::<NaiveTime>),
            SqlType::Timestamptz => q.bind(None::<DateTime<Utc>>),
            SqlType::Uuid => q.bind(None::<Uuid>),
            SqlType::Json => q.bind(None::<Value>),
            SqlType::Array(_) => unreachable_array(&col.name),
            SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => unreachable_network(&col.name),
            SqlType::FullText => unreachable_pg_only(&col.name, "FullText (tsvector)"),
            // ForeignKey stores as i64 — same as BigInt.
            SqlType::ForeignKey => q.bind(None::<i64>),
            SqlType::Bytes => q.bind(None::<Vec<u8>>),
        });
    }
    let mismatch = |got: &str| BackupError::TypeMismatch {
        table: table.to_string(),
        column: col.name.clone(),
        expected: col.ty,
        got: got.to_string(),
    };
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => {
            q.bind(val.as_i64().ok_or_else(|| mismatch(json_type_name(&val)))? as i32)
        }
        SqlType::BigInt => q.bind(val.as_i64().ok_or_else(|| mismatch(json_type_name(&val)))?),
        SqlType::Real => q.bind(val.as_f64().ok_or_else(|| mismatch(json_type_name(&val)))? as f32),
        SqlType::Double => q.bind(val.as_f64().ok_or_else(|| mismatch(json_type_name(&val)))?),
        SqlType::Boolean => q.bind(
            val.as_bool()
                .ok_or_else(|| mismatch(json_type_name(&val)))?,
        ),
        SqlType::Text => q.bind(
            val.as_str()
                .ok_or_else(|| mismatch(json_type_name(&val)))?
                .to_string(),
        ),
        SqlType::Date => {
            let s = val.as_str().ok_or_else(|| mismatch(json_type_name(&val)))?;
            q.bind(
                s.parse::<NaiveDate>()
                    .map_err(|_| mismatch("invalid date string"))?,
            )
        }
        SqlType::Time => {
            let s = val.as_str().ok_or_else(|| mismatch(json_type_name(&val)))?;
            q.bind(
                s.parse::<NaiveTime>()
                    .map_err(|_| mismatch("invalid time string"))?,
            )
        }
        SqlType::Timestamptz => {
            let s = val.as_str().ok_or_else(|| mismatch(json_type_name(&val)))?;
            q.bind(
                DateTime::parse_from_rfc3339(s)
                    .map_err(|_| mismatch("invalid rfc3339 timestamp"))?
                    .with_timezone(&Utc),
            )
        }
        SqlType::Uuid => {
            let s = val.as_str().ok_or_else(|| mismatch(json_type_name(&val)))?;
            q.bind(Uuid::parse_str(s).map_err(|_| mismatch("invalid uuid string"))?)
        }
        // Json columns hold a serde_json::Value verbatim — no string
        // wrapping or parsing dance. sqlx's `json` feature handles the
        // encode side: the Value serializes to JSON text (SQLite) or
        // a JSONB byte stream (Postgres) before hitting the wire.
        SqlType::Json => q.bind(val),
        SqlType::Array(_) => unreachable_array(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => unreachable_network(&col.name),
        SqlType::FullText => unreachable_pg_only(&col.name, "FullText (tsvector)"),
        // ForeignKey stores as i64 — same as BigInt.
        SqlType::ForeignKey => q.bind(val.as_i64().ok_or_else(|| mismatch(json_type_name(&val)))?),
        // BLOB: accept a JSON array of u8 numbers — the same shape the
        // dump path emits.
        SqlType::Bytes => {
            let arr = val.as_array().ok_or_else(|| mismatch(json_type_name(&val)))?;
            let mut bytes: Vec<u8> = Vec::with_capacity(arr.len());
            for v in arr {
                let n = v.as_u64().ok_or_else(|| mismatch("non-number in bytes array"))?;
                if n > 255 {
                    return Err(mismatch("element out of u8 range"));
                }
                bytes.push(n as u8);
            }
            q.bind(bytes)
        }
    })
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
