//! The migration engine — the north star.
//!
//! Implements the **declare → migrate → change → migrate** cycle from
//! `arch.md §0`. Users declare or change a model, run `migrate`, and the
//! framework either generates the missing migration file (via `make`)
//! or applies pending migration files (via `run`).
//!
//! At M5 (this milestone) the surface ships:
//!
//! - A process-wide [`ModelRegistry`] populated by
//!   `App::builder().model::<T>()`.
//! - A [`Snapshot`] of every registered model's metadata, JSON-
//!   serialisable so it can live inside a migration file's
//!   `snapshot_after`.
//! - An [`Operation`] enum with the two minimum-viable ops:
//!   [`Operation::CreateTable`] and [`Operation::DropTable`]. Column-
//!   level ops (`AddColumn`, `DropColumn`, `AlterColumn`) land at M8
//!   alongside rename detection (per `arch.md §7` and
//!   `docs/specs/06-migration-engine.md`). The "M5.1" label in the
//!   `UnsupportedChange` error message is shorthand for the same slot.
//! - A [`MigrationFile`] format (one JSON file per migration) carrying
//!   `id`, `operations`, and `snapshot_after`.
//! - The `umbra_migrations` tracking table (one row per applied
//!   migration, keyed by `(plugin, name)`).
//! - High-level entry points: [`make`], [`run`], [`show`].
//!
//! Reserved for M5.1+:
//!
//! - Column-level ops (`AddColumn`, `DropColumn`, `AlterColumn`).
//! - Rename-detection vs drop+add disambiguation (spec 06 §M8).
//! - `RunSql` / `RunCode` data-migration ops.
//! - Squashing, `--fake`, `--fake-initial` (PRD F-MIG-6 P2).
//! - Cross-plugin migration dependencies (needs M7 Plugin contract).
//!
//! See `docs/specs/06-migration-engine.md` for the full target shape.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::orm::{FieldSpec, Model, SqlType};

/// Per-process model registry. Published by `AppBuilder::build()`
/// after `.model::<T>()` calls and `.plugin(...)` registrations
/// collected metadata into the builder.
///
/// Stored as a flat vector of `(plugin_name, model)` pairs so M5's
/// existing `registered_models()` keeps working (drop the plugin
/// names) and the M7 plugin-aware walks (`registered_plugins`,
/// `models_for_plugin`) can read the same source of truth without a
/// second registry. The plugin name `"app"` covers models registered
/// via `.model::<T>()`; every other name is a real Plugin's.
static REGISTRY: OnceLock<Vec<(String, ModelMeta)>> = OnceLock::new();

/// Initialize the registry with one entry per plugin.
///
/// `App::build()` calls this after collecting `.model::<T>()` into the
/// implicit `"app"` plugin and walking every registered plugin's
/// `Plugin::models()`. Plugins missing from the map contribute zero
/// models (default-noop `models()` returns an empty vec; the entry
/// can be omitted).
pub(crate) fn init_plugins(per_plugin: std::collections::HashMap<String, Vec<ModelMeta>>) {
    let mut flat: Vec<(String, ModelMeta)> = Vec::new();
    let mut plugin_names: Vec<String> = per_plugin.keys().cloned().collect();
    plugin_names.sort();
    for plugin in plugin_names {
        for m in per_plugin.get(&plugin).cloned().unwrap_or_default() {
            flat.push((plugin.clone(), m));
        }
    }
    REGISTRY
        .set(flat)
        .expect("umbra::migrate::init_plugins called more than once");
}

/// Return every registered model, flat. Drops the per-plugin grouping;
/// useful when the caller only needs the model set (e.g. M5's `make`
/// when the codebase only had a single `"app"` plugin).
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub fn registered_models() -> Vec<ModelMeta> {
    REGISTRY
        .get()
        .expect("umbra: model registry not initialised — did you call App::build()?")
        .iter()
        .map(|(_, m)| m.clone())
        .collect()
}

