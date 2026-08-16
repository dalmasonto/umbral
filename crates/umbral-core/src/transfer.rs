//! Data transfer engine: a resumable, PK-preserving, streaming row copy between
//! two umbral databases (env1 -> env2). See
//! `docs/decisions/2026-08-16-data-transfer-engine.md`.
//!
//! Both ends share the app's registered schema. Rows are copied verbatim —
//! primary keys and foreign keys preserved — so the object graph is identical
//! on the target. Tables are copied in FK-topological order (parents first),
//! each streamed in keyset-paginated batches. Every batch commits its inserts
//! AND its resume checkpoint in one target transaction, so an interrupted run
//! resumes exactly where it stopped with no duplicate rows.

use std::collections::{HashMap, HashSet};

use sea_query::{Alias, Expr};

use crate::db::DbPool;
use crate::migrate::ModelMeta;
use crate::orm::dynamic::DynQuerySet;

/// Tooling-owned resume table on the target. Same pattern as the migrations
/// ledger: created via the schema-DDL exception, never modelled.
const STATE_TABLE: &str = "umbral_transfer_state";

/// Knobs for a transfer run.
#[derive(Debug, Clone)]
pub struct TransferOptions {
    /// Rows per keyset page / per target transaction.
    pub batch_size: u64,
    /// Limit the copy to these tables (FK order still respected among them).
    /// `None` copies every registered model.
    pub only: Option<Vec<String>>,
    /// Report the copy order + source row counts without writing anything.
    pub dry_run: bool,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            batch_size: 1000,
            only: None,
            dry_run: false,
        }
    }
}

/// Per-run summary.
#[derive(Debug, Default)]
pub struct TransferReport {
    /// `(table, rows_copied)` in the order tables were processed.
    pub per_table: Vec<(String, u64)>,
    /// Total rows copied across all tables.
    pub rows: u64,
}

/// Errors a transfer can raise.
#[derive(Debug)]
pub enum TransferError {
    Db(sqlx::Error),
    Write(String),
    Read(String),
    /// A model has no primary key, so it can't be keyset-paginated.
    NoPrimaryKey(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferError::Db(e) => write!(f, "database error: {e}"),
            TransferError::Write(e) => write!(f, "write error: {e}"),
            TransferError::Read(e) => write!(f, "read error: {e}"),
            TransferError::NoPrimaryKey(t) => {
                write!(f, "table `{t}` has no primary key; cannot stream it")
            }
        }
    }
}
impl std::error::Error for TransferError {}
impl From<sqlx::Error> for TransferError {
    fn from(e: sqlx::Error) -> Self {
        TransferError::Db(e)
    }
}

/// Order models so every table's FK parents come before it (Kahn's algorithm
/// over `fk_target`; self-FKs ignored). A stable input order + name tiebreak
/// keeps the output deterministic. A cycle (mutually-referential tables) can't
/// be fully ordered — the remaining nodes are appended in name order so the
/// run still makes progress (their cross-refs need phase-2 deferred handling).
pub fn fk_topo_order(models: Vec<ModelMeta>) -> Vec<ModelMeta> {
    let tables: HashSet<String> = models.iter().map(|m| m.table.clone()).collect();
    // deps[table] = set of parent tables it FKs to (excluding self).
    let mut deps: HashMap<String, HashSet<String>> = HashMap::new();
    for m in &models {
        let mut d = HashSet::new();
        for col in &m.fields {
            if let Some(target) = &col.fk_target {
                if target != &m.table && tables.contains(target) {
                    d.insert(target.clone());
                }
            }
        }
        deps.insert(m.table.clone(), d);
    }
    let mut by_table: HashMap<String, ModelMeta> =
        models.into_iter().map(|m| (m.table.clone(), m)).collect();

    let mut ordered: Vec<ModelMeta> = Vec::with_capacity(by_table.len());
    let mut placed: HashSet<String> = HashSet::new();
    // Repeatedly emit every table whose deps are all placed, in name order.
    loop {
        let mut ready: Vec<String> = by_table
            .keys()
            .filter(|t| !placed.contains(*t))
            .filter(|t| deps[*t].iter().all(|d| placed.contains(d)))
            .cloned()
            .collect();
        if ready.is_empty() {
            break;
        }
        ready.sort();
        for t in ready {
            ordered.push(by_table.remove(&t).unwrap());
            placed.insert(t);
        }
    }
    // Any leftover (cycle) — append in name order so the run still processes them.
    let mut leftover: Vec<ModelMeta> = by_table.into_values().collect();
    leftover.sort_by(|a, b| a.table.cmp(&b.table));
    ordered.extend(leftover);
    ordered
}

