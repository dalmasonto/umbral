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
//!   "umbral_dump_version": "1",
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
//!   present, which is what `umbral-cli migrate` is for).
//!
//! ## Deferred
//!
//! - Schema-snapshot embedding for forward-compat (the dump captures
//!   data only; the receiver needs a compatible schema).
//! - Streaming for very large databases.
//! - Selective dump / load with model filters.

use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::db::DbPool;
use crate::migrate::{Column, ModelMeta};
use crate::orm::{ArrayElement, SqlType, TsVector};

const DUMP_VERSION: &str = "1";

/// One table resolved against the live schema, paired with the dump
/// rows to load into it. Borrows the rows out of the [`Dump`] so the
/// restore never copies row data.
type ResolvedTable<'a> = (ModelMeta, &'a [Map<String, Value>]);

/// The on-disk envelope. `models` order is the order [`dump`] wrote
/// them in (sorted by table name for determinism). `exported_at` is
/// captured at dump time for traceability; [`load`] doesn't read it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dump {
    pub umbral_dump_version: String,
    pub exported_at: String,
    pub models: Vec<ModelDump>,
}

/// One table's worth of rows. The `table` field carries the SQL
/// table name (`Model::TABLE`), not the Rust struct name, so a load
/// against a schema that ran `#[umbral(table = "...")]` overrides
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
    /// The dump carries two entries for the same table. A merged or
    /// hand-edited dump would otherwise load the first entry and route
    /// the rest to `skipped_tables` (the "unknown schema" bucket),
    /// masking the duplicate. Fail loudly instead.
    DuplicateTable {
        table: String,
    },
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::Io(e) => write!(f, "umbral backup: io: {e}"),
            BackupError::Json(e) => write!(f, "umbral backup: json: {e}"),
            BackupError::Sqlx(e) => write!(f, "umbral backup: sqlx: {e}"),
            BackupError::UnsupportedVersion(v) => write!(
                f,
                "umbral backup: dump version `{v}` is not supported by this build \
                 (this build knows version `{DUMP_VERSION}`)"
            ),
            BackupError::UnknownColumn { table, column } => write!(
                f,
                "umbral backup: column `{table}.{column}` in the dump isn't in the \
                 current schema; run `umbral-cli migrate` first or update the dump"
            ),
            BackupError::TypeMismatch {
                table,
                column,
                expected,
                got,
            } => write!(
                f,
                "umbral backup: column `{table}.{column}` expects {expected:?} but the \
                 dump has {got}"
            ),
            BackupError::DuplicateTable { table } => write!(
                f,
                "umbral backup: dump contains two entries for table `{table}`; a dump must \
                 carry one entry per table (was it merged or hand-edited?)"
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
/// ambient pool (published by `App::build`) is the source.
pub async fn dump() -> Result<Dump, BackupError> {
    let pool = crate::db::pool_dispatched();
    let mut models = crate::migrate::registered_models();
    models.sort_by(|a, b| a.table.cmp(&b.table));

    let mut out: Vec<ModelDump> = Vec::with_capacity(models.len());
    for model in models {
        out.push(dump_one(pool, &model).await?);
    }
    Ok(Dump {
        umbral_dump_version: DUMP_VERSION.to_string(),
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
/// (run `umbral-cli migrate` first). Rows insert via `sqlx::query` with
/// per-column type dispatch; the ambient pool is the target.
pub async fn load(dump: &Dump) -> Result<LoadReport, BackupError> {
    if dump.umbral_dump_version != DUMP_VERSION {
        return Err(BackupError::UnsupportedVersion(
            dump.umbral_dump_version.clone(),
        ));
    }
    let pool = crate::db::pool_dispatched();
    let registered = crate::migrate::registered_models();
    let by_table: std::collections::HashMap<String, ModelMeta> = registered
        .into_iter()
        .map(|m| (m.table.clone(), m))
        .collect();

    let mut report = LoadReport::default();

    // Resolve every dump entry against the live schema. Duplicate table
    // entries are a hard error (was the dump merged / hand-edited?); an
    // entry whose table isn't in the schema is skipped with a warning
    // (a dump from a richer schema still restores the tables this build
    // knows). We look up WITHOUT removing so a legitimate duplicate is
    // caught here rather than silently routed to `skipped_tables`.
    let mut resolved: Vec<ResolvedTable<'_>> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for model in &dump.models {
        let Some(meta) = by_table.get(&model.table) else {
            report.skipped_tables.push(model.table.clone());
            continue;
        };
        if !seen.insert(model.table.clone()) {
            return Err(BackupError::DuplicateTable {
                table: model.table.clone(),
            });
        }
        resolved.push((meta.clone(), model.rows.as_slice()));
    }

    // Topologically order by FK dependency so a child table never loads
    // before its parent — the dump is written in alphabetical order,
    // which puts `comment` before `post` and fails FK checks on
    // restore. Cycles / self-references degrade to the dump's original
    // order for the affected nodes rather than erroring.
    let ordered = topo_order_by_fk(resolved);

    // One transaction for the whole restore so a mid-load failure rolls
    // back cleanly instead of leaving a half-populated database.
    match pool {
        DbPool::Sqlite(p) => load_all_sqlite(p, &ordered, &mut report).await?,
        DbPool::Postgres(p) => load_all_postgres(p, &ordered, &mut report).await?,
    }
    Ok(report)
}

/// Order the resolved `(meta, rows)` pairs so every table appears after
/// the tables its foreign keys reference. A Kahn-style walk: repeatedly
/// emit the nodes whose FK targets are all already emitted, preserving
/// the input order among ready nodes for determinism. FK targets that
/// aren't part of this load (e.g. a table not in the dump) impose no
/// ordering constraint. A dependency cycle (or a self-referential FK)
/// can't be fully ordered by table; the walk breaks it by emitting the
/// lowest-input-index remaining node, which reproduces the old
/// best-effort behaviour for those nodes.
fn topo_order_by_fk(items: Vec<ResolvedTable<'_>>) -> Vec<ResolvedTable<'_>> {
    let present: std::collections::HashSet<String> =
        items.iter().map(|(m, _)| m.table.clone()).collect();

    let mut deps: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for (m, _) in &items {
        let mut d = std::collections::HashSet::new();
        for col in &m.fields {
            if let Some(target) = &col.fk_target {
                if target != &m.table && present.contains(target) {
                    d.insert(target.clone());
                }
            }
        }
        deps.insert(m.table.clone(), d);
    }

    let order_index: std::collections::HashMap<String, usize> = items
        .iter()
        .enumerate()
        .map(|(i, (m, _))| (m.table.clone(), i))
        .collect();

    let mut emitted_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut emitted: Vec<String> = Vec::new();
    let mut remaining: Vec<String> = items.iter().map(|(m, _)| m.table.clone()).collect();

    while !remaining.is_empty() {
        let mut ready: Vec<String> = remaining
            .iter()
            .filter(|t| deps[*t].iter().all(|d| emitted_set.contains(d)))
            .cloned()
            .collect();
        if ready.is_empty() {
            // Cycle: break it deterministically by the lowest input index.
            let pick = remaining
                .iter()
                .min_by_key(|t| order_index[*t])
                .cloned()
                .expect("remaining is non-empty");
            ready.push(pick);
        }
        ready.sort_by_key(|t| order_index[t]);
        for t in ready {
            emitted_set.insert(t.clone());
            emitted.push(t.clone());
            remaining.retain(|x| x != &t);
        }
    }

    let mut by_table: std::collections::HashMap<String, ResolvedTable<'_>> = items
        .into_iter()
        .map(|(m, r)| (m.table.clone(), (m, r)))
        .collect();
    emitted
        .into_iter()
        .filter_map(|t| by_table.remove(&t))
        .collect()
}

async fn load_all_sqlite(
    pool: &sqlx::SqlitePool,
    ordered: &[ResolvedTable<'_>],
    report: &mut LoadReport,
) -> Result<(), BackupError> {
    let mut tx = pool.begin().await?;
    for (meta, rows) in ordered {
        let inserted = insert_rows_sqlite(&mut tx, meta, rows).await?;
        report.rows_loaded += inserted;
        report.tables_loaded.push(meta.table.clone());
    }
    tx.commit().await?;
    Ok(())
}

async fn load_all_postgres(
    pool: &sqlx::PgPool,
    ordered: &[ResolvedTable<'_>],
    report: &mut LoadReport,
) -> Result<(), BackupError> {
    let mut tx = pool.begin().await?;
    for (meta, rows) in ordered {
        let inserted = insert_rows_postgres(&mut tx, meta, rows).await?;
        report.rows_loaded += inserted;
        report.tables_loaded.push(meta.table.clone());
    }
    // A restore inserts explicit primary keys, but a BIGSERIAL sequence
    // still starts at 1 — the first ORM insert after restore then
    // collides on the PK. Advance each integer-PK table's sequence past
    // its restored max so new inserts don't duplicate a restored id.
    for (meta, _) in ordered {
        reset_pg_sequence(&mut tx, meta).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Advance a table's owning sequence past `MAX(pk)` on Postgres. Only
/// integer PKs own a BIGSERIAL sequence; String / Uuid / composite /
/// absent PKs are skipped. `pg_get_serial_sequence` returns NULL for an
/// integer PK that doesn't own a sequence (an app-assigned id), so the
/// NULL guard keeps `setval(NULL, ...)` from ever running.
async fn reset_pg_sequence(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    meta: &ModelMeta,
) -> Result<(), BackupError> {
    let Some(pk) = meta.fields.iter().find(|c| c.primary_key) else {
        return Ok(());
    };
    if !matches!(
        pk.ty,
        SqlType::BigInt | SqlType::Integer | SqlType::SmallInt
    ) {
        return Ok(());
    }
    let seq: Option<String> = sqlx::query_scalar("SELECT pg_get_serial_sequence($1, $2)")
        .bind(&meta.table)
        .bind(&pk.name)
        .fetch_one(&mut **tx)
        .await?;
    let Some(seq) = seq else {
        return Ok(());
    };
    // `is_called = false` means the NEXT nextval() returns exactly this
    // value, so `MAX(pk) + 1` is the next id handed out. On an empty
    // table MAX is NULL → COALESCE → 0 → next id is 1.
    let reset_sql = format!(
        "SELECT setval($1, COALESCE((SELECT MAX({pk}) FROM {tbl}), 0) + 1, false)",
        pk = quoted_ident(&pk.name),
        tbl = quoted_ident(&meta.table),
    );
    sqlx::query(&reset_sql)
        .bind(&seq)
        .fetch_one(&mut **tx)
        .await?;
    Ok(())
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

async fn dump_one(pool: &DbPool, model: &ModelMeta) -> Result<ModelDump, BackupError> {
    match pool {
        DbPool::Sqlite(pool) => dump_one_sqlite(pool, model).await,
        DbPool::Postgres(pool) => dump_one_postgres(pool, model).await,
    }
}

async fn dump_one_sqlite(
    pool: &sqlx::SqlitePool,
    model: &ModelMeta,
) -> Result<ModelDump, BackupError> {
    let sql = format!(
        "SELECT {} FROM {}",
        column_list(model),
        quoted_ident(&model.table)
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

async fn insert_rows_sqlite(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    model: &ModelMeta,
    rows: &[Map<String, Value>],
) -> Result<u64, BackupError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quoted_ident(&model.table),
        column_list(model),
        sqlite_placeholders(model.fields.len())
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
        q.execute(&mut **tx).await?;
        count += 1;
    }
    Ok(count)
}

async fn dump_one_postgres(
    pool: &sqlx::PgPool,
    model: &ModelMeta,
) -> Result<ModelDump, BackupError> {
    let sql = format!(
        "SELECT {} FROM {}",
        column_list_pg_select(model),
        quoted_ident(&model.table)
    );
    let rows = sqlx::query(&sql).fetch_all(pool).await?;

    let mut out: Vec<Map<String, Value>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut obj = Map::new();
        for col in &model.fields {
            obj.insert(col.name.clone(), column_to_json_pg(&row, col)?);
        }
        out.push(obj);
    }
    Ok(ModelDump {
        table: model.table.clone(),
        rows: out,
    })
}

async fn insert_rows_postgres(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    model: &ModelMeta,
    rows: &[Map<String, Value>],
) -> Result<u64, BackupError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quoted_ident(&model.table),
        column_list(model),
        postgres_placeholders(model.fields.len())
    );

    let mut count: u64 = 0;
    for row in rows {
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
            q = bind_value_pg(q, &model.table, col, val)?;
        }
        q.execute(&mut **tx).await?;
        count += 1;
    }
    Ok(count)
}

fn quoted_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn column_list(model: &ModelMeta) -> String {
    model
        .fields
        .iter()
        .map(|c| quoted_ident(&c.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Like [`column_list`] but, for the Postgres dump SELECT, casts the
/// text-backed Postgres-only types (`XML` / `LTREE` / `BIT VARYING`,
/// gaps2 #70) to `text` and re-aliases them to their column name so the
/// driver hands them back as a plain `String` (sqlx has no native
/// `Decode` for those column types into `String`). The cast is harmless
/// for every other column, so only the special types are wrapped.
fn column_list_pg_select(model: &ModelMeta) -> String {
    model
        .fields
        .iter()
        .map(|c| {
            if matches!(c.ty, SqlType::Xml | SqlType::Ltree | SqlType::Bit) {
                let q = quoted_ident(&c.name);
                format!("{q}::text AS {q}")
            } else {
                quoted_ident(&c.name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn sqlite_placeholders(count: usize) -> String {
    (0..count).map(|_| "?").collect::<Vec<_>>().join(", ")
}

fn postgres_placeholders(count: usize) -> String {
    (1..=count)
        .map(|idx| format!("${idx}"))
        .collect::<Vec<_>>()
        .join(", ")
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
        return Ok(match crate::migrate::fk_effective_type(col) {
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
            // gaps2 #70: text-backed Postgres types — backup's SQLite
            // path is unreachable for them (field.backend gates at boot).
            SqlType::Xml => unreachable_pg_only(&col.name, "Xml"),
            SqlType::Ltree => unreachable_pg_only(&col.name, "Ltree"),
            SqlType::Bit => unreachable_pg_only(&col.name, "Bit"),
            // ForeignKey stores as i64 — same as BigInt.
            SqlType::ForeignKey => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(Value::Null, Value::from),
            // BLOB / BYTEA. Backup format is a JSON array of u8
            // numbers — exactly the same shape `json_to_sea_value`
            // accepts on load.
            SqlType::Bytes => row
                .try_get::<Option<Vec<u8>>, _>(name)?
                .map_or(Value::Null, |b| {
                    Value::Array(b.into_iter().map(Value::from).collect())
                }),
            // BUG-10: Decimal is Postgres-only.
            SqlType::Decimal => unreachable_pg_only(&col.name, "Decimal"),
        });
    }
    // Non-nullable: same dispatch without the Option layer.
    Ok(match crate::migrate::fk_effective_type(col) {
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
        SqlType::Xml => unreachable_pg_only(&col.name, "Xml"),
        SqlType::Ltree => unreachable_pg_only(&col.name, "Ltree"),
        SqlType::Bit => unreachable_pg_only(&col.name, "Bit"),
        // ForeignKey stores as i64 — same as BigInt.
        SqlType::ForeignKey => Value::from(row.try_get::<i64, _>(name)?),
        SqlType::Bytes => {
            let bytes: Vec<u8> = row.try_get(name)?;
            Value::Array(bytes.into_iter().map(Value::from).collect())
        }
        SqlType::Decimal => unreachable_pg_only(&col.name, "Decimal"),
    })
}

fn column_to_json_pg(row: &sqlx::postgres::PgRow, col: &Column) -> Result<Value, BackupError> {
    let name = col.name.as_str();
    if col.nullable {
        return Ok(match crate::migrate::fk_effective_type(col) {
            SqlType::SmallInt => row
                .try_get::<Option<i16>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Integer => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::BigInt | SqlType::ForeignKey => row
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
            SqlType::Json => row
                .try_get::<Option<Value>, _>(name)?
                .unwrap_or(Value::Null),
            SqlType::Array(elem) => pg_array_column_to_json_nullable(row, name, elem)?,
            SqlType::Inet | SqlType::Cidr => row
                .try_get::<Option<IpNetwork>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::MacAddr => row
                .try_get::<Option<MacAddress>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::FullText => row
                .try_get::<Option<TsVector>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.into_inner())),
            // gaps2 #70: text-backed Postgres types dump via their text
            // form. The dump query casts these columns to `text` (see
            // `select_columns_sql`), so the driver hands back a `String`.
            SqlType::Xml | SqlType::Ltree | SqlType::Bit => row
                .try_get::<Option<String>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Bytes => row
                .try_get::<Option<Vec<u8>>, _>(name)?
                .map_or(Value::Null, bytes_to_json),
            SqlType::Decimal => row
                .try_get::<Option<Decimal>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
        });
    }
    Ok(match crate::migrate::fk_effective_type(col) {
        SqlType::SmallInt => Value::from(row.try_get::<i16, _>(name)?),
        SqlType::Integer => Value::from(row.try_get::<i32, _>(name)?),
        SqlType::BigInt | SqlType::ForeignKey => Value::from(row.try_get::<i64, _>(name)?),
        SqlType::Real => Value::from(row.try_get::<f32, _>(name)? as f64),
        SqlType::Double => Value::from(row.try_get::<f64, _>(name)?),
        SqlType::Boolean => Value::from(row.try_get::<bool, _>(name)?),
        SqlType::Text => Value::from(row.try_get::<String, _>(name)?),
        SqlType::Date => Value::from(row.try_get::<NaiveDate, _>(name)?.to_string()),
        SqlType::Time => Value::from(row.try_get::<NaiveTime, _>(name)?.to_string()),
        SqlType::Timestamptz => Value::from(row.try_get::<DateTime<Utc>, _>(name)?.to_rfc3339()),
        SqlType::Uuid => Value::from(row.try_get::<Uuid, _>(name)?.to_string()),
        SqlType::Json => row.try_get::<Value, _>(name)?,
        SqlType::Array(elem) => pg_array_column_to_json(row, name, elem)?,
        SqlType::Inet | SqlType::Cidr => {
            Value::from(row.try_get::<IpNetwork, _>(name)?.to_string())
        }
        SqlType::MacAddr => Value::from(row.try_get::<MacAddress, _>(name)?.to_string()),
        SqlType::FullText => Value::from(row.try_get::<TsVector, _>(name)?.into_inner()),
        // gaps2 #70: dump via the `::text` cast added in column_list_pg_select.
        SqlType::Xml | SqlType::Ltree | SqlType::Bit => {
            Value::from(row.try_get::<String, _>(name)?)
        }
        SqlType::Bytes => bytes_to_json(row.try_get::<Vec<u8>, _>(name)?),
        SqlType::Decimal => Value::from(row.try_get::<Decimal, _>(name)?.to_string()),
    })
}

fn pg_array_column_to_json_nullable(
    row: &sqlx::postgres::PgRow,
    name: &str,
    elem: ArrayElement,
) -> Result<Value, BackupError> {
    Ok(match elem {
        ArrayElement::SmallInt => row
            .try_get::<Option<Vec<i16>>, _>(name)?
            .map_or(Value::Null, |values| array_to_json(values, Value::from)),
        ArrayElement::Integer => row
            .try_get::<Option<Vec<i32>>, _>(name)?
            .map_or(Value::Null, |values| array_to_json(values, Value::from)),
        ArrayElement::BigInt => row
            .try_get::<Option<Vec<i64>>, _>(name)?
            .map_or(Value::Null, |values| array_to_json(values, Value::from)),
        ArrayElement::Real => row
            .try_get::<Option<Vec<f32>>, _>(name)?
            .map_or(Value::Null, |values| {
                array_to_json(values, |v| Value::from(v as f64))
            }),
        ArrayElement::Double => row
            .try_get::<Option<Vec<f64>>, _>(name)?
            .map_or(Value::Null, |values| array_to_json(values, Value::from)),
        ArrayElement::Boolean => row
            .try_get::<Option<Vec<bool>>, _>(name)?
            .map_or(Value::Null, |values| array_to_json(values, Value::from)),
        ArrayElement::Text => row
            .try_get::<Option<Vec<String>>, _>(name)?
            .map_or(Value::Null, |values| array_to_json(values, Value::from)),
        ArrayElement::Uuid => row
            .try_get::<Option<Vec<Uuid>>, _>(name)?
            .map_or(Value::Null, |values| {
                array_to_json(values, |v| Value::from(v.to_string()))
            }),
    })
}

fn pg_array_column_to_json(
    row: &sqlx::postgres::PgRow,
    name: &str,
    elem: ArrayElement,
) -> Result<Value, BackupError> {
    Ok(match elem {
        ArrayElement::SmallInt => array_to_json(row.try_get::<Vec<i16>, _>(name)?, Value::from),
        ArrayElement::Integer => array_to_json(row.try_get::<Vec<i32>, _>(name)?, Value::from),
        ArrayElement::BigInt => array_to_json(row.try_get::<Vec<i64>, _>(name)?, Value::from),
        ArrayElement::Real => {
            array_to_json(row.try_get::<Vec<f32>, _>(name)?, |v| Value::from(v as f64))
        }
        ArrayElement::Double => array_to_json(row.try_get::<Vec<f64>, _>(name)?, Value::from),
        ArrayElement::Boolean => array_to_json(row.try_get::<Vec<bool>, _>(name)?, Value::from),
        ArrayElement::Text => array_to_json(row.try_get::<Vec<String>, _>(name)?, Value::from),
        ArrayElement::Uuid => array_to_json(row.try_get::<Vec<Uuid>, _>(name)?, |v| {
            Value::from(v.to_string())
        }),
    })
}

fn array_to_json<T>(values: Vec<T>, mut item: impl FnMut(T) -> Value) -> Value {
    Value::Array(values.into_iter().map(&mut item).collect())
}

fn bytes_to_json(bytes: Vec<u8>) -> Value {
    Value::Array(bytes.into_iter().map(Value::from).collect())
}

/// Boot-path-bypassed sentinel. Array fields are Postgres-only — the
/// field.backend system check fires at App::build before any dump or
/// load runs against the SQLite pool. If we reach here, the boot path
/// was bypassed.
fn unreachable_array(column: &str) -> ! {
    panic!(
        "umbral backup: column `{column}` is a Postgres-only Array; \
         the field.backend system check should have failed boot. \
         For portable list storage use SqlType::Json instead."
    )
}

/// Phase 4.4 counterpart for Inet/Cidr/MacAddr — same gating story.
fn unreachable_network(column: &str) -> ! {
    panic!(
        "umbral backup: column `{column}` is a Postgres-only network \
         address type (Inet/Cidr/MacAddr); the field.backend system \
         check should have failed boot."
    )
}

/// Phase 4.3 generic sentinel for Postgres-only types (FullText today).
fn unreachable_pg_only(column: &str, type_name: &str) -> ! {
    panic!(
        "umbral backup: column `{column}` is a Postgres-only {type_name} \
         type; the field.backend system check should have failed boot."
    )
}

fn bind_value<'q>(
    q: SqliteQuery<'q>,
    table: &str,
    col: &Column,
    val: Value,
) -> Result<SqliteQuery<'q>, BackupError> {
    // Null binding is the same shape regardless of SqlType — SQLite
    // accepts a typed NULL on any column whose schema allows it.
    if matches!(val, Value::Null) {
        return Ok(match crate::migrate::fk_effective_type(col) {
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
            // gaps2 #70: text-backed Postgres types — backup's SQLite
            // path is unreachable for them (field.backend gates at boot).
            SqlType::Xml => unreachable_pg_only(&col.name, "Xml"),
            SqlType::Ltree => unreachable_pg_only(&col.name, "Ltree"),
            SqlType::Bit => unreachable_pg_only(&col.name, "Bit"),
            // ForeignKey stores as i64 — same as BigInt.
            SqlType::ForeignKey => q.bind(None::<i64>),
            SqlType::Bytes => q.bind(None::<Vec<u8>>),
            SqlType::Decimal => unreachable_pg_only(&col.name, "Decimal"),
        });
    }
    let mismatch = |got: &str| BackupError::TypeMismatch {
        table: table.to_string(),
        column: col.name.clone(),
        expected: col.ty,
        got: got.to_string(),
    };
    Ok(match crate::migrate::fk_effective_type(col) {
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
        SqlType::Xml => unreachable_pg_only(&col.name, "Xml"),
        SqlType::Ltree => unreachable_pg_only(&col.name, "Ltree"),
        SqlType::Bit => unreachable_pg_only(&col.name, "Bit"),
        // ForeignKey stores as i64 — same as BigInt.
        SqlType::ForeignKey => q.bind(val.as_i64().ok_or_else(|| mismatch(json_type_name(&val)))?),
        // BLOB: accept a JSON array of u8 numbers — the same shape the
        // dump path emits.
        SqlType::Bytes => q.bind(bytes_from_json(table, col, &val)?),
        SqlType::Decimal => unreachable_pg_only(&col.name, "Decimal"),
    })
}

type SqliteQuery<'q> = sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>;
type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

fn bind_value_pg<'q>(
    q: PgQuery<'q>,
    table: &str,
    col: &Column,
    val: Value,
) -> Result<PgQuery<'q>, BackupError> {
    if matches!(val, Value::Null) {
        return Ok(match crate::migrate::fk_effective_type(col) {
            SqlType::SmallInt => q.bind(None::<i16>),
            SqlType::Integer => q.bind(None::<i32>),
            SqlType::BigInt | SqlType::ForeignKey => q.bind(None::<i64>),
            SqlType::Real => q.bind(None::<f32>),
            SqlType::Double => q.bind(None::<f64>),
            SqlType::Boolean => q.bind(None::<bool>),
            SqlType::Text => q.bind(None::<String>),
            SqlType::Date => q.bind(None::<NaiveDate>),
            SqlType::Time => q.bind(None::<NaiveTime>),
            SqlType::Timestamptz => q.bind(None::<DateTime<Utc>>),
            SqlType::Uuid => q.bind(None::<Uuid>),
            SqlType::Json => q.bind(None::<Value>),
            SqlType::Array(elem) => bind_null_array_pg(q, elem),
            SqlType::Inet | SqlType::Cidr => q.bind(None::<IpNetwork>),
            SqlType::MacAddr => q.bind(None::<MacAddress>),
            SqlType::FullText => q.bind(None::<TsVector>),
            // gaps2 #70: text-backed types bind their NULL as a text
            // parameter; Postgres applies the column's assignment cast.
            SqlType::Xml | SqlType::Ltree | SqlType::Bit => q.bind(None::<String>),
            SqlType::Bytes => q.bind(None::<Vec<u8>>),
            SqlType::Decimal => q.bind(None::<Decimal>),
        });
    }
    let mismatch = |got: &str| BackupError::TypeMismatch {
        table: table.to_string(),
        column: col.name.clone(),
        expected: col.ty,
        got: got.to_string(),
    };
    Ok(match crate::migrate::fk_effective_type(col) {
        SqlType::SmallInt => q.bind(
            i16::try_from(val.as_i64().ok_or_else(|| mismatch(json_type_name(&val)))?)
                .map_err(|_| mismatch("number out of i16 range"))?,
        ),
        SqlType::Integer => q.bind(
            i32::try_from(val.as_i64().ok_or_else(|| mismatch(json_type_name(&val)))?)
                .map_err(|_| mismatch("number out of i32 range"))?,
        ),
        SqlType::BigInt | SqlType::ForeignKey => {
            q.bind(val.as_i64().ok_or_else(|| mismatch(json_type_name(&val)))?)
        }
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
        SqlType::Json => q.bind(val),
        SqlType::Array(elem) => bind_array_pg(q, table, col, elem, &val)?,
        SqlType::Inet | SqlType::Cidr => {
            let s = val.as_str().ok_or_else(|| mismatch(json_type_name(&val)))?;
            q.bind(IpNetwork::from_str(s).map_err(|_| mismatch("invalid network string"))?)
        }
        SqlType::MacAddr => {
            let s = val.as_str().ok_or_else(|| mismatch(json_type_name(&val)))?;
            q.bind(MacAddress::from_str(s).map_err(|_| mismatch("invalid macaddr string"))?)
        }
        SqlType::FullText => {
            let s = val.as_str().ok_or_else(|| mismatch(json_type_name(&val)))?;
            q.bind(TsVector::from(s))
        }
        // gaps2 #70: text-backed types bind their string form; Postgres
        // applies the column's assignment cast (text → xml / ltree /
        // bit) on insert.
        SqlType::Xml | SqlType::Ltree | SqlType::Bit => {
            let s = val.as_str().ok_or_else(|| mismatch(json_type_name(&val)))?;
            q.bind(s.to_string())
        }
        SqlType::Bytes => q.bind(bytes_from_json(table, col, &val)?),
        SqlType::Decimal => {
            let parsed = match &val {
                Value::String(s) => Decimal::from_str(s).ok(),
                Value::Number(n) => Decimal::from_str(&n.to_string()).ok(),
                _ => None,
            };
            q.bind(parsed.ok_or_else(|| mismatch(json_type_name(&val)))?)
        }
    })
}

fn bind_null_array_pg<'q>(q: PgQuery<'q>, elem: ArrayElement) -> PgQuery<'q> {
    match elem {
        ArrayElement::SmallInt => q.bind(None::<Vec<i16>>),
        ArrayElement::Integer => q.bind(None::<Vec<i32>>),
        ArrayElement::BigInt => q.bind(None::<Vec<i64>>),
        ArrayElement::Real => q.bind(None::<Vec<f32>>),
        ArrayElement::Double => q.bind(None::<Vec<f64>>),
        ArrayElement::Boolean => q.bind(None::<Vec<bool>>),
        ArrayElement::Text => q.bind(None::<Vec<String>>),
        ArrayElement::Uuid => q.bind(None::<Vec<Uuid>>),
    }
}

fn bind_array_pg<'q>(
    q: PgQuery<'q>,
    table: &str,
    col: &Column,
    elem: ArrayElement,
    val: &Value,
) -> Result<PgQuery<'q>, BackupError> {
    Ok(match elem {
        ArrayElement::SmallInt => q.bind(
            int_array_from_json(table, col, val)?
                .into_iter()
                .map(|n| {
                    i16::try_from(n)
                        .map_err(|_| type_mismatch(table, col, "element out of i16 range"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        ArrayElement::Integer => q.bind(
            int_array_from_json(table, col, val)?
                .into_iter()
                .map(|n| {
                    i32::try_from(n)
                        .map_err(|_| type_mismatch(table, col, "element out of i32 range"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        ArrayElement::BigInt => q.bind(int_array_from_json(table, col, val)?),
        ArrayElement::Real => q.bind(
            float_array_from_json(table, col, val)?
                .into_iter()
                .map(|n| n as f32)
                .collect::<Vec<_>>(),
        ),
        ArrayElement::Double => q.bind(float_array_from_json(table, col, val)?),
        ArrayElement::Boolean => q.bind(
            array_values(table, col, val)?
                .iter()
                .map(|v| {
                    v.as_bool()
                        .ok_or_else(|| type_mismatch(table, col, "non-boolean in array"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        ArrayElement::Text => q.bind(
            array_values(table, col, val)?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(ToString::to_string)
                        .ok_or_else(|| type_mismatch(table, col, "non-string in array"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        ArrayElement::Uuid => q.bind(
            array_values(table, col, val)?
                .iter()
                .map(|v| {
                    let s = v
                        .as_str()
                        .ok_or_else(|| type_mismatch(table, col, "non-string uuid in array"))?;
                    Uuid::parse_str(s)
                        .map_err(|_| type_mismatch(table, col, "invalid uuid string in array"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
    })
}

fn array_values<'a>(
    table: &str,
    col: &Column,
    val: &'a Value,
) -> Result<&'a Vec<Value>, BackupError> {
    val.as_array()
        .ok_or_else(|| type_mismatch(table, col, json_type_name(val)))
}

fn int_array_from_json(table: &str, col: &Column, val: &Value) -> Result<Vec<i64>, BackupError> {
    array_values(table, col, val)?
        .iter()
        .map(|v| {
            v.as_i64()
                .ok_or_else(|| type_mismatch(table, col, "non-integer in array"))
        })
        .collect()
}

fn float_array_from_json(table: &str, col: &Column, val: &Value) -> Result<Vec<f64>, BackupError> {
    array_values(table, col, val)?
        .iter()
        .map(|v| {
            v.as_f64()
                .ok_or_else(|| type_mismatch(table, col, "non-number in array"))
        })
        .collect()
}

fn bytes_from_json(table: &str, col: &Column, val: &Value) -> Result<Vec<u8>, BackupError> {
    let arr = val
        .as_array()
        .ok_or_else(|| type_mismatch(table, col, json_type_name(val)))?;
    let mut bytes: Vec<u8> = Vec::with_capacity(arr.len());
    for v in arr {
        let n = v
            .as_u64()
            .ok_or_else(|| type_mismatch(table, col, "non-number in bytes array"))?;
        if n > 255 {
            return Err(type_mismatch(table, col, "element out of u8 range"));
        }
        bytes.push(n as u8);
    }
    Ok(bytes)
}

fn type_mismatch(table: &str, col: &Column, got: impl Into<String>) -> BackupError {
    BackupError::TypeMismatch {
        table: table.to_string(),
        column: col.name.clone(),
        expected: col.ty,
        got: got.into(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_generation_matches_backend_syntax() {
        assert_eq!(sqlite_placeholders(3), "?, ?, ?");
        assert_eq!(postgres_placeholders(3), "$1, $2, $3");
    }

    #[test]
    fn quoted_ident_escapes_double_quotes() {
        assert_eq!(quoted_ident("plain"), "\"plain\"");
        assert_eq!(quoted_ident("weird\"name"), "\"weird\"\"name\"");
    }
}