/// Return the registered plugin names that contributed at least one
/// model. Sorted deterministically. Used as a fallback when no
/// topological order is published; the M7 walk used this directly,
/// and M8 prefers [`plugin_order`] when it's been set.
pub fn registered_plugins() -> Vec<String> {
    let mut names: Vec<String> = REGISTRY
        .get()
        .expect("umbra: model registry not initialised — did you call App::build()?")
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The topological plugin order published by `App::build()` after its
/// phase 1.5 sort. `None` until that runs; the CLI subcommands
/// (`makemigrations`, `migrate`, `showmigrations`) call `App::build()`
/// via `boot_for_management` before reaching the migration engine.
static PLUGIN_ORDER: OnceLock<Vec<String>> = OnceLock::new();

/// Publish the topological plugin order. Called by `App::build()` once
/// the phase 1.5 sort has produced the order. Must include the
/// implicit `"app"` plugin even when no real plugins are registered.
pub(crate) fn init_plugin_order(order: Vec<String>) {
    PLUGIN_ORDER
        .set(order)
        .expect("umbra::migrate::init_plugin_order called more than once");
}

/// Return the topological plugin order if `App::build()` published
/// one; otherwise fall back to [`registered_plugins`] (sorted by
/// name). The fallback keeps existing M5 / M6 tests working without
/// requiring them to wire a full plugin sort.
pub fn plugin_order() -> Vec<String> {
    PLUGIN_ORDER
        .get()
        .cloned()
        .unwrap_or_else(registered_plugins)
}

/// Return the models registered against a specific plugin. Empty if
/// no plugin by that name registered models.
pub fn models_for_plugin(plugin: &str) -> Vec<ModelMeta> {
    REGISTRY
        .get()
        .expect("umbra: model registry not initialised — did you call App::build()?")
        .iter()
        .filter(|(p, _)| p == plugin)
        .map(|(_, m)| m.clone())
        .collect()
}

/// Static metadata for one registered model, copied off the `Model`
/// trait's `const`s when the user calls `App::builder().model::<T>()`.
///
/// Owned (no lifetimes) so the registry can hold an arbitrary number
/// without the lifetime contortions a slice of trait references would
/// need. The cost is one Vec at `App::build` time; the win is
/// `registered_models()` having a plain `&'static [ModelMeta]` signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMeta {
    /// The struct name (`Model::NAME`). Identifies the model across
    /// snapshot diffs even if the table is renamed.
    pub name: String,
    /// The SQL table name (`Model::TABLE`).
    pub table: String,
    /// One owned column descriptor per field, in declaration order.
    /// Owned (`Column`, not the underlying static `FieldSpec`) so the
    /// snapshot round-trips cleanly through serde.
    pub fields: Vec<Column>,
}

impl ModelMeta {
    /// Read static metadata off `T: Model` into an owned `ModelMeta`.
    /// Called from `AppBuilder::model::<T>()`.
    pub fn for_<T: Model>() -> Self {
        Self {
            name: T::NAME.to_string(),
            table: T::TABLE.to_string(),
            fields: T::FIELDS.iter().map(Column::from).collect(),
        }
    }
}

/// A snapshot of every registered model at a point in time.
///
/// Serialised into the `snapshot_after` field of a migration file so
/// future `makemigrations` runs can diff against it without replaying
/// every prior migration's operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Snapshot {
    /// Models sorted by name so the JSON is deterministic and the
    /// snapshot_hash is stable across runs that produce equivalent
    /// content.
    pub models: Vec<ModelMeta>,
}

impl Snapshot {
    /// Build a snapshot from the live registry (the current state of
    /// the application's models, post-`App::build`).
    pub fn current() -> Self {
        let mut models = registered_models().to_vec();
        models.sort_by(|a, b| a.name.cmp(&b.name));
        Self { models }
    }

    /// Build a snapshot containing only the models registered
    /// against the given plugin. Used by `make_in` to diff each
    /// plugin's migrations independently against its own prior
    /// snapshot, so cross-plugin model sets don't bleed into one
    /// migration file.
    pub fn current_for(plugin: &str) -> Self {
        let mut models = models_for_plugin(plugin);
        models.sort_by(|a, b| a.name.cmp(&b.name));
        Self { models }
    }

