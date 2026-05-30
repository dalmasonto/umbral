//! `inspectdb` — introspect an existing database into umbra models.
//!
//! The porting payoff. A team running Django or anything else with a
//! SQLite database points `inspectdb` at it and gets a `models.rs`
//! with `#[derive(Model)]` structs plus a `0001_initial.json`
//! migration carrying one `CreateTable` op per table. The migration
//! is recorded as applied in `umbra_migrations` so the next `migrate`
//! is a no-op until the user actually changes a model.
//!
//! After that, the introspected schema enters the M5 declare →
//! migrate → change → migrate loop with no separate code path.
//!
//! ## M6 v1 scope
//!
//! - **Backend.** SQLite only. The introspection uses `PRAGMA
//!   table_info`. Postgres lands when the M4 [`DatabaseBackend`]
//!   abstraction grows an `introspect` hook.
//! - **Output.** A flat `models.rs` plus `migrations/0001_initial.json`
//!   in the user-chosen output directory. No `Cargo.toml`, no `lib.rs`
//!   with a `Plugin` impl: the plugin trait isn't shipped until M7,
//!   so M6 v1 leaves the wiring (one `mod models;` plus one
//!   `.model::<T>()` per generated struct) to the user. M7 turns the
//!   output into a self-contained plugin crate.
//! - **Type mapping.** Covers the M5 [`SqlType`] catalogue
//!   (integers, floats, bool, text, date / time / timestamptz, uuid)
//!   plus their nullable variants. Anything else (NUMERIC, JSON,
//!   BYTEA, arrays, custom types) returns
//!   [`InspectError::UnsupportedColumnType`] with the table / column
//!   names; the user fixes by-hand or waits for the field-type
//!   catalogue to grow.
//! - **FKs and indexes.** Not yet read out. The CreateTable op carries
//!   columns only; FK / index detection lands with the field-level
//!   support in [`crate::orm`].
//!
//! See [`docs/specs/07-inspectdb.md`] for the eventual target shape
//! and the deferred items.
//!
//! [`DatabaseBackend`]: crate::backend::DatabaseBackend
//! [`SqlType`]: crate::orm::SqlType

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::migrate::{self, Column, MigrationFile, ModelMeta, Operation, Snapshot};
use crate::orm::SqlType;

/// Default plugin name the generated migration is filed under. Matches
/// [`crate::migrate::APP_PLUGIN_NAME`] so the produced
/// `0001_initial.json` lands inside the same `migrations/app/`
/// directory the M5 engine reads from. M7 lifts this once the user can
/// choose a real plugin name via `--plugin`.
pub const INSPECTED_PLUGIN_NAME: &str = migrate::APP_PLUGIN_NAME;

/// Default filename for the introspected initial migration.
pub const INITIAL_MIGRATION_ID: &str = "0001_initial";

/// The introspection result. A flat list of tables, each with its
/// columns in declaration order. Indexes and foreign keys are omitted
/// at M6 v1 (the field types they target don't exist yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrospectedSchema {
    pub tables: Vec<IntrospectedTable>,
}

/// One introspected table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrospectedTable {
    /// The SQL table name as it appears in the database.
    pub table: String,
    /// The struct name the renderer will use. Defaults to the table
    /// name in UpperCamelCase; the M6 v1 importer does not strip
    /// prefixes (deferred to M7's `--strip-prefix` flag).
    pub name: String,
    /// One descriptor per column, in declaration order.
    pub columns: Vec<IntrospectedColumn>,
}

/// One introspected column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrospectedColumn {
    pub name: String,
    pub ty: SqlType,
    pub primary_key: bool,
    pub nullable: bool,
}

/// Errors `inspectdb` can produce. Carries enough detail for the CLI
/// to print a single-line diagnostic with the offending table and
/// column.
#[derive(Debug)]
pub enum InspectError {
    /// IO error reading or writing a generated file.
    Io(std::io::Error),
    /// sqlx error executing the introspection queries.
    Sqlx(sqlx::Error),
    /// The introspection ran but found no tables. Surfaced so the CLI
    /// can print "nothing to import" instead of writing empty files.
    NoTables,
    /// A column's SQL type isn't in the M6 v1 mapping table. Holds the
    /// table / column / raw SQL type so the user can decide whether to
    /// add a field type, edit the generated code, or wait for the
    /// catalogue to grow.
    UnsupportedColumnType {
        table: String,
        column: String,
        sql_type: String,
    },
    /// Pass-through for migration-engine failures (e.g. recording the
    /// initial migration as applied).
    Migrate(migrate::MigrateError),
}

impl std::fmt::Display for InspectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InspectError::Io(e) => write!(f, "umbra inspectdb: io: {e}"),
            InspectError::Sqlx(e) => write!(f, "umbra inspectdb: sqlx: {e}"),
            InspectError::NoTables => write!(
                f,
                "umbra inspectdb: no tables found in the database (nothing to import)"
            ),
            InspectError::UnsupportedColumnType {
                table,
                column,
                sql_type,
            } => write!(
                f,
                "umbra inspectdb: column `{table}.{column}` has unsupported SQL type `{sql_type}`; \
                 add a matching SqlType variant or edit the generated model by hand"
            ),
            InspectError::Migrate(e) => write!(f, "umbra inspectdb: migrate: {e}"),
        }
    }
}