/// A `pk > last` keyset condition. Handles integer and string/uuid PKs (the
/// value comes straight from the last row's JSON).
fn pk_gt_condition(pk_col: &str, last: &serde_json::Value) -> sea_query::SimpleExpr {
    let col = Expr::col(Alias::new(pk_col));
    match last {
        serde_json::Value::Number(n) if n.is_i64() => col.gt(n.as_i64().unwrap()),
        serde_json::Value::Number(n) if n.is_u64() => col.gt(n.as_u64().unwrap() as i64),
        serde_json::Value::String(s) => col.gt(s.clone()),
        _ => col.gt(last.to_string()),
    }
}

/// Copy every (selected) registered model from `source` to `target`, resumably.
pub async fn transfer(
    source: &DbPool,
    target: &DbPool,
    models: Vec<ModelMeta>,
    opts: &TransferOptions,
) -> Result<TransferReport, TransferError> {
    let ordered = fk_topo_order(models);
    let only: Option<HashSet<String>> = opts.only.as_ref().map(|v| v.iter().cloned().collect());

    if !opts.dry_run {
        ensure_state_table(target).await?;
    }
    let mut state = if opts.dry_run {
        HashMap::new()
    } else {
        read_state(target).await?
    };

    let mut report = TransferReport::default();
    for meta in &ordered {
        if only.as_ref().is_some_and(|s| !s.contains(&meta.table)) {
            continue;
        }
        let pk_col = meta
            .pk_column()
            .ok_or_else(|| TransferError::NoPrimaryKey(meta.table.clone()))?
            .name
            .clone();

        if opts.dry_run {
            let n = count_rows(source, &meta.table).await?;
            report.per_table.push((meta.table.clone(), n));
            report.rows += n;
            continue;
        }

        let entry = state.get(&meta.table);
        if entry.is_some_and(|(_, done)| *done) {
            continue; // already finished on a previous run
        }
        let mut last: Option<serde_json::Value> = entry.and_then(|(pk, _)| pk.clone());

        let mut copied: u64 = 0;
        loop {
            let mut qs = DynQuerySet::for_meta(meta).unredacted_for_backup();
            if let Some(last) = &last {
                qs = qs.filter_condition(
                    sea_query::Condition::all().add(pk_gt_condition(&pk_col, last)),
                );
            }
            let rows = qs
                .order_by_col(&pk_col, false)
                .limit(opts.batch_size)
                .fetch_as_json_on(source)
                .await
                .map_err(|e| TransferError::Read(e.to_string()))?;
            if rows.is_empty() {
                break;
            }
            let batch_len = rows.len();
            let new_last = rows.last().and_then(|r| r.get(&pk_col)).cloned();

            // Inserts + checkpoint bump commit together: a crash rolls back both.
            let mut tx = begin_on(target).await?;
            for row in &rows {
                DynQuerySet::for_meta(meta)
                    .presealed()
                    .insert_json_in_tx(row, &mut tx)
                    .await
                    .map_err(|e| TransferError::Write(e.to_string()))?;
            }
            upsert_state_in_tx(&mut tx, &meta.table, new_last.as_ref(), false).await?;
            tx.commit().await?;

            copied += batch_len as u64;
            last = new_last;
            if batch_len < opts.batch_size as usize {
                break; // short page => source exhausted
            }
        }

        // Table done: record it + clear the autoincrement cursor past the copied ids.
        let mut tx = begin_on(target).await?;
        upsert_state_in_tx(&mut tx, &meta.table, last.as_ref(), true).await?;
        tx.commit().await?;
        reset_sequence(target, &meta.table, &pk_col).await?;

        state.insert(meta.table.clone(), (last, true));
        report.per_table.push((meta.table.clone(), copied));
        report.rows += copied;
    }
    Ok(report)
}

/// Begin a transaction on an explicit pool (not the ambient one).
async fn begin_on(pool: &DbPool) -> Result<crate::db::Transaction, TransferError> {
    Ok(match pool {
        DbPool::Sqlite(p) => crate::db::begin_sqlite(p).await?,
        DbPool::Postgres(p) => crate::db::begin_pg(p).await?,
    })
}