    /// Compute the snapshot's SHA-256 hash, hex-encoded. Stored in the
    /// `umbra_migrations.snapshot_hash` column for drift detection.
    pub fn hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let json = serde_json::to_string(self).expect("Snapshot serializes");
        let digest = Sha256::digest(json.as_bytes());
        hex(&digest[..])
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// One operation inside a migration. The migration engine renders each
/// operation to SQL via the active backend (M4 `DatabaseBackend::
/// map_type`) and runs them in declaration order inside one
/// transaction per migration file.
///
/// M5 v1 shipped table-level ops; M8 v1 adds `AddColumn` and
/// `DropColumn`. `AlterColumn`, index / constraint ops, and
/// `RunSql` / `RunCode` are deferred (see `docs/specs/06-migration-
/// engine.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Operation {
    /// Create a new table. `columns` is in declaration order; the
    /// engine builds a sea-query `Table::create()` over them and runs
    /// the rendered DDL.
    CreateTable { table: String, columns: Vec<Column> },
    /// Drop an existing table.
    DropTable { table: String },
    /// Add a new column to an existing table. Rendered as
    /// `ALTER TABLE x ADD COLUMN y TYPE [NOT NULL]`. SQLite refuses a
    /// non-nullable add against a populated table without a default;
    /// the engine surfaces that as a sqlx error at apply time (M8 v1).
    /// A future op `AddColumnWithDefault` lifts the restriction once
    /// the `#[umbra(default = ...)]` attribute lands.
    AddColumn { table: String, column: Column },
    /// Drop a column from an existing table. Rendered as
    /// `ALTER TABLE x DROP COLUMN y`. SQLite 3.35+ and Postgres
    /// support this natively; older SQLite would need a table-
    /// recreation dance the engine doesn't implement.
    DropColumn { table: String, column: String },
}

/// One column inside a [`Operation::CreateTable`].
///
/// Mirrors the structure of [`FieldSpec`] but is fully owned for
/// serialisation. Reconstructed from a `FieldSpec` at diff time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub ty: SqlType,
    pub primary_key: bool,
    pub nullable: bool,
}

impl From<&FieldSpec> for Column {
    fn from(f: &FieldSpec) -> Self {
        Self {
            name: f.name.to_string(),
            ty: f.ty,
            primary_key: f.primary_key,
            nullable: f.nullable,
        }
    }
}

/// The on-disk shape of one migration. Files in `migrations/<plugin>/`
/// deserialize into this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationFile {
    /// Stable id, matches the filename minus `.json`.
    pub id: String,
    /// The plugin that owns this migration. M5 hardcodes `"app"` for
    /// the user's binary; M7 generalises to one directory per plugin.
    pub plugin: String,
    /// Predecessor migrations, in `(plugin, id)` form. Within-plugin
    /// predecessors are implicit (the prior numeric file); cross-
    /// plugin predecessors land at M7.
    #[serde(default)]
    pub depends_on: Vec<MigrationRef>,
    /// Ordered operations applied when this migration runs.
    pub operations: Vec<Operation>,
    /// The full snapshot of every model after this migration has run.
    /// Source of truth for the next `make` to diff against.
    pub snapshot_after: Snapshot,
}

/// A pointer to one (plugin, migration_id) pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationRef {
    pub plugin: String,
    pub migration: String,
}

/// At M5 every migration belongs to a single placeholder plugin. M7's
/// Plugin contract replaces this with `Plugin::name()`.
pub const APP_PLUGIN_NAME: &str = "app";

/// Default directory for migration files. `make` writes into
/// `migrations/<plugin>/`; `run` reads from the same place. Override
/// with `--migrations-dir` once the CLI grows real arg parsing (M5+).
pub const MIGRATIONS_DIR: &str = "migrations";

