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
use sqlx::Row;

use crate::db::DbPool;
use crate::migrate::ModelMeta;
use crate::orm::SqlType;
use crate::orm::dynamic::DynQuerySet;

/// Tooling-owned resume table on the target. Same pattern as the migrations
/// ledger: created via the schema-DDL exception, never modelled.
const STATE_TABLE: &str = "umbral_transfer_state";

/// How to translate a *foreign-shaped* source's column names to the umbral
/// target's. The source and target tables share a name (inspectdb targets the
/// same table); only FK / junction columns differ by the source framework's
/// naming convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransferMap {
    /// Source and target share the umbral schema — no translation (env1->env2).
    #[default]
    None,
    /// Django: FK column `<field>_id`, M2M junction columns `<model>_id`.
    /// Mirrors `inspectdb --framework django` in reverse.
    Django,
    /// Rails / ActiveRecord: FK column `<field>_id`, join-table columns
    /// `<model>_id` — the same snake-case `_id` convention as Django.
    Rails,
    /// Laravel / Eloquent: FK column `<field>_id`, pivot columns `<model>_id`
    /// — the same snake-case `_id` convention as Django.
    Laravel,
    /// Prisma / TypeORM (and camelCase JS ORMs generally): FK column
    /// `<field>Id` (e.g. `authorId`), junction columns `<model>Id`.
    Prisma,
}

impl TransferMap {
    pub fn parse(s: &str) -> Option<TransferMap> {
        match s.to_ascii_lowercase().as_str() {
            "django" => Some(TransferMap::Django),
            "rails" | "activerecord" => Some(TransferMap::Rails),
            "laravel" | "eloquent" => Some(TransferMap::Laravel),
            "prisma" | "typeorm" => Some(TransferMap::Prisma),
            "none" | "" => Some(TransferMap::None),
            _ => None,
        }
    }

    /// The source column an umbral field reads from, or `None` when the umbral
    /// name already matches the source (no rename). The umbral field is
    /// snake_case. A snake-`_id` framework (Django/Rails/Laravel) only renames
    /// FK columns (`author` -> `author_id`); its other columns are already
    /// snake_case. A camelCase framework (Prisma) renames EVERY column
    /// (`first_name` -> `firstName`), and a FK additionally gets `Id`
    /// (`author` -> `authorId`).
    fn source_column(self, field: &str, is_fk: bool) -> Option<String> {
        match self {
            TransferMap::None => None,
            TransferMap::Django | TransferMap::Rails | TransferMap::Laravel => {
                if is_fk && !field.ends_with("_id") {
                    Some(format!("{field}_id"))
                } else {
                    None
                }
            }
            TransferMap::Prisma => {
                let base = to_lower_camel(field);
                let col = if is_fk { format!("{base}Id") } else { base };
                (col != field).then_some(col)
            }
        }
    }

    /// The source junction's `(parent, child)` FK column names, given the two
    /// endpoint model (struct) names. `None` means the umbral `parent_id` /
    /// `child_id` are already right.
    fn junction_source_columns(self, owner_model: &str, target_model: &str) -> (String, String) {
        match self {
            TransferMap::None => ("parent_id".to_string(), "child_id".to_string()),
            TransferMap::Django | TransferMap::Rails | TransferMap::Laravel => (
                format!("{}_id", owner_model.to_ascii_lowercase()),
                format!("{}_id", target_model.to_ascii_lowercase()),
            ),
            TransferMap::Prisma => (
                format!("{}Id", to_lower_camel(owner_model)),
                format!("{}Id", to_lower_camel(target_model)),
            ),
        }
    }
}

/// `blog_category` / `BlogCategory` -> `blogCategory`. Normalizes any casing to
/// snake first, then lower-camel-cases it. Used for camelCase source columns.
fn to_lower_camel(s: &str) -> String {
    let snake = umbral_casing::to_snake_case(s);
    let mut out = String::new();
    for (i, part) in snake.split('_').filter(|p| !p.is_empty()).enumerate() {
        if i == 0 {
            out.push_str(part);
        } else {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        }
    }
    out
}

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
    /// Translate a foreign-shaped source's column names (see [`TransferMap`]).
    pub map: TransferMap,
    /// Copy independent tables concurrently, up to this many at once (per FK
    /// level). `1` is fully sequential.
    pub workers: usize,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            batch_size: 1000,
            only: None,
            dry_run: false,
            map: TransferMap::None,
            workers: 1,
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
/// be fully ordered — the remaining nodes are appended in name order (the
/// transfer copies them under FK deferral, see [`copy_cyclic_group`]).
pub fn fk_topo_order(models: Vec<ModelMeta>) -> Vec<ModelMeta> {
    fk_topo_levels(models).into_iter().flatten().collect()
}