async fn count_rows(pool: &DbPool, table: &str) -> Result<u64, TransferError> {
    let sql = format!("SELECT COUNT(*) FROM \"{}\"", table.replace('"', "\"\""));
    let n: i64 = match pool {
        DbPool::Sqlite(p) => sqlx::query_scalar(&sql).fetch_one(p).await?,
        DbPool::Postgres(p) => sqlx::query_scalar(&sql).fetch_one(p).await?,
    };
    Ok(n.max(0) as u64)
}

async fn ensure_state_table(target: &DbPool) -> Result<(), TransferError> {
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {STATE_TABLE} \
         (table_name TEXT PRIMARY KEY, last_pk TEXT, done INTEGER NOT NULL DEFAULT 0)"
    );
    match target {
        DbPool::Sqlite(p) => {
            sqlx::query(&sql).execute(p).await?;
        }
        DbPool::Postgres(p) => {
            sqlx::query(&sql).execute(p).await?;
        }
    }
    Ok(())
}

/// Read the resume table: `table -> (last_pk_json, done)`.
async fn read_state(
    target: &DbPool,
) -> Result<HashMap<String, (Option<serde_json::Value>, bool)>, TransferError> {
    let sql = format!("SELECT table_name, last_pk, done FROM {STATE_TABLE}");
    let mut out = HashMap::new();
    let rows: Vec<(String, Option<String>, i64)> = match target {
        DbPool::Sqlite(p) => sqlx::query_as(&sql).fetch_all(p).await?,
        DbPool::Postgres(p) => sqlx::query_as(&sql).fetch_all(p).await?,
    };
    for (table, last, done) in rows {
        let pk = last.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        out.insert(table, (pk, done != 0));
    }
    Ok(out)
}

/// Upsert one resume row inside the batch's transaction (atomic with the data).
async fn upsert_state_in_tx(
    tx: &mut crate::db::Transaction,
    table: &str,
    last_pk: Option<&serde_json::Value>,
    done: bool,
) -> Result<(), TransferError> {
    let last_str = last_pk.map(|v| v.to_string());
    let done_int = i64::from(done);
    match tx.backend_name() {
        "sqlite" => {
            let sql = format!(
                "INSERT INTO {STATE_TABLE} (table_name, last_pk, done) VALUES (?, ?, ?) \
                 ON CONFLICT(table_name) DO UPDATE SET last_pk = excluded.last_pk, done = excluded.done"
            );
            let inner = tx.as_sqlite_mut().expect("sqlite backend");
            sqlx::query(&sql)
                .bind(table)
                .bind(last_str)
                .bind(done_int)
                .execute(&mut **inner)
                .await?;
        }
        _ => {
            let sql = format!(
                "INSERT INTO {STATE_TABLE} (table_name, last_pk, done) VALUES ($1, $2, $3) \
                 ON CONFLICT(table_name) DO UPDATE SET last_pk = excluded.last_pk, done = excluded.done"
            );
            let inner = tx.as_pg_mut().expect("postgres backend");
            sqlx::query(&sql)
                .bind(table)
                .bind(last_str)
                .bind(done_int)
                .execute(&mut **inner)
                .await?;
        }
    }
    Ok(())
}

/// Clear the autoincrement cursor past the copied ids so the app's next insert
/// doesn't collide. Postgres bumps the serial sequence; SQLite rowid tables
/// auto-track max(rowid), so an explicit reset is only needed for Postgres.
async fn reset_sequence(target: &DbPool, table: &str, pk_col: &str) -> Result<(), TransferError> {
    if let DbPool::Postgres(p) = target {
        let sql = format!(
            "SELECT setval(pg_get_serial_sequence('{t}', '{c}'), \
             COALESCE((SELECT MAX(\"{c}\") FROM \"{t}\"), 1)) \
             WHERE pg_get_serial_sequence('{t}', '{c}') IS NOT NULL",
            t = table.replace('\'', "''"),
            c = pk_col.replace('\'', "''"),
        );
        // A non-integer PK has no serial sequence; the guard makes this a no-op.
        let _ = sqlx::query(&sql).execute(p).await;
    }
    Ok(())
}