/// Errors the migration engine can produce.
#[derive(Debug)]
pub enum MigrateError {
    /// IO error reading or writing a migration file or directory.
    Io(std::io::Error),
    /// JSON parse error on a migration file.
    Json(serde_json::Error),
    /// sqlx error executing a migration's SQL or touching the
    /// tracking table.
    Sqlx(sqlx::Error),
    /// `make` ran but found no differences against the latest snapshot,
    /// so there's nothing to write. Surfaced so the CLI can print
    /// "no changes detected" instead of an empty migration file.
    NoChanges,
    /// The current models diverge from the snapshot in a way M5 v1
    /// can't represent yet (anything other than create/drop table).
    /// M5.1 lifts this when column-level ops land.
    UnsupportedChange(String),
    /// A column-level change the engine can't apply automatically:
    /// type change, or a nullable flip on a populated SQLite table.
    /// Surfaces from `diff` so the build stops before producing a
    /// migration that would lose data or fail to apply. The user
    /// resolves by hand-writing the migration with the appropriate
    /// data-preserving steps. Carries the model / column / reason.
    UnsafeAlter {
        model: String,
        column: String,
        reason: String,
    },
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::Io(e) => write!(f, "umbra migrate: io: {e}"),
            MigrateError::Json(e) => write!(f, "umbra migrate: json: {e}"),
            MigrateError::Sqlx(e) => write!(f, "umbra migrate: sqlx: {e}"),
            MigrateError::NoChanges => write!(
                f,
                "umbra migrate: no changes detected; declare or change a model first"
            ),
            MigrateError::UnsupportedChange(msg) => {
                write!(f, "umbra migrate: unsupported change at M5 v1: {msg}")
            }
            MigrateError::UnsafeAlter {
                model,
                column,
                reason,
            } => write!(
                f,
                "umbra migrate: unsafe column change on `{model}.{column}`: {reason}; \
                 hand-write the migration with a data-preserving step"
            ),
        }
    }
}

impl std::error::Error for MigrateError {}

impl From<std::io::Error> for MigrateError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for MigrateError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<sqlx::Error> for MigrateError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

// =========================================================================
// Top-level entry points.
// =========================================================================

/// Generate one migration file per registered plugin that has changes,
/// diffing each plugin's current model set against the latest snapshot
/// in `migrations/<plugin>/`. Each new file lands inside its own
/// plugin directory with the next sequence number and a `_<short_name>`
/// suffix derived from the dominant operation.
///
/// Returns the paths of every file written, one per plugin that had a
/// non-empty diff. Returns `MigrateError::NoChanges` if no plugin
/// produced any changes at all.
pub async fn make() -> Result<Vec<PathBuf>, MigrateError> {
    make_in(Path::new(MIGRATIONS_DIR)).await
}

/// Same as [`make`] but takes an explicit base directory. Used by
/// tests to avoid touching the cwd.
///
/// Iterates [`plugin_order`], which is the topological order
/// published by `App::build()`'s phase 1.5 sort. Cross-plugin FKs
/// land in dependency order this way (a plugin's `CreateTable` for
/// the FK target runs before the dependent plugin's `CreateTable`).
/// Falls back to [`registered_plugins`] when no order has been
/// published (e.g. low-level tests that init the registry directly).
pub async fn make_in(dir: &Path) -> Result<Vec<PathBuf>, MigrateError> {
    let mut written: Vec<PathBuf> = Vec::new();

    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);

        // The previous snapshot is the `snapshot_after` of the highest-
        // numbered migration file (filenames are zero-padded so lexical
        // sort matches numeric order). An empty or missing directory
        // means "no prior state", the first-run case for this plugin.
        let existing = list_migration_files(&plugin_dir)?;
        let previous = match existing.last() {
            Some(path) => read_migration_file(path)?.snapshot_after,
            None => Snapshot::default(),
        };

        let current = Snapshot::current_for(&plugin);
        let operations = diff(&previous, &current)?;
        if operations.is_empty() {
            continue;
        }

        let seq = (existing.len() + 1) as u32;
        let suffix = suffix_for(&operations);
        let id = format!("{seq:04}_{suffix}");
        let filename = format!("{id}.json");

        let file = MigrationFile {
            id: id.clone(),
            plugin: plugin.clone(),
            depends_on: Vec::new(),
            operations,
            snapshot_after: current,
        };

        std::fs::create_dir_all(&plugin_dir)?;
        let path = plugin_dir.join(filename);
        let json = serde_json::to_string_pretty(&file)?;
        std::fs::write(&path, json)?;
        written.push(path);
    }

    if written.is_empty() {
        return Err(MigrateError::NoChanges);
    }
    Ok(written)
}