impl std::error::Error for InspectError {}

impl From<std::io::Error> for InspectError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<sqlx::Error> for InspectError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl From<migrate::MigrateError> for InspectError {
    fn from(e: migrate::MigrateError) -> Self {
        Self::Migrate(e)
    }
}

/// CLI-driven options. The CLI subcommand wires its flags into this
/// struct and hands it to [`inspectdb`].
#[derive(Debug, Clone)]
pub struct InspectOptions {
    /// Directory the generated files are written under. `models.rs`
    /// lands at the root; the migration lands at
    /// `<output>/migrations/<INSPECTED_PLUGIN_NAME>/0001_initial.json`.
    pub output: PathBuf,
    /// Mark `0001_initial` as applied in `umbra_migrations` after
    /// writing it. The right default when the target database already
    /// has tables (running the migration would fail). Off for empty
    /// databases.
    pub mark_applied: bool,
}

/// Summary returned to the CLI. Counts that the caller can render as a
/// one-line "imported N tables / M columns" message.
#[derive(Debug, Clone, Default)]
pub struct InspectReport {
    pub tables: usize,
    pub columns: usize,
    pub models_path: PathBuf,
    pub migration_path: PathBuf,
}

// =========================================================================
// Top-level entry points. Bodies filled in by the M6 fan-out subagents.
// =========================================================================

/// Run the full `inspectdb` pipeline against the ambient SQLite pool:
/// introspect, render `models.rs`, render `0001_initial.json`, write
/// both to `opts.output`, and optionally mark applied.
pub async fn inspectdb(opts: InspectOptions) -> Result<InspectReport, InspectError> {
    let pool = crate::db::pool();
    let schema = introspect_pool(&pool).await?;
    if schema.tables.is_empty() {
        return Err(InspectError::NoTables);
    }

    let models_src = render_models(&schema);
    let migration = render_initial_migration(&schema);
    let report = write_outputs(&opts.output, &models_src, &migration).await?;

    if opts.mark_applied {
        let hash = migration.snapshot_after.hash();
        migrate::record_applied(&migration.plugin, &migration.id, &hash).await?;
    }

    Ok(report)
}

/// Introspect the schema reachable through the given SQLite pool.
/// Reads `sqlite_master` for table names and `PRAGMA table_info(...)`
/// for column descriptors. Skips internal tables (`sqlite_*`,
/// `umbra_migrations`).
///
/// Filled in by subagent A.
pub async fn introspect_pool(_pool: &SqlitePool) -> Result<IntrospectedSchema, InspectError> {
    Ok(IntrospectedSchema { tables: Vec::new() })
}

/// Render the introspected schema as the contents of a `models.rs`
/// file. The output is one `#[derive(Model)]` struct per table, with
/// fields in declaration order and the `#[umbra(table = "…")]`
/// attribute set when the struct name differs from the SQL table.
///
/// Filled in by subagent B.
pub fn render_models(_schema: &IntrospectedSchema) -> String {
    String::new()
}

/// Render the introspected schema as a [`MigrationFile`] suitable for
/// writing to `migrations/<INSPECTED_PLUGIN_NAME>/0001_initial.json`.
/// One `CreateTable` per introspected table; `snapshot_after` captures
/// the imported state so subsequent `make_in` runs diff against it.
///
/// Filled in by subagent B.
pub fn render_initial_migration(schema: &IntrospectedSchema) -> MigrationFile {
    let mut models: Vec<ModelMeta> = schema
        .tables
        .iter()
        .map(|t| ModelMeta {
            name: t.name.clone(),
            table: t.table.clone(),
            fields: t.columns.iter().map(Column::from).collect(),
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));

    let operations = schema
        .tables
        .iter()
        .map(|t| Operation::CreateTable {
            table: t.table.clone(),
            columns: t.columns.iter().map(Column::from).collect(),
        })
        .collect();

    MigrationFile {
        id: INITIAL_MIGRATION_ID.to_string(),
        plugin: INSPECTED_PLUGIN_NAME.to_string(),
        depends_on: Vec::new(),
        operations,
        snapshot_after: Snapshot { models },
    }
}

/// Write `models.rs` and the initial migration to `output`. Creates
/// `output/` and `output/migrations/<INSPECTED_PLUGIN_NAME>/` as
/// needed. Returns the report carrying the table / column counts and
/// the paths.
///
/// Filled in by subagent B.
pub async fn write_outputs(
    _output: &Path,
    _models_src: &str,
    _migration: &MigrationFile,
) -> Result<InspectReport, InspectError> {
    Ok(InspectReport::default())
}

// =========================================================================
// Internal helpers.
// =========================================================================

impl From<&IntrospectedColumn> for Column {
    fn from(c: &IntrospectedColumn) -> Self {
        Self {
            name: c.name.clone(),
            ty: c.ty,
            primary_key: c.primary_key,
            nullable: c.nullable,
        }
    }
}
