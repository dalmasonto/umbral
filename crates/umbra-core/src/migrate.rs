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

use crate::backend::DatabaseBackend;
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

/// Whether the model registry has been initialised. False before
/// `App::build()` has run; true after the phase-3 `init_plugins`
/// call publishes the per-plugin map. Used by system checks that
/// walk the registry — they return an empty result when the
/// registry isn't ready rather than panicking (so low-level tests
/// that drive `check::run_all` without booting an App keep working).
pub fn is_initialised() -> bool {
    REGISTRY.get().is_some()
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

/// Per-model database alias (`Model::NAME -> alias`) published by
/// `App::build()` after walking each registered plugin's
/// `Plugin::database()`. Models whose plugin returned `None` are
/// absent from the map; QuerySet's `resolve_pool` falls back to the
/// `"default"` alias for those. Lookup is `O(1)` on a `HashMap`.
static MODEL_ALIASES: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();

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

/// Publish the per-model alias routing. Called by `App::build()`
/// during phase 3 after walking every plugin's `Plugin::database()`.
/// Plugins that returned `None` contribute no entries; only the
/// explicit overrides land here.
pub(crate) fn init_model_aliases(map: std::collections::HashMap<String, String>) {
    MODEL_ALIASES
        .set(map)
        .expect("umbra::migrate::init_model_aliases called more than once");
}

/// Look up the database alias for one model. Returns `None` if the
/// model isn't routed explicitly (the caller falls back to the
/// `"default"` pool); returns `None` even when the alias map hasn't
/// been initialised so low-level tests that drive `init_plugins`
/// directly don't have to wire a second call.
pub fn model_alias(model_name: &str) -> Option<String> {
    MODEL_ALIASES.get()?.get(model_name).cloned()
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
    /// Alter a column's nullable flag (the only safe in-place change
    /// the engine ships at M5.1). Self-contained: carries the full
    /// new column list so the SQLite table-recreation dance can
    /// rebuild the schema without re-reading the snapshot. The
    /// `column` field names the specific column that triggered the
    /// alter (used for the filename suffix and diagnostics); the
    /// `new_columns` list is the post-change schema.
    AlterColumn {
        table: String,
        column: String,
        new_columns: Vec<Column>,
    },
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
    /// For `SqlType::ForeignKey` columns: the SQL table name of the
    /// referenced model. `None` for all non-FK columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fk_target: Option<String>,
}

impl From<&FieldSpec> for Column {
    fn from(f: &FieldSpec) -> Self {
        Self {
            name: f.name.to_string(),
            ty: f.ty,
            primary_key: f.primary_key,
            nullable: f.nullable,
            fk_target: f.fk_target.map(|s| s.to_string()),
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

/// The state of a single migration from the perspective of drift detection.
/// Returned inside [`DriftReport`] so callers can decide how to handle each
/// state independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationStatus {
    /// The migration is recorded in the tracking table AND the file
    /// exists on disk. Normal applied state.
    Applied,
    /// The migration is recorded in the tracking table BUT the
    /// corresponding file is missing from disk. The database is ahead
    /// of what version control has; recovering requires restoring the
    /// file or running with `--allow-drift`.
    AppliedButMissing,
    /// The migration file exists on disk AND its sequence number is
    /// lower than the highest applied migration for this plugin, but it
    /// is not recorded in the tracking table. Looks like someone dropped
    /// a migration file back into a directory after a teammate already
    /// applied later ones. Should warn, not error.
    OutOfOrder,
    /// Normal pending state: the file is on disk and its sequence number
    /// is higher than anything applied. Ready to apply.
    Pending,
}

/// Per-migration entry inside a [`DriftReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationEntry {
    pub plugin: String,
    pub name: String,
    pub status: MigrationStatus,
}

/// The output of [`detect_drift`]: one entry per migration (applied or
/// on-disk), categorised into the four states above.
///
/// The caller inspects `has_critical_drift()` to decide whether to abort
/// before applying migrations. Surfaced by `show_in_with_drift` for
/// `showmigrations` and checked by `run_in_with_drift_check` before
/// executing any SQL.
#[derive(Debug, Clone, Default)]
pub struct DriftReport {
    pub entries: Vec<MigrationEntry>,
}

impl DriftReport {
    /// Returns true when at least one migration is `AppliedButMissing`.
    /// This state means the tracking table references a file that no
    /// longer exists on disk — the operator needs to act before it is
    /// safe to continue applying new migrations.
    pub fn has_critical_drift(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.status == MigrationStatus::AppliedButMissing)
    }

    /// All migrations with `AppliedButMissing` status. Convenience
    /// accessor for building the error message.
    pub fn missing_on_disk(&self) -> Vec<&MigrationEntry> {
        self.entries
            .iter()
            .filter(|e| e.status == MigrationStatus::AppliedButMissing)
            .collect()
    }
}

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
    /// The tracking table records migrations that no longer have
    /// corresponding files on disk. Carries the list of missing names.
    /// The operator must either restore the files from VCS or run with
    /// `--allow-drift` to proceed despite the inconsistency.
    DriftDetected { missing: Vec<(String, String)> },
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
            MigrateError::DriftDetected { missing } => {
                let names: Vec<String> = missing
                    .iter()
                    .map(|(plugin, name)| format!("{plugin}/{name}"))
                    .collect();
                write!(
                    f,
                    "umbra migrate: drift detected — the following migrations are recorded in \
                     the tracking table but their files are missing from disk:\n  {}\n\
                     Restore the files from VCS or run `umbra migrate --allow-drift` to \
                     proceed despite the inconsistency.",
                    names.join("\n  ")
                )
            }
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
///
/// This variant performs a drift check before executing any SQL. If
/// any migration is `AppliedButMissing` (in the DB but not on disk),
/// the call returns [`MigrateError::DriftDetected`] listing the
/// missing names. Pass `allow_drift = true` (via [`run_checked_in`])
/// to suppress the error and proceed anyway (with a warning printed to
/// stderr).
pub async fn run() -> Result<u64, MigrateError> {
    run_checked(false).await
}

/// Same as [`run`] but controls drift handling.
/// `allow_drift = true` corresponds to the `--allow-drift` CLI flag:
/// the command logs a warning and proceeds even if some applied
/// migrations are missing on disk.
pub async fn run_checked(allow_drift: bool) -> Result<u64, MigrateError> {
    run_checked_in(Path::new(MIGRATIONS_DIR), allow_drift).await
}

/// Same as [`run_checked`] but takes an explicit base directory.
pub async fn run_checked_in(dir: &Path, allow_drift: bool) -> Result<u64, MigrateError> {
    match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(p) => run_in_sqlite_checked(dir, p, allow_drift).await,
        crate::db::DbPool::Postgres(p) => run_in_postgres_checked(dir, p, allow_drift).await,
    }
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
///
/// This legacy entry point does NOT perform drift checking so the
/// existing tests (which bypass drift by design) keep passing. New
/// callers should prefer [`run_checked_in`].
pub async fn run_in(dir: &Path) -> Result<u64, MigrateError> {
    match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(p) => run_in_sqlite(dir, p).await,
        crate::db::DbPool::Postgres(p) => run_in_postgres(dir, p).await,
    }
}