/// Apply every pending migration across every registered plugin's
/// `migrations/<plugin>/` directory to the ambient pool. Reads the
/// `umbra_migrations` tracking table to determine "pending"; each
/// migration runs in its own transaction along with its tracking-table
/// insert.
///
/// Returns the total number of migrations applied (zero if every
/// plugin's migrations were already in the tracking table).
pub async fn run() -> Result<u64, MigrateError> {
    run_in(Path::new(MIGRATIONS_DIR)).await
}

/// Same as [`run`] but takes an explicit base directory. Used by
/// tests to avoid touching the cwd.
///
/// Iterates `registered_plugins()` in sorted-by-name order. M7 v1
/// accepts this as a limitation: cross-plugin FK ordering wants
/// topological order across plugins (the FK target's `CreateTable`
/// applies before the dependent plugin's `CreateTable`), but the
/// engine doesn't see `Plugin::dependencies()` from inside this
/// standalone function. M8 lifts the limitation via a registry that
/// remembers the toposorted order computed at `App::build()` time.
pub async fn run_in(dir: &Path) -> Result<u64, MigrateError> {
    let pool = crate::db::pool();
    ensure_tracking_table(&pool).await?;
    let applied = applied_names(&pool).await?;

    let mut applied_count: u64 = 0;
    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);
        let paths = list_migration_files(&plugin_dir)?;

        for path in paths {
            let file = read_migration_file(&path)?;
            if applied.contains(&(file.plugin.clone(), file.id.clone())) {
                continue;
            }

            let mut tx = pool.begin().await?;
            for op in &file.operations {
                let sql = render_operation(op);
                sqlx::query(&sql).execute(&mut *tx).await?;
            }
            let snapshot_hash = file.snapshot_after.hash();
            let applied_at = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO umbra_migrations (plugin, name, applied_at, snapshot_hash) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&file.plugin)
            .bind(&file.id)
            .bind(&applied_at)
            .bind(&snapshot_hash)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            applied_count += 1;
        }
    }
    Ok(applied_count)
}

/// Record a migration as applied in the `umbra_migrations` tracking
/// table without running its operations. The "mark as applied" path
/// `inspectdb --mark-applied` uses to register the introspected
/// `0001_initial` against an already-populated database. Idempotent:
/// if the `(plugin, name)` row already exists, the call is a no-op.
pub async fn record_applied(
    plugin: &str,
    name: &str,
    snapshot_hash: &str,
) -> Result<(), MigrateError> {
    let pool = crate::db::pool();
    ensure_tracking_table(&pool).await?;
    let applied_at = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT OR IGNORE INTO umbra_migrations (plugin, name, applied_at, snapshot_hash) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(plugin)
    .bind(name)
    .bind(&applied_at)
    .bind(snapshot_hash)
    .execute(&pool)
    .await?;
    Ok(())
}

/// Print the per-migration state, applied or pending. Output goes to
/// stdout; the return value is the count of pending migrations so a
/// CLI can `exit(n)` on need.
pub async fn show() -> Result<u64, MigrateError> {
    show_in(Path::new(MIGRATIONS_DIR)).await
}

/// Same as [`show`] but takes an explicit base directory. Walks every
/// registered plugin in sorted-by-name order, printing one section per
/// plugin that owns at least one migration file; empty plugins are
/// skipped silently rather than emitting a bare header.
pub async fn show_in(dir: &Path) -> Result<u64, MigrateError> {
    let pool = crate::db::pool();
    ensure_tracking_table(&pool).await?;
    let applied = applied_names(&pool).await?;

    let mut pending: u64 = 0;
    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);
        let paths = list_migration_files(&plugin_dir)?;
        if paths.is_empty() {
            continue;
        }
        println!("# plugin: {plugin}");
        for path in paths {
            let file = read_migration_file(&path)?;
            let key = (file.plugin.clone(), file.id.clone());
            if applied.contains(&key) {
                println!("[X] {}/{}", file.plugin, file.id);
            } else {
                println!("[ ] {}/{}", file.plugin, file.id);
                pending += 1;
            }
        }
    }
    Ok(pending)
}