/// Like [`fk_topo_order`], but grouped into dependency LEVELS: every table in a
/// level depends only on tables in earlier levels, so a level's tables are
/// mutually independent and safe to copy concurrently. Parents-before-children
/// holds across levels. A cycle's leftover tables form a final level.
pub fn fk_topo_levels(models: Vec<ModelMeta>) -> Vec<Vec<ModelMeta>> {
    let (mut levels, cyclic) = fk_topo_plan(models);
    if !cyclic.is_empty() {
        levels.push(cyclic);
    }
    levels
}

/// The scheduling plan: `(orderable_levels, cyclic_leftover)`. The levels are
/// FK-topologically ordered (parents before children); `cyclic_leftover` holds
/// the mutually-referential tables that couldn't be ordered at all — they need
/// the deferred single-transaction copy. Self-referential tables stay in their
/// natural level (their self-FK is ignored for ordering) but are copied with
/// deferral individually.
pub fn fk_topo_plan(models: Vec<ModelMeta>) -> (Vec<Vec<ModelMeta>>, Vec<ModelMeta>) {
    let tables: HashSet<String> = models.iter().map(|m| m.table.clone()).collect();
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

    let mut levels: Vec<Vec<ModelMeta>> = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();
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
        let level: Vec<ModelMeta> = ready.iter().map(|t| by_table.remove(t).unwrap()).collect();
        for t in ready {
            placed.insert(t);
        }
        levels.push(level);
    }
    // Whatever's left is in a cycle (mutually-referential), name-ordered.
    let mut cyclic: Vec<ModelMeta> = by_table.into_values().collect();
    cyclic.sort_by(|a, b| a.table.cmp(&b.table));
    (levels, cyclic)
}

/// One many-to-many junction table to copy — an umbral-auto-generated
/// `<parent_table>_<field>` with `(parent_id, child_id)` and a composite PK.
/// Not a registered model, so it's copied by raw SQL (the junction exception,
/// same as its DDL) after both endpoint tables.
#[derive(Debug, Clone)]
struct Junction {
    table: String,
    parent_ty: SqlType,
    child_ty: SqlType,
    /// Source FK column names. On the umbral schema these are `parent_id` /
    /// `child_id`; under `TransferMap::Django` the source (Django) junction
    /// names them `<model>_id`, e.g. `community_id` / `software_id`.
    src_parent_col: String,
    src_child_col: String,
}

/// Enumerate every M2M junction the registered models declare. `parent_ty` is
/// the owner's PK type; `child_ty` the target's (resolved from the model set,
/// defaulting to `BigInt` when the target isn't registered here). `map` sets the
/// source-side column names to read from.
fn collect_junctions(models: &[ModelMeta], map: TransferMap) -> Vec<Junction> {
    let pk_ty = |table: &str| -> SqlType {
        models
            .iter()
            .find(|m| m.table == table)
            .and_then(|m| m.pk_column())
            .map(|c| c.ty)
            .unwrap_or(SqlType::BigInt)
    };
    let mut out = Vec::new();
    for m in models {
        let parent_ty = m.pk_column().map(|c| c.ty).unwrap_or(SqlType::BigInt);
        for rel in &m.m2m_relations {
            // Source junction FK columns are named after the two endpoint models
            // (`community_id` / `software_id` under a snake framework).
            let (src_parent_col, src_child_col) =
                map.junction_source_columns(&m.name, &rel.target_name);
            out.push(Junction {
                table: format!("{}_{}", m.table, rel.field_name),
                parent_ty,
                child_ty: pk_ty(&rel.target_table),
                src_parent_col,
                src_child_col,
            });
        }
    }
    out
}