/// SQLite path for [`run_in`]. Reads / writes the tracking table with
/// `?` placeholders and `INSERT OR IGNORE`.
async fn run_in_sqlite(dir: &Path, pool: &sqlx::SqlitePool) -> Result<u64, MigrateError> {
    ensure_tracking_table_sqlite(pool).await?;
    let applied = applied_names_sqlite(pool).await?;

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
                for sql in render_operation(op) {
                    sqlx::query(&sql).execute(&mut *tx).await?;
                }
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

/// Postgres path for [`run_in`]. The tracking-table DDL is dialect-
/// neutral; placeholders are `$1..$N` and the conflict clause is
/// `ON CONFLICT DO NOTHING` rather than SQLite's `INSERT OR IGNORE`.
async fn run_in_postgres(dir: &Path, pool: &sqlx::PgPool) -> Result<u64, MigrateError> {
    ensure_tracking_table_postgres(pool).await?;
    let applied = applied_names_postgres(pool).await?;

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
                for sql in render_operation(op) {
                    sqlx::query(&sql).execute(&mut *tx).await?;
                }
            }
            let snapshot_hash = file.snapshot_after.hash();
            let applied_at = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO umbra_migrations (plugin, name, applied_at, snapshot_hash) \
                 VALUES ($1, $2, $3, $4)",
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

/// SQLite drift-checking path for `run_checked_in`.
///
/// Reads the applied set, runs `detect_all_drift`, and either errors
/// (if `allow_drift = false` and critical drift is found) or logs a
/// warning and proceeds (if `allow_drift = true`). Then delegates to
/// `run_in_sqlite` for the actual apply loop.
async fn run_in_sqlite_checked(
    dir: &Path,
    pool: &sqlx::SqlitePool,
    allow_drift: bool,
) -> Result<u64, MigrateError> {
    ensure_tracking_table_sqlite(pool).await?;
    let applied = applied_names_sqlite(pool).await?;
    let report = detect_all_drift(&applied, dir)?;

    if report.has_critical_drift() {
        if allow_drift {
            let missing = report.missing_on_disk();
            for entry in &missing {
                eprintln!(
                    "warning: umbra migrate --allow-drift: migration {}/{} is recorded in \
                     the tracking table but the file is missing from disk; proceeding.",
                    entry.plugin, entry.name
                );
            }
        } else {
            let missing: Vec<(String, String)> = report
                .missing_on_disk()
                .iter()
                .map(|e| (e.plugin.clone(), e.name.clone()))
                .collect();
            return Err(MigrateError::DriftDetected { missing });
        }
    }

    // Emit warnings for out-of-order files.
    for entry in report
        .entries
        .iter()
        .filter(|e| e.status == MigrationStatus::OutOfOrder)
    {
        eprintln!(
            "warning: umbra migrate: migration {}/{} is on disk but appears before the \
             last applied migration for this plugin; it looks like a file was restored \
             after a teammate already applied later ones.",
            entry.plugin, entry.name
        );
    }

    run_in_sqlite(dir, pool).await
}

/// Postgres drift-checking path for `run_checked_in`. Same logic as
/// `run_in_sqlite_checked` but uses the Postgres applied-set reader.
async fn run_in_postgres_checked(
    dir: &Path,
    pool: &sqlx::PgPool,
    allow_drift: bool,
) -> Result<u64, MigrateError> {
    ensure_tracking_table_postgres(pool).await?;
    let applied = applied_names_postgres(pool).await?;
    let report = detect_all_drift(&applied, dir)?;

    if report.has_critical_drift() {
        if allow_drift {
            let missing = report.missing_on_disk();
            for entry in &missing {
                eprintln!(
                    "warning: umbra migrate --allow-drift: migration {}/{} is recorded in \
                     the tracking table but the file is missing from disk; proceeding.",
                    entry.plugin, entry.name
                );
            }
        } else {
            let missing: Vec<(String, String)> = report
                .missing_on_disk()
                .iter()
                .map(|e| (e.plugin.clone(), e.name.clone()))
                .collect();
            return Err(MigrateError::DriftDetected { missing });
        }
    }

    for entry in report
        .entries
        .iter()
        .filter(|e| e.status == MigrationStatus::OutOfOrder)
    {
        eprintln!(
            "warning: umbra migrate: migration {}/{} is on disk but appears before the \
             last applied migration for this plugin; it looks like a file was restored \
             after a teammate already applied later ones.",
            entry.plugin, entry.name
        );
    }

    run_in_postgres(dir, pool).await
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
    let applied_at = chrono::Utc::now().to_rfc3339();
    match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(pool) => {
            ensure_tracking_table_sqlite(pool).await?;
            sqlx::query(
                "INSERT OR IGNORE INTO umbra_migrations \
                 (plugin, name, applied_at, snapshot_hash) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(plugin)
            .bind(name)
            .bind(&applied_at)
            .bind(snapshot_hash)
            .execute(pool)
            .await?;
        }
        crate::db::DbPool::Postgres(pool) => {
            ensure_tracking_table_postgres(pool).await?;
            sqlx::query(
                "INSERT INTO umbra_migrations \
                 (plugin, name, applied_at, snapshot_hash) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (plugin, name) DO NOTHING",
            )
            .bind(plugin)
            .bind(name)
            .bind(&applied_at)
            .bind(snapshot_hash)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

// =========================================================================
// Drift detection — gap 24.
// =========================================================================

/// Compute the drift report for a single plugin directory. Compares the
/// set of `(plugin, name)` pairs recorded in the tracking table against
/// the migration files present on disk and classifies each into one of
/// the four [`MigrationStatus`] states.
///
/// `applied` is the full set of `(plugin, name)` tuples already read
/// from the tracking table (shared across plugins to avoid extra DB
/// round-trips). `plugin_dir` is the on-disk directory for this plugin;
/// an absent directory is treated the same as an empty one.
///
/// # Classification
///
/// - File present + in DB → `Applied`
/// - File absent + in DB → `AppliedButMissing`
/// - File present + not in DB + seq ≤ max_applied_seq → `OutOfOrder`
/// - File present + not in DB + seq > max_applied_seq → `Pending`
///
/// The sequence number is the numeric prefix of the migration name
/// (e.g. `0001` in `0001_create_post`). Absence of any applied
/// migration for this plugin means `max_applied_seq = 0`.
pub fn detect_drift(
    plugin: &str,
    applied: &std::collections::HashSet<(String, String)>,
    plugin_dir: &Path,
) -> Result<Vec<MigrationEntry>, MigrateError> {
    // Collect on-disk migration names (the id, not the full path).
    let paths = list_migration_files(plugin_dir)?;
    let mut on_disk: Vec<String> = Vec::new();
    for path in &paths {
        let file = read_migration_file(path)?;
        on_disk.push(file.id.clone());
    }

    // Pull every tracking-table entry for this plugin.
    let plugin_applied: Vec<&str> = applied
        .iter()
        .filter(|(p, _)| p == plugin)
        .map(|(_, n)| n.as_str())
        .collect();

    // Highest sequence number among applied migrations for this plugin.
    let max_applied_seq: u32 = plugin_applied
        .iter()
        .filter_map(|name| name.split('_').next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);

    let on_disk_set: std::collections::HashSet<&str> = on_disk.iter().map(|s| s.as_str()).collect();

    let mut entries: Vec<MigrationEntry> = Vec::new();

    // Walk on-disk files in order.
    for name in &on_disk {
        let key = (plugin.to_string(), name.clone());
        let status = if applied.contains(&key) {
            MigrationStatus::Applied
        } else {
            // Determine this migration's sequence number.
            let seq: u32 = name
                .split('_')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if seq <= max_applied_seq && max_applied_seq > 0 {
                MigrationStatus::OutOfOrder
            } else {
                MigrationStatus::Pending
            }
        };
        entries.push(MigrationEntry {
            plugin: plugin.to_string(),
            name: name.clone(),
            status,
        });
    }

    // Walk applied entries not present on disk.
    for name in &plugin_applied {
        if !on_disk_set.contains(*name) {
            entries.push(MigrationEntry {
                plugin: plugin.to_string(),
                name: (*name).to_string(),
                status: MigrationStatus::AppliedButMissing,
            });
        }
    }

    // Sort: applied-but-missing entries bubble after their expected
    // position is not determinable; sort all entries by name for a
    // deterministic order. In practice, applied-but-missing names
    // are still prefixed with the numeric sequence so lexical sort
    // yields the right display order.
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(entries)
}

/// Detect drift across every registered plugin and return a combined
/// [`DriftReport`]. Called by `run_in_checked` before executing SQL
/// and by `show_in` when displaying the four-state list.
///
/// `applied` is already fetched from the DB; `dir` is the migrations
/// root directory.
pub fn detect_all_drift(
    applied: &std::collections::HashSet<(String, String)>,
    dir: &Path,
) -> Result<DriftReport, MigrateError> {
    let mut all_entries: Vec<MigrationEntry> = Vec::new();

    // Also surface any tracking-table entries whose plugin directory
    // doesn't appear in the registered-plugins list — a plugin was
    // removed entirely but its DB rows remain.
    let mut seen_plugins: std::collections::HashSet<String> = std::collections::HashSet::new();

    for plugin in plugin_order() {
        seen_plugins.insert(plugin.clone());
        let plugin_dir = dir.join(&plugin);
        let entries = detect_drift(&plugin, applied, &plugin_dir)?;
        all_entries.extend(entries);
    }

    // Any applied entries whose plugin is not in the registered set at
    // all — treat them as AppliedButMissing (the whole plugin is gone).
    for (plugin, name) in applied {
        if !seen_plugins.contains(plugin.as_str()) {
            all_entries.push(MigrationEntry {
                plugin: plugin.clone(),
                name: name.clone(),
                status: MigrationStatus::AppliedButMissing,
            });
        }
    }

    Ok(DriftReport {
        entries: all_entries,
    })
}

/// Record a migration as applied in the tracking table WITHOUT running
/// its SQL operations. The `--fake` recovery path: the schema already
/// exists (e.g. the migration was run outside umbra, or the DB was
/// bootstrapped from a dump) and the operator wants to bring the
/// tracking table into sync without re-executing the DDL.
///
/// Idempotent: if `(plugin, name)` is already in the table the call
/// is a no-op (same behaviour as `record_applied`).
///
/// The snapshot hash is derived from the migration file on disk.
/// Returns `MigrateError::Io` if the file can't be found (the caller
/// should verify the name before calling this).
pub async fn fake_apply(plugin: &str, name: &str) -> Result<(), MigrateError> {
    fake_apply_in(plugin, name, Path::new(MIGRATIONS_DIR)).await
}

/// Same as [`fake_apply`] but takes an explicit migrations base dir.
/// Used by tests and by the CLI when `--migrations-dir` is passed.
pub async fn fake_apply_in(plugin: &str, name: &str, dir: &Path) -> Result<(), MigrateError> {
    let path = dir.join(plugin).join(format!("{name}.json"));
    let file = read_migration_file(&path)?;
    let snapshot_hash = file.snapshot_after.hash();
    record_applied(plugin, name, &snapshot_hash).await
}

/// For every registered plugin's first migration (`0001_*`), check
/// whether the tables that migration would create already exist in the
/// database. If they do, fake-apply the migration (mark it applied
/// without running its SQL).
///
/// This is Django's `--fake-initial` path: the operator has a database
/// bootstrapped outside umbra (a dump restore, a manual `CREATE TABLE`,
/// or a previous schema manager) and wants to bring the tracking table
/// into sync so subsequent `migrate` calls apply only the genuine
/// deltas.
///
/// Returns the number of plugins whose `0001_*` migration was
/// fake-applied. Zero means either no `0001_*` file exists or the
/// target tables were absent (in which case normal `migrate` should be
/// run to create them).
pub async fn fake_initial() -> Result<u64, MigrateError> {
    fake_initial_in(Path::new(MIGRATIONS_DIR)).await
}

/// Same as [`fake_initial`] but takes an explicit migrations base dir.
pub async fn fake_initial_in(dir: &Path) -> Result<u64, MigrateError> {
    match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(pool) => fake_initial_sqlite(dir, pool).await,
        crate::db::DbPool::Postgres(pool) => fake_initial_postgres(dir, pool).await,
    }
}

/// SQLite path for [`fake_initial_in`].
async fn fake_initial_sqlite(dir: &Path, pool: &sqlx::SqlitePool) -> Result<u64, MigrateError> {
    ensure_tracking_table_sqlite(pool).await?;
    let applied = applied_names_sqlite(pool).await?;
    let mut count: u64 = 0;

    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);
        let paths = list_migration_files(&plugin_dir)?;

        // Find the first migration file (lowest sequence number).
        let first = paths.first();
        let first = match first {
            Some(p) => p,
            None => continue,
        };
        let file = read_migration_file(first)?;

        // Skip if already applied.
        if applied.contains(&(file.plugin.clone(), file.id.clone())) {
            continue;
        }

        // Check whether the tables the first migration would create
        // already exist in the database.
        let tables_to_create: Vec<&str> = file
            .operations
            .iter()
            .filter_map(|op| match op {
                Operation::CreateTable { table, .. } => Some(table.as_str()),
                _ => None,
            })
            .collect();

        if tables_to_create.is_empty() {
            continue;
        }

        // All tables present → fake-apply.
        let mut all_present = true;
        for table in &tables_to_create {
            let exists: Option<(String,)> =
                sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
                    .bind(*table)
                    .fetch_optional(pool)
                    .await?;
            if exists.is_none() {
                all_present = false;
                break;
            }
        }

        if all_present {
            let snapshot_hash = file.snapshot_after.hash();
            let applied_at = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT OR IGNORE INTO umbra_migrations \
                 (plugin, name, applied_at, snapshot_hash) VALUES (?, ?, ?, ?)",
            )
            .bind(&file.plugin)
            .bind(&file.id)
            .bind(&applied_at)
            .bind(&snapshot_hash)
            .execute(pool)
            .await?;
            count += 1;
        }
    }

    Ok(count)
}