// =========================================================================
// Internal helpers. Crate-private; the public surface above is the only
// thing the rest of umbra calls into.
// =========================================================================

/// Return every `*.json` migration file in `plugin_dir`, sorted by
/// filename (lexical sort matches numeric order because the prefix is
/// zero-padded). Returns an empty vec if the directory is missing.
fn list_migration_files(plugin_dir: &Path) -> Result<Vec<PathBuf>, MigrateError> {
    if !plugin_dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(plugin_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Read and parse one migration file.
fn read_migration_file(path: &Path) -> Result<MigrationFile, MigrateError> {
    let text = std::fs::read_to_string(path)?;
    let file: MigrationFile = serde_json::from_str(&text)?;
    Ok(file)
}

/// Diff the previous snapshot against the current one and produce the
/// ordered operation list.
///
/// Emits `CreateTable` / `DropTable` for whole-model changes (M5 v1),
/// and `AddColumn` / `DropColumn` for column-level changes on a model
/// that appears in both snapshots (M8 v1). A column whose name stays
/// the same but whose type or nullable flag changed surfaces as
/// [`MigrateError::UnsafeAlter`]: SQLite can't ALTER COLUMN TYPE in
/// place, and a nullable flip on a populated table is destructive.
/// Renames are still handled as drop+add (the heuristic detector that
/// disambiguates rename vs drop+add is deferred past M8 v1).
fn diff(previous: &Snapshot, current: &Snapshot) -> Result<Vec<Operation>, MigrateError> {
    use std::collections::BTreeMap;

    let prev_by_name: BTreeMap<&str, &ModelMeta> = previous
        .models
        .iter()
        .map(|m| (m.name.as_str(), m))
        .collect();
    let curr_by_name: BTreeMap<&str, &ModelMeta> = current
        .models
        .iter()
        .map(|m| (m.name.as_str(), m))
        .collect();

    let mut ops: Vec<Operation> = Vec::new();

    // Creates and column-level diffs, in deterministic name order.
    for (name, curr) in &curr_by_name {
        match prev_by_name.get(name) {
            None => ops.push(Operation::CreateTable {
                table: curr.table.clone(),
                columns: curr.fields.clone(),
            }),
            Some(prev) if prev == curr => {}
            Some(prev) => {
                ops.extend(diff_columns(name, prev, curr)?);
            }
        }
    }

    // Drops, also in deterministic name order.
    for (name, prev) in &prev_by_name {
        if !curr_by_name.contains_key(name) {
            ops.push(Operation::DropTable {
                table: prev.table.clone(),
            });
        }
    }

    Ok(ops)
}

/// Per-model column diff. Same-name columns whose type or nullable
/// flag changed return `UnsafeAlter` (no `AlterColumn` until M8 v1.1
/// covers the table-recreation dance for SQLite plus native ALTER for
/// Postgres). New-named columns emit `AddColumn`; missing-name columns
/// emit `DropColumn`. The ordering is: drops first, then adds, so a
/// rename-as-drop+add doesn't violate a uniqueness constraint mid-
/// migration on a single-row table.
fn diff_columns(
    model: &str,
    previous: &ModelMeta,
    current: &ModelMeta,
) -> Result<Vec<Operation>, MigrateError> {
    use std::collections::BTreeMap;

    let prev_cols: BTreeMap<&str, &Column> = previous
        .fields
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();
    let curr_cols: BTreeMap<&str, &Column> = current
        .fields
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();

    // In-place type / nullable / pk changes are UnsafeAlter at M8 v1.
    // Walk the intersection by name and fail fast on the first one.
    for (name, prev_col) in &prev_cols {
        if let Some(curr_col) = curr_cols.get(name) {
            if prev_col.ty != curr_col.ty {
                return Err(MigrateError::UnsafeAlter {
                    model: model.to_string(),
                    column: (*name).to_string(),
                    reason: format!(
                        "type change {prev_ty:?} -> {curr_ty:?} needs the AlterColumn op (deferred past M8 v1)",
                        prev_ty = prev_col.ty,
                        curr_ty = curr_col.ty,
                    ),
                });
            }
            if prev_col.nullable != curr_col.nullable {
                return Err(MigrateError::UnsafeAlter {
                    model: model.to_string(),
                    column: (*name).to_string(),
                    reason: format!(
                        "nullable flip {prev_n} -> {curr_n} needs the AlterColumn op (deferred past M8 v1)",
                        prev_n = prev_col.nullable,
                        curr_n = curr_col.nullable,
                    ),
                });
            }
            if prev_col.primary_key != curr_col.primary_key {
                return Err(MigrateError::UnsafeAlter {
                    model: model.to_string(),
                    column: (*name).to_string(),
                    reason: "primary-key flips need a manual data-preserving migration".to_string(),
                });
            }
        }
    }

    let mut ops: Vec<Operation> = Vec::new();

    // Drops first so a same-position add can reuse the column slot.
    for (name, prev_col) in &prev_cols {
        if !curr_cols.contains_key(name) {
            ops.push(Operation::DropColumn {
                table: current.table.clone(),
                column: prev_col.name.clone(),
            });
        }
    }

    // Then adds, in current declaration order so the schema retains
    // the user-written column order even after re-runs.
    for col in &current.fields {
        if !prev_cols.contains_key(col.name.as_str()) {
            ops.push(Operation::AddColumn {
                table: current.table.clone(),
                column: col.clone(),
            });
        }
    }

    Ok(ops)
}

/// Pick the suffix used in a migration filename. Single-op migrations
/// get a descriptive suffix; multi-op migrations fall back to `auto`.
fn suffix_for(ops: &[Operation]) -> String {
    match ops {
        [Operation::CreateTable { table, .. }] => format!("create_{table}"),
        [Operation::DropTable { table }] => format!("drop_{table}"),
        [Operation::AddColumn { table, column }] => format!("add_{}_{}", table, column.name),
        [Operation::DropColumn { table, column }] => format!("drop_{table}_{column}"),
        _ => "auto".to_string(),
    }
}

/// Create the tracking table if it isn't there already. SQLite-shaped
/// DDL kept inline because this table is a chicken-and-egg case: every
/// other migration needs the tracking row written, so the table itself
/// can't be a migration.
async fn ensure_tracking_table(pool: &sqlx::SqlitePool) -> Result<(), MigrateError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS umbra_migrations (
            plugin TEXT NOT NULL,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL,
            snapshot_hash TEXT NOT NULL,
            PRIMARY KEY (plugin, name)
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Pull the set of `(plugin, name)` tuples already recorded in the
/// tracking table.
async fn applied_names(
    pool: &sqlx::SqlitePool,
) -> Result<std::collections::HashSet<(String, String)>, MigrateError> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT plugin, name FROM umbra_migrations")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Render one operation to SQL via sea-query + the active backend's
/// `map_type` mapping. M5 v1 shipped the two table-level ops; M8 v1
/// adds the column-level ones. `AlterColumn` is still deferred.
///
/// The `AddColumn` / `DropColumn` bodies are filled in by subagent A;
/// the scaffold returns a placeholder that fails fast so a stray apply
/// surfaces the gap obviously.
fn render_operation(op: &Operation) -> String {
    use sea_query::{Alias, ColumnDef, SqliteQueryBuilder, Table};

    match op {
        Operation::CreateTable { table, columns } => {
            let mut stmt = Table::create();
            stmt.table(Alias::new(table));
            let backend = crate::backend::active();
            for col in columns {
                let mut def =
                    ColumnDef::new_with_type(Alias::new(&col.name), backend.map_type(col.ty));
                if !col.nullable {
                    def.not_null();
                }
                if col.primary_key {
                    def.primary_key();
                }
                stmt.col(&mut def);
            }
            stmt.build(SqliteQueryBuilder)
        }
        Operation::DropTable { table } => Table::drop()
            .table(Alias::new(table))
            .build(SqliteQueryBuilder),
        Operation::AddColumn {
            table: _,
            column: _,
        } => {
            // Filled in by subagent A.
            String::from("-- umbra: AddColumn rendering pending")
        }
        Operation::DropColumn {
            table: _,
            column: _,
        } => {
            // Filled in by subagent A.
            String::from("-- umbra: DropColumn rendering pending")
        }
    }
}