/// Copy one junction's `(parent_id, child_id)` rows, keyset-paginated on the
/// composite key, translating source column names to the umbral `parent_id` /
/// `child_id` on write. Self-contained (own per-batch checkpoint + done marker)
/// so junctions can run concurrently.
async fn copy_one_junction(
    source: &DbPool,
    target: &DbPool,
    jn: &Junction,
    start_last: Option<(serde_json::Value, serde_json::Value)>,
    batch: u64,
) -> Result<u64, TransferError> {
    let mut last = start_last;
    let mut copied: u64 = 0;
    loop {
        let rows = read_junction_batch(source, jn, last.as_ref(), batch).await?;
        if rows.is_empty() {
            break;
        }
        let batch_len = rows.len();
        let new_last = rows.last().cloned();

        let mut tx = begin_on(target).await?;
        for (p, c) in &rows {
            insert_junction_in_tx(&mut tx, jn, p, c).await?;
        }
        let checkpoint = new_last
            .as_ref()
            .map(|(p, c)| serde_json::Value::Array(vec![p.clone(), c.clone()]));
        upsert_state_in_tx(&mut tx, &jn.table, checkpoint.as_ref(), false).await?;
        tx.commit().await?;

        copied += batch_len as u64;
        last = new_last;
        if batch_len < batch as usize {
            break;
        }
    }

    let checkpoint = last
        .as_ref()
        .map(|(p, c)| serde_json::Value::Array(vec![p.clone(), c.clone()]));
    let mut tx = begin_on(target).await?;
    upsert_state_in_tx(&mut tx, &jn.table, checkpoint.as_ref(), true).await?;
    tx.commit().await?;
    Ok(copied)
}

/// How a junction id column reads and binds. Junction ids are only ever PK
/// types, so integer, UUID, or a string (slug) PK.
#[derive(Clone, Copy, PartialEq)]
enum IdKind {
    Int,
    Uuid,
    Text,
}

fn id_kind(ty: SqlType) -> IdKind {
    match ty {
        SqlType::Integer | SqlType::BigInt | SqlType::SmallInt => IdKind::Int,
        SqlType::Uuid => IdKind::Uuid,
        _ => IdKind::Text,
    }
}

/// Read one junction id column from a SQLite row into JSON. SQLite stores a UUID
/// as TEXT, but decoding through `uuid::Uuid` normalises it either way.
fn read_id_sqlite(
    row: &sqlx::sqlite::SqliteRow,
    idx: usize,
    ty: SqlType,
) -> Result<serde_json::Value, TransferError> {
    Ok(match id_kind(ty) {
        IdKind::Int => serde_json::Value::from(row.try_get::<i64, _>(idx)?),
        IdKind::Uuid => serde_json::Value::from(row.try_get::<uuid::Uuid, _>(idx)?.to_string()),
        IdKind::Text => serde_json::Value::from(row.try_get::<String, _>(idx)?),
    })
}