/// Postgres path for [`fake_initial_in`].
async fn fake_initial_postgres(dir: &Path, pool: &sqlx::PgPool) -> Result<u64, MigrateError> {
    ensure_tracking_table_postgres(pool).await?;
    let applied = applied_names_postgres(pool).await?;
    let mut count: u64 = 0;

    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);
        let paths = list_migration_files(&plugin_dir)?;

        let first = paths.first();
        let first = match first {
            Some(p) => p,
            None => continue,
        };
        let file = read_migration_file(first)?;

        if applied.contains(&(file.plugin.clone(), file.id.clone())) {
            continue;
        }

        let tables_to_create: Vec<&str> = file
            .operations
            .iter()
            .filter_map(|op| match op {
                Operation::CreateTable { table, .. } => Some(table.as_str()),
                _ => None,
            })
            .collect();

        if tables_to_create.is_empty() {
            continue;
        }

        let mut all_present = true;
        for table in &tables_to_create {
            let exists: Option<(String,)> = sqlx::query_as(
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = 'public' AND table_name = $1",
            )
            .bind(*table)
            .fetch_optional(pool)
            .await?;
            if exists.is_none() {
                all_present = false;
                break;
            }
        }

        if all_present {
            let snapshot_hash = file.snapshot_after.hash();
            let applied_at = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO umbra_migrations \
                 (plugin, name, applied_at, snapshot_hash) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (plugin, name) DO NOTHING",
            )
            .bind(&file.plugin)
            .bind(&file.id)
            .bind(&applied_at)
            .bind(&snapshot_hash)
            .execute(pool)
            .await?;
            count += 1;
        }
    }

    Ok(count)
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
///
/// Four-state output (gap 24):
///
/// - `[X]` applied and file present on disk (normal)
/// - `[ ]` pending (on disk, not yet applied, sequence after last applied)
/// - `[!]` applied but missing on disk (drift — tracking table ahead of VCS)
/// - `[?]` on disk but out of order (sequence before last applied, not in DB)
pub async fn show_in(dir: &Path) -> Result<u64, MigrateError> {
    let applied = match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(pool) => {
            ensure_tracking_table_sqlite(pool).await?;
            applied_names_sqlite(pool).await?
        }
        crate::db::DbPool::Postgres(pool) => {
            ensure_tracking_table_postgres(pool).await?;
            applied_names_postgres(pool).await?
        }
    };

    let report = detect_all_drift(&applied, dir)?;

    // Group by plugin for display.
    let mut by_plugin: std::collections::BTreeMap<&str, Vec<&MigrationEntry>> =
        std::collections::BTreeMap::new();
    for entry in &report.entries {
        by_plugin
            .entry(entry.plugin.as_str())
            .or_default()
            .push(entry);
    }

    let mut pending: u64 = 0;
    for (plugin, entries) in &by_plugin {
        if entries.is_empty() {
            continue;
        }
        println!("# plugin: {plugin}");
        for entry in entries {
            let marker = match entry.status {
                MigrationStatus::Applied => "[X]",
                MigrationStatus::Pending => {
                    pending += 1;
                    "[ ]"
                }
                MigrationStatus::AppliedButMissing => "[!]",
                MigrationStatus::OutOfOrder => "[?]",
            };
            println!("{marker} {}/{}", entry.plugin, entry.name);
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
///
/// `pub` (not `pub(crate)`) so the M8 integration tests can drive the
/// diff directly with hand-built snapshots. Spec 06 calls the diff
/// the engine's contract; exposing it lets the tests pin every column-
/// level scenario without laundering snapshots through the process-
/// wide registry first.
pub fn diff(previous: &Snapshot, current: &Snapshot) -> Result<Vec<Operation>, MigrateError> {
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

    // Walk the intersection by name. Type and pk changes still
    // surface as UnsafeAlter (need cast semantics + a primary-key
    // rebuild dance that's not in scope at the M5.1 close). Nullable
    // flips become AlterColumn ops, rendered via the SQLite
    // table-recreation dance.
    let mut alter_columns: Vec<&str> = Vec::new();
    for (name, prev_col) in &prev_cols {
        if let Some(curr_col) = curr_cols.get(name) {
            if prev_col.ty != curr_col.ty {
                return Err(MigrateError::UnsafeAlter {
                    model: model.to_string(),
                    column: (*name).to_string(),
                    reason: format!(
                        "type change {prev_ty:?} -> {curr_ty:?} needs cast semantics not yet modelled",
                        prev_ty = prev_col.ty,
                        curr_ty = curr_col.ty,
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
            if prev_col.nullable != curr_col.nullable {
                alter_columns.push(*name);
            }
        }
    }

    let mut ops: Vec<Operation> = Vec::new();

    // AlterColumn ops first, in name order. One AlterColumn per
    // changed column; each carries the full new schema so the render
    // can rebuild without further context. Multiple nullable flips on
    // one table generate multiple AlterColumns; the apply loop runs
    // them sequentially (each is a table-recreation, so back-to-back
    // alters drop and recreate twice; the cost is acceptable while
    // M5.1 ships the simple case).
    let new_columns: Vec<Column> = current.fields.clone();
    for name in alter_columns {
        ops.push(Operation::AlterColumn {
            table: current.table.clone(),
            column: name.to_string(),
            new_columns: new_columns.clone(),
        });
    }

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
        [Operation::AlterColumn { table, column, .. }] => format!("alter_{table}_{column}"),
        _ => "auto".to_string(),
    }
}

/// Create the tracking table if it isn't there already. The DDL is
/// dialect-neutral (TEXT + composite PK is valid SQL on both shipped
/// backends), but the executor type isn't — sqlx::query is generic
/// over the database, so each backend gets its own thin wrapper.
///
/// Kept inline because this table is a chicken-and-egg case: every
/// other migration needs the tracking row written, so the table
/// itself can't be a migration.
async fn ensure_tracking_table_sqlite(pool: &sqlx::SqlitePool) -> Result<(), MigrateError> {
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

/// Postgres counterpart to [`ensure_tracking_table_sqlite`].
async fn ensure_tracking_table_postgres(pool: &sqlx::PgPool) -> Result<(), MigrateError> {
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
/// tracking table (SQLite).
async fn applied_names_sqlite(
    pool: &sqlx::SqlitePool,
) -> Result<std::collections::HashSet<(String, String)>, MigrateError> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT plugin, name FROM umbra_migrations")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Postgres counterpart to [`applied_names_sqlite`].
async fn applied_names_postgres(
    pool: &sqlx::PgPool,
) -> Result<std::collections::HashSet<(String, String)>, MigrateError> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT plugin, name FROM umbra_migrations")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Render one operation to a list of SQL statements via sea-query.
///
/// Dispatches on the ambient backend's [`crate::backend::active`]
/// name; SQLite and Postgres are the two shipped dialects. Most ops
/// produce one statement; `AlterColumn` produces either the SQLite
/// table-recreation dance (`CREATE _umbra_new` + `INSERT ... SELECT`
/// + `DROP` + `RENAME`) or a single native `ALTER TABLE ... ALTER
/// COLUMN ... SET/DROP NOT NULL` on Postgres.
///
/// The apply loop in `run_in` executes each statement in order inside
/// the same transaction.
///
/// `AddColumn` ignores the `primary_key` flag: neither SQLite nor
/// Postgres lets a primary key be added to an existing table without
/// a table-recreation step, and the autodetector won't route a
/// pk-flagged column through `AddColumn` anyway. A hand-edited
/// migration that sets the flag is taken to mean "the user is taking
/// responsibility".
fn render_operation(op: &Operation) -> Vec<String> {
    render_operation_for(op, crate::backend::active().name())
}

/// Render one operation against an explicit backend name. The
/// dispatching seam — the public [`render_operation`] is just
/// `render_operation_for(op, backend::active().name())`. Splitting
/// the two lets tests render Postgres DDL without installing the
/// process-wide ambient backend (the `OnceLock` can only be set once,
/// so `App::build` and tests would otherwise collide).
///
/// Panics on unknown backend names; only `"sqlite"` and `"postgres"`
/// are shipped in Phase 2.
pub fn render_operation_for(op: &Operation, backend_name: &str) -> Vec<String> {
    match backend_name {
        "sqlite" => render_operation_sqlite(op),
        "postgres" => render_operation_postgres(op),
        other => panic!(
            "umbra::migrate: no DDL renderer for backend `{other}`; \
             Phase 2 ships sqlite and postgres only"
        ),
    }
}

/// SQLite-dialect rendering for one operation.
fn render_operation_sqlite(op: &Operation) -> Vec<String> {
    use sea_query::{Alias, SqliteQueryBuilder, Table};

    match op {
        Operation::CreateTable { table, columns } => {
            let mut stmt = Table::create();
            stmt.table(Alias::new(table));
            for col in columns {
                let mut def = build_column_def_sqlite(col);
                stmt.col(&mut def);
            }
            vec![stmt.build(SqliteQueryBuilder)]
        }
        Operation::DropTable { table } => vec![
            Table::drop()
                .table(Alias::new(table))
                .build(SqliteQueryBuilder),
        ],
        Operation::AddColumn { table, column } => {
            let mut stmt = Table::alter();
            stmt.table(Alias::new(table));
            let mut def = build_column_def_sqlite(column);
            stmt.add_column(&mut def);
            vec![stmt.build(SqliteQueryBuilder)]
        }
        Operation::DropColumn { table, column } => vec![
            Table::alter()
                .table(Alias::new(table))
                .drop_column(Alias::new(column))
                .build(SqliteQueryBuilder),
        ],
        Operation::AlterColumn {
            table,
            column: _,
            new_columns,
        } => render_alter_column_dance_sqlite(table, new_columns),
    }
}

/// Postgres-dialect rendering for one operation.
///
/// Postgres has native `ALTER COLUMN` so `AlterColumn` doesn't need
/// the SQLite table-recreation dance; it lowers to a single statement.
/// Integer primary keys use sea-query's `auto_increment()` flag, which
/// the Postgres query builder lowers to `BIGSERIAL` / `SERIAL` rather
/// than SQLite's `INTEGER PRIMARY KEY AUTOINCREMENT` quirk.
fn render_operation_postgres(op: &Operation) -> Vec<String> {
    use sea_query::{Alias, PostgresQueryBuilder, Table};

    match op {
        Operation::CreateTable { table, columns } => {
            let mut stmt = Table::create();
            stmt.table(Alias::new(table));
            for col in columns {
                let mut def = build_column_def_postgres(col);
                stmt.col(&mut def);
            }
            vec![stmt.build(PostgresQueryBuilder)]
        }
        Operation::DropTable { table } => vec![
            Table::drop()
                .table(Alias::new(table))
                .build(PostgresQueryBuilder),
        ],
        Operation::AddColumn { table, column } => {
            let mut stmt = Table::alter();
            stmt.table(Alias::new(table));
            let mut def = build_column_def_postgres(column);
            stmt.add_column(&mut def);
            vec![stmt.build(PostgresQueryBuilder)]
        }
        Operation::DropColumn { table, column } => vec![
            Table::alter()
                .table(Alias::new(table))
                .drop_column(Alias::new(column))
                .build(PostgresQueryBuilder),
        ],
        Operation::AlterColumn {
            table,
            column,
            new_columns,
        } => render_alter_column_postgres(table, column, new_columns),
    }
}

/// The SQLite table-recreation dance for `AlterColumn`. SQLite has no
/// in-place `ALTER COLUMN`, so the only safe way to flip a column's
/// nullable flag is to rebuild the table:
///
/// 1. `CREATE TABLE _umbra_new_<table>` with the new schema.
/// 2. `INSERT ... SELECT` to copy every row from the old table.
/// 3. `DROP TABLE <table>`.
/// 4. `ALTER TABLE _umbra_new_<table> RENAME TO <table>`.
///
/// Wrapped in a transaction by the caller. Indexes, triggers, and FK
/// targets aren't preserved at M5.1 because umbra-core's schema model
/// doesn't yet carry them; once it does, this routine picks them up
/// by rebuilding them at step 1.
///
/// Nullable `TRUE -> FALSE` fails at step 2 if any row holds NULL,
/// which is the correct data-integrity behaviour. Nullable
/// `FALSE -> TRUE` always succeeds.
fn render_alter_column_dance_sqlite(table: &str, new_columns: &[Column]) -> Vec<String> {
    use sea_query::{Alias, SqliteQueryBuilder, Table};

    let tmp = format!("_umbra_new_{table}");

    // Step 1 — CREATE TABLE _umbra_new_<table>.
    let mut create = Table::create();
    create.table(Alias::new(&tmp));
    for col in new_columns {
        let mut def = build_column_def_sqlite(col);
        create.col(&mut def);
    }

    // Step 2 — INSERT ... SELECT. Same column list both sides; the
    // dance only handles nullable flips (columns are otherwise
    // identical). Each name is double-quoted so SQLite identifier
    // rules don't bite on reserved words.
    let column_list = new_columns
        .iter()
        .map(|c| format!("\"{}\"", c.name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql =
        format!("INSERT INTO \"{tmp}\" ({column_list}) SELECT {column_list} FROM \"{table}\"");

    // Step 3 — DROP TABLE <table>.
    let drop_sql = Table::drop()
        .table(Alias::new(table))
        .build(SqliteQueryBuilder);

    // Step 4 — ALTER TABLE _umbra_new_<table> RENAME TO <table>.
    let rename_sql = Table::rename()
        .table(Alias::new(&tmp), Alias::new(table))
        .build(SqliteQueryBuilder);

    vec![
        create.build(SqliteQueryBuilder),
        insert_sql,
        drop_sql,
        rename_sql,
    ]
}

/// Native Postgres `AlterColumn`. Postgres supports
/// `ALTER TABLE x ALTER COLUMN y SET NOT NULL` and
/// `ALTER TABLE x ALTER COLUMN y DROP NOT NULL` in place, so the
/// SQLite table-recreation dance isn't needed. Lowers to a single
/// statement.
///
/// `SET NOT NULL` fails at the server if any row holds NULL on `y`,
/// matching SQLite's INSERT-time failure on the dance — the
/// data-integrity contract is identical between backends.
///
/// `column` is the field name that triggered the flip; `new_columns`
/// is the post-change schema (carried for parity with the SQLite
/// dance, though Postgres only needs the one column).
fn render_alter_column_postgres(table: &str, column: &str, new_columns: &[Column]) -> Vec<String> {
    let new = new_columns.iter().find(|c| c.name == column).expect(
        "umbra::migrate: AlterColumn op references a column missing from new_columns; \
             this is a bug in `diff_columns`",
    );

    // Postgres treats `SET NOT NULL` and `DROP NOT NULL` as idempotent
    // against a column whose flag already matches — flipping to the new
    // state is always safe to issue.
    let clause = if new.nullable {
        "DROP NOT NULL"
    } else {
        "SET NOT NULL"
    };

    let q_table = quote_pg_ident(table);
    let q_column = quote_pg_ident(column);

    vec![format!(
        "ALTER TABLE {q_table} ALTER COLUMN {q_column} {clause}"
    )]
}

/// Quote a SQL identifier the Postgres way: wrap in double quotes,
/// escape inner double quotes by doubling them. Matches sea-query's
/// `PostgresQueryBuilder` output for identifiers so the rendered
/// statements look uniform.
fn quote_pg_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Build a SQLite `ColumnDef`. SQLite has one important quirk: its
/// ROWID-alias mechanic (which gives a primary-key column auto-
/// increment behaviour out of the box) only fires when the column's
/// type is the exact text `INTEGER` — case-insensitive but no other
/// variant. `BIGINT PRIMARY KEY`, even on a column the M3 derive
/// declared as `i64`, does NOT auto-increment, so an `INSERT INTO t
/// (other_col) VALUES (...)` without an explicit PK value fails the
/// NOT NULL constraint. Every umbra user with an `id: i64` model
/// would hit this without the override.
///
/// The fix: when a column is a primary key with an integer SqlType
/// (Integer or BigInt), force the rendered type to `Integer` and
/// attach `auto_increment()` so the generated DDL reads `"id" integer
/// NOT NULL PRIMARY KEY AUTOINCREMENT`. SQLite stores both `i32` and
/// `i64` as INTEGER affinity anyway, so the override is a no-op
/// semantically — the rows that round-trip through `sqlx::FromRow`
/// deserialize back into `i64` cleanly.
///
/// For `SqlType::Uuid` PKs: SQLite stores UUIDs as TEXT. No
/// `DEFAULT gen_random_uuid()` is emitted; the application must supply
/// the UUID at create time (or pass `Uuid::nil()` to trigger the
/// omit-on-insert sentinel that leaves the column to a future default).
///
/// For `SqlType::ForeignKey` columns: rendered as `BIGINT` with a
/// `REFERENCES "<target>"("id")` suffix appended via `.extra()`. The
/// target table name comes from `col.fk_target`.
fn build_column_def_sqlite(col: &Column) -> sea_query::ColumnDef {
    use sea_query::{Alias, ColumnDef, ColumnType};

    // ForeignKey gets a special path: BIGINT + inline REFERENCES clause.
    if matches!(col.ty, SqlType::ForeignKey) {
        let fk_target = col
            .fk_target
            .as_deref()
            .unwrap_or("_unknown_")
            .replace('"', "\"\"");
        let mut def = ColumnDef::new_with_type(Alias::new(&col.name), ColumnType::BigInteger);
        if !col.nullable {
            def.not_null();
        }
        def.extra(format!("REFERENCES \"{fk_target}\"(\"id\")"));
        return def;
    }

    let is_int_pk = col.primary_key && matches!(col.ty, SqlType::Integer | SqlType::BigInt);

    let column_type = if is_int_pk {
        ColumnType::Integer
    } else {
        crate::backend::SqliteBackend.map_type(col.ty)
    };

    let mut def = ColumnDef::new_with_type(Alias::new(&col.name), column_type);
    if !col.nullable {
        def.not_null();
    }
    if col.primary_key {
        def.primary_key();
        if is_int_pk {
            def.auto_increment();
        }
    }
    def
}

/// Build a Postgres `ColumnDef`. Integer primary keys use the
/// standard `auto_increment()` flag — sea-query's `PostgresQueryBuilder`
/// lowers that to `BIGSERIAL` for `BigInt` and `SERIAL` for `Integer`.
/// No SQLite-style INTEGER-type override needed; Postgres has proper
/// `BIGSERIAL` / identity columns and respects the declared width.
///
/// For `SqlType::ForeignKey` columns: rendered as `BIGINT` with a
/// `REFERENCES "<target>"("id")` suffix. The target table name comes
/// from `col.fk_target`.
fn build_column_def_postgres(col: &Column) -> sea_query::ColumnDef {
    use sea_query::{Alias, ColumnDef};

    // ForeignKey gets a special path: BIGINT + inline REFERENCES clause.
    if matches!(col.ty, SqlType::ForeignKey) {
        let fk_target = col
            .fk_target
            .as_deref()
            .unwrap_or("_unknown_")
            .replace('"', "\"\"");
        let mut def = ColumnDef::new_with_type(
            Alias::new(&col.name),
            crate::backend::PostgresBackend.map_type(SqlType::BigInt),
        );
        if !col.nullable {
            def.not_null();
        }
        def.extra(format!("REFERENCES \"{fk_target}\"(\"id\")"));
        return def;
    }

    let column_type = crate::backend::PostgresBackend.map_type(col.ty);

    let mut def = ColumnDef::new_with_type(Alias::new(&col.name), column_type);
    if !col.nullable {
        def.not_null();
    }
    if col.primary_key {
        def.primary_key();
        if matches!(
            col.ty,
            SqlType::Integer | SqlType::BigInt | SqlType::SmallInt
        ) {
            def.auto_increment();
        }
    }
    def
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M8 — `plugin_order()` falls back to `registered_plugins()` when
    /// no topological order has been published. The fallback keeps the
    /// engine usable from low-level paths that drive `init_plugins`
    /// directly (the M5 / M6 tests that pre-date phase 1.5 of
    /// `App::build()`).
    ///
    /// Runs in the lib's unit-test binary, which is wholly separate
    /// from the integration test binaries and so owns its own copies
    /// of `REGISTRY` and `PLUGIN_ORDER`. This test seeds `REGISTRY` via
    /// `init_plugins`, never touches `init_plugin_order`, and pins the
    /// fallback to the sorted-by-name `registered_plugins()` output.
    /// As the only test that touches either OnceLock in this binary,
    /// it has them to itself.
    #[test]
    fn plugin_order_falls_back_to_registered_plugins_when_unpublished() {
        let mut per_plugin: std::collections::HashMap<String, Vec<ModelMeta>> =
            std::collections::HashMap::new();
        per_plugin.insert(
            "zeta".to_string(),
            vec![ModelMeta {
                name: "ZetaModel".to_string(),
                table: "zeta".to_string(),
                fields: Vec::new(),
            }],
        );
        per_plugin.insert(
            "alpha".to_string(),
            vec![ModelMeta {
                name: "AlphaModel".to_string(),
                table: "alpha".to_string(),
                fields: Vec::new(),
            }],
        );
        init_plugins(per_plugin);

        // `init_plugin_order` was never called, so `plugin_order` must
        // return the sorted-by-name fallback.
        let order = plugin_order();
        assert_eq!(
            order,
            vec!["alpha".to_string(), "zeta".to_string()],
            "fallback should sort by name; got {order:?}",
        );
        assert_eq!(
            order,
            registered_plugins(),
            "fallback should exactly equal registered_plugins()",
        );
    }
}