/// Read one junction id column from a Postgres row into JSON. A UUID is a native
/// pg type, so it decodes through `uuid::Uuid` (a `String` read would fail).
fn read_id_pg(
    row: &sqlx::postgres::PgRow,
    idx: usize,
    ty: SqlType,
) -> Result<serde_json::Value, TransferError> {
    Ok(match id_kind(ty) {
        IdKind::Int => serde_json::Value::from(row.try_get::<i64, _>(idx)?),
        IdKind::Uuid => serde_json::Value::from(row.try_get::<uuid::Uuid, _>(idx)?.to_string()),
        IdKind::Text => serde_json::Value::from(row.try_get::<String, _>(idx)?),
    })
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

/// Build the SOURCE-shaped meta (field names swapped to the source's column
/// names under a map) plus the `source_col -> target_field` rename that undoes
/// it after reading. Under [`TransferMap::None`] this is the meta unchanged and
/// an empty rename.
fn source_meta_for(meta: &ModelMeta, map: TransferMap) -> (ModelMeta, HashMap<String, String>) {
    let mut rename = HashMap::new();
    if map == TransferMap::None {
        return (meta.clone(), rename);
    }
    // Each field reads from the source column its framework names — a snake
    // framework only reshapes FK columns (`author` -> `author_id`); a camelCase
    // framework reshapes every column (`first_name` -> `firstName`, FK
    // `author` -> `authorId`).
    let mut src = meta.clone();
    for col in &mut src.fields {
        if let Some(source_col) = map.source_column(&col.name, col.fk_target.is_some()) {
            rename.insert(source_col.clone(), col.name.clone());
            col.name = source_col;
        }
    }
    (src, rename)
}

/// Rename a row's keys from source columns to target fields (identity when the
/// map is empty).
fn apply_key_rename(
    row: &serde_json::Map<String, serde_json::Value>,
    rename: &HashMap<String, String>,
) -> serde_json::Map<String, serde_json::Value> {
    if rename.is_empty() {
        return row.clone();
    }
    row.iter()
        .map(|(k, v)| {
            (
                rename.get(k).cloned().unwrap_or_else(|| k.clone()),
                v.clone(),
            )
        })
        .collect()
}

/// Copy one model's rows, keyset-paginated, translating source columns to target
/// fields per `key_rename`. Each batch's inserts + its checkpoint commit in one
/// target transaction; on completion the table is marked done and its sequence
/// reset. Self-contained so it can run concurrently with sibling tables.
#[allow(clippy::too_many_arguments)]
async fn copy_one_model(
    source: &DbPool,
    target: &DbPool,
    read_meta: &ModelMeta,
    write_meta: &ModelMeta,
    key_rename: &HashMap<String, String>,
    pk_col: &str,
    start_last: Option<serde_json::Value>,
    batch: u64,
) -> Result<u64, TransferError> {
    let mut last = start_last;
    let mut copied: u64 = 0;
    loop {
        let mut qs = DynQuerySet::for_meta(read_meta).unredacted_for_backup();
        if let Some(l) = &last {
            qs = qs.filter_condition(sea_query::Condition::all().add(pk_gt_condition(pk_col, l)));
        }
        let rows = qs
            .order_by_col(pk_col, false)
            .limit(batch)
            .fetch_as_json_on(source)
            .await
            .map_err(|e| TransferError::Read(e.to_string()))?;
        if rows.is_empty() {
            break;
        }
        let batch_len = rows.len();
        let new_last = rows.last().and_then(|r| r.get(pk_col)).cloned();

        let mut tx = begin_on(target).await?;
        for row in &rows {
            let mapped = apply_key_rename(row, key_rename);
            DynQuerySet::for_meta(write_meta)
                .presealed()
                .trusted()
                .insert_json_in_tx(&mapped, &mut tx)
                .await
                .map_err(|e| TransferError::Write(e.to_string()))?;
        }
        upsert_state_in_tx(&mut tx, &write_meta.table, new_last.as_ref(), false).await?;
        tx.commit().await?;

        copied += batch_len as u64;
        last = new_last;
        if batch_len < batch as usize {
            break;
        }
    }

    let mut tx = begin_on(target).await?;
    upsert_state_in_tx(&mut tx, &write_meta.table, last.as_ref(), true).await?;
    tx.commit().await?;
    reset_sequence(target, &write_meta.table, pk_col).await?;
    Ok(copied)
}

/// Whether a model FK-references itself — its rows can hold a forward reference
/// (child id < parent id) that a per-row FK check would reject on insert, so it
/// needs the deferred single-transaction copy.
fn has_self_fk(meta: &ModelMeta) -> bool {
    meta.fields
        .iter()
        .any(|c| c.fk_target.as_deref() == Some(meta.table.as_str()))
}

/// Defer foreign-key enforcement to the end of the current transaction, so a
/// cyclic / forward reference resolves once every row in the group is present.
async fn defer_fk_in_tx(tx: &mut crate::db::Transaction) -> Result<(), TransferError> {
    match tx.backend_name() {
        "sqlite" => {
            let inner = tx.as_sqlite_mut().expect("sqlite backend");
            sqlx::query("PRAGMA defer_foreign_keys = ON")
                .execute(&mut **inner)
                .await?;
        }
        _ => {
            // Works when the FK constraints are DEFERRABLE; a no-op otherwise.
            let inner = tx.as_pg_mut().expect("postgres backend");
            sqlx::query("SET CONSTRAINTS ALL DEFERRED")
                .execute(&mut **inner)
                .await?;
        }
    }
    Ok(())
}

/// Copy a set of mutually- or self-referential tables inside ONE transaction
/// with FK enforcement deferred, so their cross-references resolve at commit
/// (when every row exists). Trades the per-batch checkpoint for correctness on
/// a cycle — these tables are all-or-nothing within the run, which is fine for
/// the small tables cycles usually involve (a category tree, an org/user pair).
async fn copy_cyclic_group(
    source: &DbPool,
    target: &DbPool,
    group: &[&ModelMeta],
    map: TransferMap,
    batch: u64,
) -> Result<Vec<(String, u64)>, TransferError> {
    let mut tx = begin_on(target).await?;
    defer_fk_in_tx(&mut tx).await?;
    let mut results = Vec::new();
    for meta in group {
        let pk_col = meta
            .pk_column()
            .ok_or_else(|| TransferError::NoPrimaryKey(meta.table.clone()))?
            .name
            .clone();
        let (read_meta, key_rename) = source_meta_for(meta, map);
        let mut last: Option<serde_json::Value> = None;
        let mut copied: u64 = 0;
        loop {
            let mut qs = DynQuerySet::for_meta(&read_meta).unredacted_for_backup();
            if let Some(l) = &last {
                qs = qs
                    .filter_condition(sea_query::Condition::all().add(pk_gt_condition(&pk_col, l)));
            }
            let rows = qs
                .order_by_col(&pk_col, false)
                .limit(batch)
                .fetch_as_json_on(source)
                .await
                .map_err(|e| TransferError::Read(e.to_string()))?;
            if rows.is_empty() {
                break;
            }
            let batch_len = rows.len();
            last = rows.last().and_then(|r| r.get(&pk_col)).cloned();
            for row in &rows {
                let mapped = apply_key_rename(row, &key_rename);
                DynQuerySet::for_meta(meta)
                    .presealed()
                    .trusted()
                    .insert_json_in_tx(&mapped, &mut tx)
                    .await
                    .map_err(|e| TransferError::Write(e.to_string()))?;
            }
            copied += batch_len as u64;
            if batch_len < batch as usize {
                break;
            }
        }
        upsert_state_in_tx(&mut tx, &meta.table, last.as_ref(), true).await?;
        results.push((meta.table.clone(), copied));
    }
    // The single deferred FK check happens HERE — every row is present.
    tx.commit().await?;
    for meta in group {
        if let Some(pk) = meta.pk_column() {
            reset_sequence(target, &meta.table, &pk.name).await?;
        }
    }
    Ok(results)
}

/// Copy every (selected) registered model from `source` to `target`, resumably.
pub async fn transfer(
    source: &DbPool,
    target: &DbPool,
    models: Vec<ModelMeta>,
    opts: &TransferOptions,
) -> Result<TransferReport, TransferError> {
    use futures_util::stream::{StreamExt, TryStreamExt};

    let (levels, cyclic) = fk_topo_plan(models);
    let flat: Vec<ModelMeta> = levels
        .iter()
        .flatten()
        .chain(cyclic.iter())
        .cloned()
        .collect();
    let only: Option<HashSet<String>> = opts.only.as_ref().map(|v| v.iter().cloned().collect());
    let excluded = |t: &str| only.as_ref().is_some_and(|s| !s.contains(t));
    let workers = opts.workers.max(1);
    let mut report = TransferReport::default();

    // Dry run: count rows in dependency order, no writes, no state table.
    if opts.dry_run {
        for meta in &flat {
            if excluded(&meta.table) {
                continue;
            }
            let n = count_rows(source, &meta.table).await?;
            report.per_table.push((meta.table.clone(), n));
            report.rows += n;
        }
        for jn in collect_junctions(&flat, opts.map) {
            if excluded(&jn.table) {
                continue;
            }
            let n = count_rows(source, &jn.table).await?;
            report.per_table.push((jn.table.clone(), n));
            report.rows += n;
        }
        return Ok(report);
    }

    ensure_state_table(target).await?;
    let state = read_state(target).await?;
    let done = |t: &str| state.get(t).is_some_and(|(_, d)| *d);

    // Models, one FK level at a time; the tables WITHIN a level are mutually
    // independent, so up to `workers` of them copy concurrently. Each
    // `copy_one_model` owns its transactions + checkpoint, so parallel tables
    // never share mutable state.
    let state_ref = &state;
    for level in &levels {
        let tasks = level
            .iter()
            .filter(|m| !excluded(&m.table) && !done(&m.table))
            .map(|meta| {
                let map = opts.map;
                let batch = opts.batch_size;
                async move {
                    // A self-referential table can hold a forward reference, so
                    // it takes the deferred single-transaction copy on its own.
                    if has_self_fk(meta) {
                        return copy_cyclic_group(source, target, &[meta], map, batch).await;
                    }
                    let pk_col = meta
                        .pk_column()
                        .ok_or_else(|| TransferError::NoPrimaryKey(meta.table.clone()))?
                        .name
                        .clone();
                    let (read_meta, key_rename) = source_meta_for(meta, map);
                    let start_last = state_ref.get(&meta.table).and_then(|(pk, _)| pk.clone());
                    let copied = copy_one_model(
                        source,
                        target,
                        &read_meta,
                        meta,
                        &key_rename,
                        &pk_col,
                        start_last,
                        batch,
                    )
                    .await?;
                    Ok::<_, TransferError>(vec![(meta.table.clone(), copied)])
                }
            });
        let results: Vec<Vec<(String, u64)>> = futures_util::stream::iter(tasks)
            .buffer_unordered(workers)
            .try_collect()
            .await?;
        for (t, c) in results.into_iter().flatten() {
            report.per_table.push((t, c));
            report.rows += c;
        }
    }

    // Mutually-referential tables that couldn't be ordered at all: copy the
    // whole group in ONE transaction with FK enforcement deferred, so each
    // side's reference to the other resolves at commit.
    let cyclic_group: Vec<&ModelMeta> = cyclic
        .iter()
        .filter(|m| !excluded(&m.table) && !done(&m.table))
        .collect();
    if !cyclic_group.is_empty() {
        let results =
            copy_cyclic_group(source, target, &cyclic_group, opts.map, opts.batch_size).await?;
        for (t, c) in results {
            report.per_table.push((t, c));
            report.rows += c;
        }
    }

    // Junctions after every model (both endpoints now exist on the target) —
    // all independent of each other, so they run concurrently too.
    let junctions = collect_junctions(&flat, opts.map);
    let jtasks = junctions
        .iter()
        .filter(|jn| !excluded(&jn.table) && !done(&jn.table))
        .map(|jn| {
            let start = state
                .get(&jn.table)
                .and_then(|(pk, _)| pk.clone())
                .and_then(decode_pair);
            async move {
                let copied = copy_one_junction(source, target, jn, start, opts.batch_size).await?;
                Ok::<_, TransferError>((jn.table.clone(), copied))
            }
        });
    let jresults: Vec<(String, u64)> = futures_util::stream::iter(jtasks)
        .buffer_unordered(workers)
        .try_collect()
        .await?;
    for (t, c) in jresults {
        report.per_table.push((t, c));
        report.rows += c;
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

/// Decode a checkpoint `[parent_id, child_id]` JSON array back into a pair.
fn decode_pair(v: serde_json::Value) -> Option<(serde_json::Value, serde_json::Value)> {
    match v {
        serde_json::Value::Array(a) if a.len() == 2 => Some((a[0].clone(), a[1].clone())),
        _ => None,
    }
}

/// Read one keyset page of `(parent_id, child_id)` rows from a source junction,
/// after `last` in composite order. Backend + id-kind aware, so a UUID junction
/// id decodes natively on either end.
async fn read_junction_batch(
    source: &DbPool,
    jn: &Junction,
    last: Option<&(serde_json::Value, serde_json::Value)>,
    limit: u64,
) -> Result<Vec<(serde_json::Value, serde_json::Value)>, TransferError> {
    let jt = jn.table.replace('"', "\"\"");
    // Source-side column names (umbral `parent_id`/`child_id`, or Django's
    // `<model>_id` under a map). The read is by position, so the output is
    // always `(parent, child)` regardless of the source names.
    let pcol = jn.src_parent_col.replace('"', "\"\"");
    let ccol = jn.src_child_col.replace('"', "\"\"");
    let mut out = Vec::new();
    match source {
        DbPool::Sqlite(pool) => {
            let where_sql = if last.is_some() {
                format!("WHERE (\"{pcol}\", \"{ccol}\") > (?, ?)")
            } else {
                String::new()
            };
            let sql = format!(
                "SELECT \"{pcol}\", \"{ccol}\" FROM \"{jt}\" {where_sql} \
                 ORDER BY \"{pcol}\", \"{ccol}\" LIMIT {limit}"
            );
            let mut q = sqlx::query(&sql);
            if let Some((p, c)) = last {
                q = bind_id_sqlite(q, p, jn.parent_ty);
                q = bind_id_sqlite(q, c, jn.child_ty);
            }
            for row in q.fetch_all(pool).await? {
                out.push((
                    read_id_sqlite(&row, 0, jn.parent_ty)?,
                    read_id_sqlite(&row, 1, jn.child_ty)?,
                ));
            }
        }
        DbPool::Postgres(pool) => {
            let where_sql = if last.is_some() {
                format!("WHERE (\"{pcol}\", \"{ccol}\") > ($1, $2)")
            } else {
                String::new()
            };
            let sql = format!(
                "SELECT \"{pcol}\", \"{ccol}\" FROM \"{jt}\" {where_sql} \
                 ORDER BY \"{pcol}\", \"{ccol}\" LIMIT {limit}"
            );
            let mut q = sqlx::query(&sql);
            if let Some((p, c)) = last {
                q = bind_id_pg(q, p, jn.parent_ty);
                q = bind_id_pg(q, c, jn.child_ty);
            }
            for row in q.fetch_all(pool).await? {
                out.push((
                    read_id_pg(&row, 0, jn.parent_ty)?,
                    read_id_pg(&row, 1, jn.child_ty)?,
                ));
            }
        }
    }
    Ok(out)
}

/// Bind a junction id into a SQLite query per its kind (UUID + slug both bind as
/// TEXT on SQLite).
fn bind_id_sqlite<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    v: &serde_json::Value,
    ty: SqlType,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match id_kind(ty) {
        IdKind::Int => q.bind(v.as_i64()),
        _ => q.bind(v.as_str().map(str::to_string)),
    }
}

/// Bind a junction id into a Postgres query per its kind — a UUID binds as the
/// native `uuid::Uuid` (a `String` bind would be rejected by the pg type).
fn bind_id_pg<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    v: &serde_json::Value,
    ty: SqlType,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match id_kind(ty) {
        IdKind::Int => q.bind(v.as_i64()),
        IdKind::Uuid => q.bind(v.as_str().and_then(|s| uuid::Uuid::parse_str(s).ok())),
        IdKind::Text => q.bind(v.as_str().map(str::to_string)),
    }
}

/// Insert one junction row into the target inside the batch transaction. The
/// composite PK makes `ON CONFLICT DO NOTHING` an exact idempotent no-op on a
/// row already copied (resume safety, belt-and-suspenders with the checkpoint).
async fn insert_junction_in_tx(
    tx: &mut crate::db::Transaction,
    jn: &Junction,
    p: &serde_json::Value,
    c: &serde_json::Value,
) -> Result<(), TransferError> {
    let jt = jn.table.replace('"', "\"\"");
    match tx.backend_name() {
        "sqlite" => {
            let sql = format!(
                "INSERT INTO \"{jt}\" (parent_id, child_id) VALUES (?, ?) \
                 ON CONFLICT (parent_id, child_id) DO NOTHING"
            );
            let inner = tx.as_sqlite_mut().expect("sqlite backend");
            let mut q = sqlx::query(&sql);
            q = bind_id_sqlite(q, p, jn.parent_ty);
            q = bind_id_sqlite(q, c, jn.child_ty);
            q.execute(&mut **inner).await?;
        }
        _ => {
            let sql = format!(
                "INSERT INTO \"{jt}\" (parent_id, child_id) VALUES ($1, $2) \
                 ON CONFLICT (parent_id, child_id) DO NOTHING"
            );
            let inner = tx.as_pg_mut().expect("postgres backend");
            let mut q = sqlx::query(&sql);
            q = bind_id_pg(q, p, jn.parent_ty);
            q = bind_id_pg(q, c, jn.child_ty);
            q.execute(&mut **inner).await?;
        }
    }
    Ok(())
}
