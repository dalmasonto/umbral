//! `inspectdb` — introspect an existing database into umbral models.
//!
//! The porting payoff. A team with an existing
//! SQLite database points `inspectdb` at it and gets a `models.rs`
//! with `#[derive(Model)]` structs plus a `0001_initial.json`
//! migration carrying one `CreateTable` op per table. The migration
//! is recorded as applied in `umbral_migrations` so the next `migrate`
//! is a no-op until the user actually changes a model.
//!
//! After that, the introspected schema enters the M5 declare →
//! migrate → change → migrate loop with no separate code path.
//!
//! ## Backend coverage
//!
//! - **SQLite (M6 v1).** [`introspect_pool`] reads `sqlite_master` for
//!   table names and `PRAGMA table_info` for column descriptors.
//! - **Postgres (Phase 3 of the rollout).** [`introspect_pool_pg`]
//!   reads `information_schema.tables` / `information_schema.columns`
//!   and joins `information_schema.table_constraints` + `key_column_usage`
//!   for primary keys. Same `IntrospectedSchema` output; the
//!   downstream pipeline (`render_models` / `render_initial_migration`
//!   / `write_outputs`) is backend-agnostic.
//!
//! ## M6 v1 scope
//!
//! - **Output.** A flat `models.rs` plus `migrations/0001_initial.json`
//!   in the user-chosen output directory. No `Cargo.toml`, no `lib.rs`
//!   with a `Plugin` impl: the plugin trait isn't shipped until M7,
//!   so M6 v1 leaves the wiring (one `mod models;` plus one
//!   `.model::<T>()` per generated struct) to the user. M7 turns the
//!   output into a self-contained plugin crate.
//! - **Type mapping.** Covers the [`SqlType`] catalogue: integers
//!   (including `unsigned` variants from Django's PositiveIntegerField
//!   family), floats, bool, text, date / time / timestamptz, uuid, json,
//!   bytea, and numeric / decimal — plus their nullable variants.
//!   Decimal maps faithfully to `rust_decimal::Decimal` even from a
//!   SQLite source (it is Postgres-only at runtime, so the boot system
//!   check surfaces that when the model targets SQLite). Anything still
//!   off-catalogue (arrays, custom types) returns
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

use sqlx::{PgPool, Row, SqlitePool};
use umbral_casing::{pascal_case_from_table, to_snake_case};

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
    /// Multi-column UNIQUE constraints / unique indexes, each a column-name
    /// group. Rendered as `#[umbral(unique_together = [[...]])]`. Single-column
    /// uniques live on the column's `unique` flag instead.
    pub unique_together: Vec<Vec<String>>,
    /// Multi-column (non-unique) indexes, each a column-name group. Rendered as
    /// `#[umbral(indexes = [[...]])]`. Single-column indexes use the column's
    /// `index` flag.
    pub indexes: Vec<Vec<String>>,
    /// Many-to-many relations this table OWNS — recovered by folding a Django
    /// join table (`communities_community_software`) into an `M2M<T>` field on
    /// the owner (`Community.software`). The join table itself is removed from
    /// the schema; umbral auto-generates its own junction. See
    /// [`detect_m2m_relations`].
    pub m2m: Vec<IntrospectedM2M>,
}

/// One recovered many-to-many relation, folded from a Django join table onto
/// the owning model as an `M2M<Target>` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrospectedM2M {
    /// The Rust field name (`software`), from the join table's suffix after the
    /// owner table name.
    pub field_name: String,
    /// The target model's SQL table (`software`).
    pub target_table: String,
    /// The target model's resolved struct name (`Software`).
    pub target_name: String,
}

/// One introspected column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrospectedColumn {
    pub name: String,
    pub ty: SqlType,
    pub primary_key: bool,
    pub nullable: bool,
    /// The referenced table when this column is a foreign key, else `None`.
    /// Drives rendering the field as `ForeignKey<Target>` rather than a bare
    /// integer, and populates `Column::fk_target` in the initial migration.
    pub fk_target: Option<String>,
    /// A single-column UNIQUE constraint / unique index covers this column.
    pub unique: bool,
    /// A single-column (non-unique) index covers this column — rendered as
    /// `#[umbral(index)]`.
    pub index: bool,
    /// The recovered constant DB default (`'active'`, `0`, `true`), cleaned of
    /// Postgres `::type` casts and surrounding quotes. `None` when the column
    /// has no default, or one umbral can't represent as a `#[umbral(default)]`
    /// literal — a sequence (`nextval(...)`) or function call. A
    /// `CURRENT_TIMESTAMP` / `now()` default on a temporal column is lifted to
    /// `auto_now_add` instead of landing here.
    pub default: Option<String>,
    /// The column is populated with the current time on INSERT — recovered from
    /// a `CURRENT_TIMESTAMP` / `now()` default on a temporal column, or (under
    /// `--framework django`) a `created*`-named timestamp. Renders
    /// `#[umbral(auto_now_add)]`.
    pub auto_now_add: bool,
    /// The column is refreshed to the current time on every write. Not
    /// expressible as DB metadata on Postgres/SQLite (Django sets it in Python),
    /// so recovered only by the `--framework django` name heuristic
    /// (`updated*` / `modified*`). Renders `#[umbral(auto_now)]`.
    pub auto_now: bool,
}

/// Errors `inspectdb` can produce. Carries enough detail for the CLI
/// to print a single-line diagnostic with the offending table and
/// column.
#[derive(Debug)]
pub enum InspectError {
    /// IO error reading or writing a generated file.
    Io(std::io::Error),
    /// JSON serialisation error pretty-printing the generated migration.
    Json(serde_json::Error),
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
            InspectError::Io(e) => write!(f, "umbral inspectdb: io: {e}"),
            InspectError::Json(e) => write!(f, "umbral inspectdb: json: {e}"),
            InspectError::Sqlx(e) => write!(f, "umbral inspectdb: sqlx: {e}"),
            InspectError::NoTables => write!(
                f,
                "umbral inspectdb: no tables found in the database (nothing to import)"
            ),
            InspectError::UnsupportedColumnType {
                table,
                column,
                sql_type,
            } => write!(
                f,
                "umbral inspectdb: column `{table}.{column}` has unsupported SQL type `{sql_type}`; \
                 add a matching SqlType variant or edit the generated model by hand"
            ),
            InspectError::Migrate(e) => write!(f, "umbral inspectdb: migrate: {e}"),
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

impl From<serde_json::Error> for InspectError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<migrate::MigrateError> for InspectError {
    fn from(e: migrate::MigrateError) -> Self {
        Self::Migrate(e)
    }
}

/// CLI-driven options. The CLI subcommand wires its flags into this
/// struct and hands it to [`inspectdb`].
/// A source ORM/framework whose naming conventions `inspectdb` can undo to
/// produce idiomatic umbral models. Currently only Django, the porting test
/// ground. `None` keeps the raw database names verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    /// Django: a foreign-key column is `<field>_id`. Strip the `_id` (and a
    /// leading `<app>_` prefix when the leading segment is a detected app
    /// label) so the field is the clean `<field>`, bound to the real column via
    /// `#[sqlx(rename = "<field>_id")]`.
    Django,
}

impl Framework {
    /// Parse a `--framework` value (case-insensitive). Returns `None` for an
    /// unknown name so the caller can report it.
    pub fn parse(s: &str) -> Option<Framework> {
        match s.trim().to_ascii_lowercase().as_str() {
            "django" => Some(Framework::Django),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InspectOptions {
    /// The source database connection URL to introspect. `None` means "use the
    /// app's ambient pool" (the historical behaviour); `Some(url)` opens a
    /// dedicated connection to that database instead, so `umbral inspectdb
    /// <db>` can onboard a foreign schema without repointing the whole app.
    pub source: Option<String>,
    /// The source framework whose conventions to undo (`--framework django`).
    /// `None` keeps raw column names.
    pub framework: Option<Framework>,
    /// Strip the framework app-prefix off **struct names** (`blog_post` ->
    /// `Post`) and preserve the real table with a `#[umbral(table = "...")]`
    /// macro (`--with-table-names`). Off by default: struct names stay full
    /// (`BlogPost`) and round-trip to their table, so no table macro is emitted.
    /// A struct name that still wouldn't round-trip (odd casing) gets its table
    /// macro regardless, so generated models always map to the right table.
    pub with_table_names: bool,
    /// Directory the generated files are written under. `models.rs`
    /// lands at the root; the migration lands at
    /// `<output>/migrations/<INSPECTED_PLUGIN_NAME>/0001_initial.json`.
    pub output: PathBuf,
    /// Mark `0001_initial` as applied in `umbral_migrations` after
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

/// Run the full `inspectdb` pipeline against the ambient pool:
/// introspect (dispatching on the active backend), render `models.rs`,
/// render `0001_initial.json`, write both to `opts.output`, and
/// optionally mark applied.
///
/// Phase 3 of the Postgres rollout taught this entry point to dispatch
/// on `DbPool` — the SQLite path uses `PRAGMA table_info`; the
/// Postgres path uses `information_schema`. The downstream pipeline
/// (rendering + writing) is backend-agnostic and runs the same way.
pub async fn inspectdb(opts: InspectOptions) -> Result<InspectReport, InspectError> {
    // A `--source` URL opens its own connection so a foreign database can be
    // onboarded without repointing the whole app; otherwise introspect the
    // ambient pool the app already booted with.
    let schema = match &opts.source {
        Some(url) => match crate::db::connect(url).await? {
            crate::db::DbPool::Sqlite(pool) => introspect_pool(&pool).await?,
            crate::db::DbPool::Postgres(pool) => introspect_pool_pg(&pool).await?,
        },
        None => match crate::db::pool_dispatched() {
            crate::db::DbPool::Sqlite(pool) => introspect_pool(pool).await?,
            crate::db::DbPool::Postgres(pool) => introspect_pool_pg(pool).await?,
        },
    };
    if schema.tables.is_empty() {
        return Err(InspectError::NoTables);
    }
    // Lift recovered DB defaults + framework naming into umbral's semantic field
    // attributes (auto_now_add / auto_now / default) so BOTH the model and the
    // initial migration render them consistently.
    let mut schema = schema;
    apply_recovered_conventions(&mut schema, opts.framework);
    // Django names FK columns `<field>_id`; umbral names them `<field>` (the
    // field IS the column). Since inspectdb writes a fresh schema, shed the
    // suffix so a FK reads `pub author: ForeignKey<Author>` / `post.author`,
    // matching how umbral models are written.
    if opts.framework == Some(Framework::Django) {
        strip_django_fk_id_suffix(&mut schema);
    }
    // Fold Django M2M join tables into `M2M<T>` fields on their owner (and drop
    // the join table — umbral auto-generates its own junction).
    detect_m2m_relations(&mut schema, opts.framework, opts.with_table_names);

    let models_src = render_models_with(&schema, opts.framework, opts.with_table_names);
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
/// `umbral_migrations`).
pub async fn introspect_pool(pool: &SqlitePool) -> Result<IntrospectedSchema, InspectError> {
    // List user tables in lexical name order. `sqlite_master` carries
    // both tables and indexes; the `type = 'table'` predicate scopes the
    // result to tables. The skip-list takes out SQLite's internal
    // bookkeeping (`sqlite_%`) and umbral's own tracking table, which
    // would otherwise loop back through the migration engine.
    let table_rows = sqlx::query(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' \
           AND name NOT LIKE 'sqlite_%' \
           AND name <> 'umbral_migrations' \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await?;

    let mut tables: Vec<IntrospectedTable> = Vec::with_capacity(table_rows.len());
    for row in table_rows {
        let table: String = row.try_get("name")?;
        let columns = introspect_columns(pool, &table).await?;
        let (unique_together, indexes) = sqlite_composite_indexes(pool, &table).await?;
        tables.push(IntrospectedTable {
            name: pascal_case_from_table(&table),
            table,
            columns,
            unique_together,
            indexes,
            m2m: Vec::new(),
        });
    }

    Ok(IntrospectedSchema { tables })
}

/// Introspect the schema reachable through the given Postgres pool.
/// Reads `information_schema.tables` for table names,
/// `information_schema.columns` for column descriptors, and joins
/// `information_schema.table_constraints` + `key_column_usage` for
/// the primary-key flag. Scoped to the `public` schema by default;
/// internal Postgres schemas and umbral's own `umbral_migrations`
/// tracking table are skipped.
///
/// The output is the same `IntrospectedSchema` the SQLite path
/// produces — downstream rendering doesn't know which backend the
/// data came from.
pub async fn introspect_pool_pg(pool: &PgPool) -> Result<IntrospectedSchema, InspectError> {
    // List user tables in the `public` schema, lexically. Postgres
    // information_schema is standard SQL; pg_catalog is the lower-
    // level surface but information_schema is portable across
    // Postgres-compatible servers and carries everything the
    // SqlType catalogue needs.
    let table_rows: Vec<(String,)> = sqlx::query_as(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' \
           AND table_type = 'BASE TABLE' \
           AND table_name <> 'umbral_migrations' \
         ORDER BY table_name",
    )
    .fetch_all(pool)
    .await?;

    let mut tables: Vec<IntrospectedTable> = Vec::with_capacity(table_rows.len());
    for (table,) in table_rows {
        let columns = introspect_columns_pg(pool, &table).await?;
        let (unique_together, indexes) = pg_composite_indexes(pool, &table).await;
        tables.push(IntrospectedTable {
            name: pascal_case_from_table(&table),
            table,
            columns,
            unique_together,
            indexes,
            m2m: Vec::new(),
        });
    }

    Ok(IntrospectedSchema { tables })
}

/// Read one Postgres table's columns via `information_schema.columns`,
/// plus a primary-key join over `information_schema.table_constraints`
/// and `key_column_usage`. Columns come back in declaration order
/// (`ordinal_position`).
///
/// `data_type` is the normalised type string Postgres exposes through
/// information_schema (e.g. `"integer"`, `"character varying"`,
/// `"timestamp with time zone"`); [`map_postgres_type`] maps it to the
/// umbral `SqlType` catalogue. Anything unmapped surfaces as
/// [`InspectError::UnsupportedColumnType`] with the table / column
/// names and the raw type string.
async fn introspect_columns_pg(
    pool: &PgPool,
    table: &str,
) -> Result<Vec<IntrospectedColumn>, InspectError> {
    // The primary-key lookup runs once per table. The set is typically
    // tiny (one column for most tables, a handful for composite keys)
    // so collecting it up-front into a Vec keeps the inner column loop
    // O(columns × pk_columns) without an extra round trip per column.
    let pk_rows: Vec<(String,)> = sqlx::query_as(
        "SELECT kcu.column_name \
         FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage kcu \
           ON tc.constraint_name = kcu.constraint_name \
          AND tc.table_schema = kcu.table_schema \
         WHERE tc.constraint_type = 'PRIMARY KEY' \
           AND tc.table_schema = 'public' \
           AND tc.table_name = $1",
    )
    .bind(table)
    .fetch_all(pool)
    .await?;
    let pk_columns: std::collections::HashSet<String> = pk_rows.into_iter().map(|(c,)| c).collect();

    // `udt_name` carries the underlying type name even when `data_type`
    // is the abstract `"ARRAY"` placeholder. For `bigint[]` the
    // information_schema reports data_type = "ARRAY" and udt_name =
    // "_int8" (underscore prefix marks the array variant in pg_type).
    // For non-array columns udt_name carries the same physical name
    // (`int8`, `text`, etc.) but `data_type` is the canonical match
    // key we already lookup against.
    let column_rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT column_name, data_type, is_nullable, udt_name, column_default \
         FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = $1 \
         ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(pool)
    .await?;

    // Foreign keys: information_schema referential integrity views map a FK
    // column to its referenced table. One row per FK column.
    let fk_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT kcu.column_name, ccu.table_name AS foreign_table \
         FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage kcu \
           ON tc.constraint_name = kcu.constraint_name \
          AND tc.table_schema = kcu.table_schema \
         JOIN information_schema.constraint_column_usage ccu \
           ON ccu.constraint_name = tc.constraint_name \
          AND ccu.table_schema = tc.table_schema \
         WHERE tc.constraint_type = 'FOREIGN KEY' \
           AND tc.table_schema = 'public' \
           AND tc.table_name = $1",
    )
    .bind(table)
    .fetch_all(pool)
    .await?;
    let fk_map: std::collections::HashMap<String, String> = fk_rows.into_iter().collect();

    // Single-column unique / index coverage from pg_index. `indisunique` marks
    // a unique index; `indisprimary` PK indexes are excluded (the column is
    // already the PK). Only single-column (`array_length(indkey) = 1`) indexes
    // map to a per-column flag.
    let idx_rows: Vec<(String, bool)> = sqlx::query_as(
        "SELECT a.attname, ix.indisunique \
         FROM pg_index ix \
         JOIN pg_class t ON t.oid = ix.indrelid \
         JOIN pg_namespace n ON n.oid = t.relnamespace \
         JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = ix.indkey[0] \
         WHERE n.nspname = 'public' AND t.relname = $1 \
           AND ix.indnatts = 1 AND NOT ix.indisprimary",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut unique_cols = std::collections::HashSet::new();
    let mut index_cols = std::collections::HashSet::new();
    for (col, is_unique) in idx_rows {
        if is_unique {
            unique_cols.insert(col);
        } else {
            index_cols.insert(col);
        }
    }

    // PostGIS spatial columns report `data_type = "USER-DEFINED"` with
    // `udt_name = "geometry"` / `"geography"`; the real subtype + SRID live in
    // the `geometry_columns` / `geography_columns` catalog views. Look them up
    // once per table so a geometry column recovers `geometry(Point, 4326)`
    // rather than crashing as an unsupported type.
    let spatial = pg_spatial_columns(pool, table).await;

    let mut columns: Vec<IntrospectedColumn> = Vec::with_capacity(column_rows.len());
    for (name, data_type, is_nullable, udt_name, raw_default) in column_rows {
        let ty = if udt_name.eq_ignore_ascii_case("geometry")
            || udt_name.eq_ignore_ascii_case("geography")
        {
            // Prefer the catalog's exact subtype+SRID; fall back to the
            // unconstrained base type when the column isn't registered there.
            spatial.get(&name).copied().unwrap_or_else(|| {
                let spec = crate::orm::GeometrySpec::DEFAULT;
                if udt_name.eq_ignore_ascii_case("geography") {
                    SqlType::Geography(spec)
                } else {
                    SqlType::Geometry(spec)
                }
            })
        } else if data_type.eq_ignore_ascii_case("ARRAY") {
            // Element type comes from udt_name with the leading
            // underscore stripped. `_int8` -> int8 -> ArrayElement::BigInt.
            let elem_name = udt_name.strip_prefix('_').unwrap_or(udt_name.as_str());
            map_postgres_array_element(elem_name).ok_or_else(|| {
                InspectError::UnsupportedColumnType {
                    table: table.to_string(),
                    column: name.clone(),
                    sql_type: format!("ARRAY of {elem_name}"),
                }
            })?
        } else {
            map_postgres_type(&data_type).ok_or_else(|| InspectError::UnsupportedColumnType {
                table: table.to_string(),
                column: name.clone(),
                sql_type: data_type.clone(),
            })?
        };
        // A FK column renders as `ForeignKey<Target>`; the referenced table is
        // what makes it one, regardless of the stored integer type.
        let fk_target = fk_map.get(&name).cloned();
        let ty = if fk_target.is_some() {
            SqlType::ForeignKey
        } else {
            ty
        };
        let primary_key = pk_columns.contains(&name);
        // Postgres `is_nullable` is the string "YES" or "NO". A primary
        // key is non-nullable by definition (the server enforces it);
        // we force `nullable = false` so a SERIAL/BIGSERIAL PK round-
        // trips through the M3 derive (which rejects `Option<T>` PKs)
        // matching the behavioural fix already in place on the SQLite
        // path.
        let nullable = if primary_key {
            false
        } else {
            is_nullable.eq_ignore_ascii_case("YES")
        };
        let unique = !primary_key && unique_cols.contains(&name);
        let index = !primary_key && !unique && index_cols.contains(&name);
        columns.push(IntrospectedColumn {
            name,
            ty,
            primary_key,
            nullable,
            fk_target,
            unique,
            index,
            // Raw recovered default; semantic lift happens in
            // `apply_recovered_conventions`, shared with the SQLite path.
            default: raw_default,
            auto_now_add: false,
            auto_now: false,
        });
    }

    Ok(columns)
}

/// Map a Postgres array's element-type name (from `udt_name` with the
/// leading underscore stripped) to a [`SqlType::Array`] variant.
///
/// The `udt_name` column on `information_schema.columns` carries the
/// physical type name from `pg_catalog.pg_type`; array variants are
/// prefixed with `_` (`_int8` for `bigint[]`, `_text` for `text[]`).
/// The caller strips the prefix; this function maps the remaining
/// stem to the umbral `ArrayElement` catalogue.
///
/// Returns `None` if the element type isn't in
/// `umbral::orm::ArrayElement` — chrono types, JSON, network types,
/// and Postgres-specific types like NUMERIC fall outside Phase 4.1's
/// array catalogue.
/// Return `(unique_together, indexes)` — the MULTI-column index groups for a
/// Postgres table, columns in index order. A composite unique index →
/// `unique_together`; a composite plain index → `indexes`. Primary-key and
/// single-column indexes are excluded (the latter ride the per-column flags).
/// Expression indexes (a column position isn't a plain attribute) are skipped.
async fn pg_composite_indexes(pool: &PgPool, table: &str) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    // `indkey` is an int2vector of attribute numbers in index order; unnest it
    // WITH ORDINALITY to preserve that order, then resolve each to its column
    // name. `a.attnum > 0` drops system/expression positions.
    let rows: Vec<(bool, Vec<String>)> = sqlx::query_as(
        "SELECT ix.indisunique, array_agg(a.attname ORDER BY k.ord) AS cols \
         FROM pg_index ix \
         JOIN pg_class t ON t.oid = ix.indrelid \
         JOIN pg_namespace n ON n.oid = t.relnamespace \
         JOIN unnest(string_to_array(ix.indkey::text, ' ')::smallint[]) \
              WITH ORDINALITY AS k(attnum, ord) ON true \
         JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum AND a.attnum > 0 \
         WHERE n.nspname = 'public' AND t.relname = $1 \
           AND ix.indnatts > 1 AND NOT ix.indisprimary \
         GROUP BY ix.indexrelid, ix.indisunique",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut uniques = Vec::new();
    let mut plains = Vec::new();
    for (is_unique, cols) in rows {
        if cols.len() < 2 {
            continue; // a partial/expression index — can't model it
        }
        if is_unique {
            uniques.push(cols);
        } else {
            plains.push(cols);
        }
    }
    (uniques, plains)
}

/// Read PostGIS's `geometry_columns` / `geography_columns` catalog views for
/// one table, mapping each spatial column to its exact `SqlType::Geometry` /
/// `Geography` with recovered subtype + SRID. Returns an empty map when the
/// views are absent (a non-PostGIS database) or a column isn't registered, so
/// the caller falls back to the unconstrained base type.
async fn pg_spatial_columns(
    pool: &PgPool,
    table: &str,
) -> std::collections::HashMap<String, SqlType> {
    use crate::orm::{GeometryKind, GeometrySpec};
    let mut map = std::collections::HashMap::new();

    // `geometry_columns.type` is the PostGIS subtype name ('POINT',
    // 'MULTIPOLYGON', 'GEOMETRY'); `GeometryKind::from_attr` folds case.
    let geom: Vec<(String, String, i32)> = sqlx::query_as(
        "SELECT f_geometry_column, type, srid FROM geometry_columns \
         WHERE f_table_schema = 'public' AND f_table_name = $1",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (col, kind, srid) in geom {
        let spec = GeometrySpec {
            kind: GeometryKind::from_attr(&kind).unwrap_or(GeometryKind::Geometry),
            srid,
        };
        map.insert(col, SqlType::Geometry(spec));
    }

    let geog: Vec<(String, String, i32)> = sqlx::query_as(
        "SELECT f_geography_column, type, srid FROM geography_columns \
         WHERE f_table_schema = 'public' AND f_table_name = $1",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (col, kind, srid) in geog {
        let spec = GeometrySpec {
            kind: GeometryKind::from_attr(&kind).unwrap_or(GeometryKind::Geometry),
            srid,
        };
        map.insert(col, SqlType::Geography(spec));
    }

    map
}

fn map_postgres_array_element(elem: &str) -> Option<SqlType> {
    use crate::orm::ArrayElement;
    let kind = match elem.trim().to_ascii_lowercase().as_str() {
        // Postgres physical type names (per pg_type.typname). The
        // information_schema strips spaces from the data_type alias
        // form, so we match the canonical lowercase names here.
        "int2" => ArrayElement::SmallInt,
        "int4" => ArrayElement::Integer,
        "int8" => ArrayElement::BigInt,
        "float4" => ArrayElement::Real,
        "float8" => ArrayElement::Double,
        "bool" => ArrayElement::Boolean,
        "text" | "varchar" | "bpchar" => ArrayElement::Text,
        "uuid" => ArrayElement::Uuid,
        _ => return None,
    };
    Some(SqlType::Array(kind))
}

/// Map a Postgres `information_schema.columns.data_type` value to the
/// umbral `SqlType` catalogue. Postgres normalises the strings, so the
/// match table is the canonical names rather than the optional aliases
/// `pg_type.typname` would expose. The inverse of
/// [`crate::backend::PostgresBackend::map_type`] — both stay in sync
/// as new `SqlType` variants land.
///
/// Returns `None` on anything not in the catalogue (Postgres-specific
/// types like `numeric`, `jsonb`, `bytea`, arrays, custom domains).
/// The caller turns that into `UnsupportedColumnType` with enough
/// context for the operator to fix by hand or wait for the field-
/// type catalogue to grow.
fn map_postgres_type(raw: &str) -> Option<SqlType> {
    let normalised = raw.trim().to_ascii_lowercase();
    match normalised.as_str() {
        "smallint" => Some(SqlType::SmallInt),
        "integer" => Some(SqlType::Integer),
        "bigint" => Some(SqlType::BigInt),
        "real" => Some(SqlType::Real),
        "double precision" => Some(SqlType::Double),
        "boolean" => Some(SqlType::Boolean),
        // information_schema reports `text`, `character varying`, and
        // `character` for VARCHAR / CHAR / TEXT. All round-trip through
        // umbral's Text variant.
        "text" | "character varying" | "character" => Some(SqlType::Text),
        "date" => Some(SqlType::Date),
        // Both timezone variants of TIME land on umbral's Time. The
        // distinction is preserved in the database; the client-side
        // type system doesn't model it yet.
        "time without time zone" | "time with time zone" => Some(SqlType::Time),
        // Likewise both timezone variants of TIMESTAMP land on
        // Timestamptz. The umbral catalogue picks the with-tz variant
        // as the default so chrono::DateTime<Utc> is the natural Rust
        // type for either.
        "timestamp without time zone" | "timestamp with time zone" => Some(SqlType::Timestamptz),
        "uuid" => Some(SqlType::Uuid),
        // Both `json` and `jsonb` round-trip to umbral's portable Json
        // variant. The DDL renderer chose `jsonb` on the way out; if a
        // pre-existing database stores values as `json` (the unindexed
        // text variant), inspectdb still recognises it on the way in.
        // A re-migrate would normalize to `jsonb` if the user re-creates
        // the column, which matches the M5 declare-and-migrate loop.
        "json" | "jsonb" => Some(SqlType::Json),
        // Phase 4.4: Postgres network address types.
        "inet" => Some(SqlType::Inet),
        "cidr" => Some(SqlType::Cidr),
        "macaddr" => Some(SqlType::MacAddr),
        // gaps2 #70: text-backed Postgres types. `bit varying` and bare
        // `bit` (the information_schema sometimes reports `bit` for a
        // BIT(n)) both round-trip to the `Bit` variant.
        "xml" => Some(SqlType::Xml),
        "ltree" => Some(SqlType::Ltree),
        "bit" | "bit varying" | "varbit" => Some(SqlType::Bit),
        "tsvector" => Some(SqlType::FullText),
        // Postgres reports both NUMERIC and DECIMAL as `numeric` in
        // information_schema.columns.data_type (precision/scale live in
        // separate columns, so no width string to strip). Maps to
        // umbral's Decimal, whose PG DDL renders back as `numeric(19,4)`.
        "numeric" | "decimal" => Some(SqlType::Decimal),
        "bytea" => Some(SqlType::Bytes),
        _ => None,
    }
}

/// Read one table's columns via `PRAGMA table_info`. The PRAGMA returns
/// `(cid, name, type, notnull, dflt_value, pk)` rows in declaration
/// order, sorted defensively by `cid` so a downstream change to the
/// PRAGMA's behaviour doesn't silently scramble field order.
async fn introspect_columns(
    pool: &SqlitePool,
    table: &str,
) -> Result<Vec<IntrospectedColumn>, InspectError> {
    // The PRAGMA name can't be bound as a parameter, but it also can't
    // contain user-supplied input here: `table` comes from `sqlite_master`
    // and matches an existing table identifier by construction.
    let quoted = table.replace('"', "\"\"");
    let sql = format!("PRAGMA table_info(\"{quoted}\")");
    let mut rows = sqlx::query(&sql).fetch_all(pool).await?;
    rows.sort_by_key(|r| r.try_get::<i64, _>("cid").unwrap_or(0));

    // Foreign keys: `PRAGMA foreign_key_list` gives one row per FK column with
    // its referenced `table`. Map from-column -> target table so a column that
    // is a FK renders as `ForeignKey<Target>` instead of a bare integer.
    let fk_map = sqlite_foreign_keys(pool, &quoted).await?;
    // Single-column unique / index coverage from `PRAGMA index_list` +
    // `index_info`. A one-column unique index -> the column is UNIQUE; a
    // one-column plain index -> `#[umbral(index)]`.
    let (unique_cols, index_cols) = sqlite_indexed_columns(pool, &quoted).await?;

    let mut columns: Vec<IntrospectedColumn> = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.try_get("name")?;
        let raw_type: String = row.try_get("type")?;
        let notnull: i64 = row.try_get("notnull")?;
        let pk: i64 = row.try_get("pk")?;
        // `dflt_value` is NULL when the column has no default; sqlx surfaces
        // that as `None`. Otherwise it's the raw default token verbatim
        // (`CURRENT_TIMESTAMP`, `0`, `'active'`).
        let raw_default: Option<String> = row.try_get("dflt_value").ok().flatten();
        // A FK column's declared type is whatever SQLite stored (usually
        // `integer`); the target table is what makes it a foreign key.
        let fk_target = fk_map.get(&name).cloned();
        let ty = if fk_target.is_some() {
            SqlType::ForeignKey
        } else {
            map_sqlite_type(&raw_type).ok_or_else(|| InspectError::UnsupportedColumnType {
                table: table.to_string(),
                column: name.clone(),
                sql_type: raw_type.clone(),
            })?
        };
        let primary_key = pk != 0;
        // SQLite's `PRAGMA table_info` reports `notnull = 0` for
        // `INTEGER PRIMARY KEY` columns because they're aliases for
        // ROWID (which SQLite manages internally). The columns are
        // nonetheless guaranteed non-null: SQLite refuses to insert
        // NULL into a primary key. Forcing `nullable = false` here
        // makes the generated `#[derive(Model)]` compile (the M3
        // derive's PK detection requires a non-`Option` PK field)
        // and matches what the database actually enforces.
        let nullable = if primary_key { false } else { notnull == 0 };
        // A PK column is already indexed/unique implicitly; don't re-emit.
        let unique = !primary_key && unique_cols.contains(&name);
        let index = !primary_key && !unique && index_cols.contains(&name);
        columns.push(IntrospectedColumn {
            name,
            ty,
            primary_key,
            nullable,
            fk_target,
            unique,
            index,
            // Raw recovered default; the semantic lift (CURRENT_TIMESTAMP ->
            // auto_now_add, unquote strings, drop expressions) happens in
            // `apply_recovered_conventions` so it's shared with the PG path.
            default: raw_default,
            auto_now_add: false,
            auto_now: false,
        });
    }
    Ok(columns)
}

/// Map each foreign-key column of `table` to its referenced table, via
/// `PRAGMA foreign_key_list`. Composite FKs (rare in ORM-generated schemas)
/// contribute each of their `from` columns pointing at the same target; umbral
/// models a FK as a single column, so the first mapping per column wins.
async fn sqlite_foreign_keys(
    pool: &SqlitePool,
    quoted_table: &str,
) -> Result<std::collections::HashMap<String, String>, InspectError> {
    let rows = sqlx::query(&format!("PRAGMA foreign_key_list(\"{quoted_table}\")"))
        .fetch_all(pool)
        .await?;
    let mut map = std::collections::HashMap::new();
    for row in rows {
        let from: String = row.try_get("from")?;
        let target: String = row.try_get("table")?;
        map.entry(from).or_insert(target);
    }
    Ok(map)
}

/// Return `(unique_columns, indexed_columns)` for `table`: the columns covered
/// by a **single-column** unique index and a single-column plain index
/// respectively. Multi-column indexes are skipped (umbral models per-column
/// `unique`/`index`; composite `unique_together` recovery is deferred).
/// SQLite's auto-index for a UNIQUE constraint (`origin = 'u'`) and an explicit
/// `CREATE UNIQUE INDEX` both surface here as `unique = 1`.
async fn sqlite_indexed_columns(
    pool: &SqlitePool,
    quoted_table: &str,
) -> Result<
    (
        std::collections::HashSet<String>,
        std::collections::HashSet<String>,
    ),
    InspectError,
> {
    let mut unique = std::collections::HashSet::new();
    let mut plain = std::collections::HashSet::new();
    let index_rows = sqlx::query(&format!("PRAGMA index_list(\"{quoted_table}\")"))
        .fetch_all(pool)
        .await?;
    for idx in index_rows {
        let index_name: String = idx.try_get("name")?;
        let is_unique: i64 = idx.try_get("unique")?;
        let cols = sqlx::query(&format!(
            "PRAGMA index_info(\"{}\")",
            index_name.replace('"', "\"\"")
        ))
        .fetch_all(pool)
        .await?;
        // Only single-column indexes map to a per-column flag.
        if cols.len() != 1 {
            continue;
        }
        let col: String = cols[0].try_get("name")?;
        if is_unique != 0 {
            unique.insert(col);
        } else {
            plain.insert(col);
        }
    }
    Ok((unique, plain))
}

/// Return `(unique_together, indexes)` — the MULTI-column index groups for
/// `table`. A composite unique index / UNIQUE constraint → `unique_together`; a
/// composite plain index → `indexes`. The PK's auto-index (`origin = 'pk'`) is
/// skipped (umbral has no composite PK), and single-column indexes are left to
/// [`sqlite_indexed_columns`]'s per-column flags.
async fn sqlite_composite_indexes(
    pool: &SqlitePool,
    table: &str,
) -> Result<(Vec<Vec<String>>, Vec<Vec<String>>), InspectError> {
    let quoted = table.replace('"', "\"\"");
    let mut uniques: Vec<Vec<String>> = Vec::new();
    let mut plains: Vec<Vec<String>> = Vec::new();
    let index_rows = sqlx::query(&format!("PRAGMA index_list(\"{quoted}\")"))
        .fetch_all(pool)
        .await?;
    for idx in index_rows {
        let index_name: String = idx.try_get("name")?;
        let is_unique: i64 = idx.try_get("unique")?;
        // `origin`: 'c' explicit CREATE INDEX, 'u' UNIQUE constraint, 'pk' the
        // primary-key auto-index (skip — not a user index).
        let origin: String = idx.try_get("origin").unwrap_or_default();
        if origin == "pk" {
            continue;
        }
        let cols_rows = sqlx::query(&format!(
            "PRAGMA index_info(\"{}\")",
            index_name.replace('"', "\"\"")
        ))
        .fetch_all(pool)
        .await?;
        if cols_rows.len() < 2 {
            continue; // single-column → handled per-column
        }
        let mut cols: Vec<(i64, String)> = Vec::new();
        for c in &cols_rows {
            cols.push((c.try_get("seqno")?, c.try_get("name")?));
        }
        cols.sort_by_key(|(seq, _)| *seq);
        let group: Vec<String> = cols.into_iter().map(|(_, n)| n).collect();
        if is_unique != 0 {
            uniques.push(group);
        } else {
            plains.push(group);
        }
    }
    Ok((uniques, plains))
}

/// Map a raw SQLite type string to the M6 v1 [`SqlType`] catalogue.
/// Case-insensitive; trailing `(n)` or `(p,s)` width parameters are
/// stripped before matching so `VARCHAR(255)` and `NUMERIC(10,2)` come
/// through as `varchar` and `numeric`. A trailing `unsigned` / `signed`
/// qualifier is also stripped, so Django's `integer unsigned`
/// (`PositiveIntegerField`) maps to the base signed type. Returns `None` on anything not
/// in the table; the caller turns that into
/// [`InspectError::UnsupportedColumnType`] with the table and column
/// names attached.
fn map_sqlite_type(raw: &str) -> Option<SqlType> {
    let head = match raw.split_once('(') {
        Some((before, _)) => before,
        None => raw,
    };
    let normalised = head.trim().to_ascii_lowercase();
    // Strip a trailing signedness qualifier: Django's PositiveIntegerField
    // family emits `smallint unsigned` / `integer unsigned` / `bigint
    // unsigned`, and MySQL-origin dumps can carry `int signed`. SQLite
    // ignores these for column affinity, and Django range-caps the value
    // to the signed max, so the base signed type is the faithful mapping.
    let base = normalised
        .strip_suffix(" unsigned")
        .or_else(|| normalised.strip_suffix(" signed"))
        .map(str::trim_end)
        .unwrap_or(normalised.as_str());
    match base {
        "smallint" | "int2" => Some(SqlType::SmallInt),
        "int" | "integer" | "int4" => Some(SqlType::Integer),
        "bigint" | "int8" => Some(SqlType::BigInt),
        "real" | "float" | "float4" => Some(SqlType::Real),
        "double" | "double precision" | "float8" => Some(SqlType::Double),
        "boolean" | "bool" => Some(SqlType::Boolean),
        "text" | "varchar" | "char" | "clob" | "character" | "varying character" | "nchar"
        | "nvarchar" => Some(SqlType::Text),
        "date" => Some(SqlType::Date),
        "time" => Some(SqlType::Time),
        "timestamp" | "timestamptz" | "datetime" => Some(SqlType::Timestamptz),
        "uuid" => Some(SqlType::Uuid),
        // SQLite doesn't have a native JSON column type, but a user
        // declaring `CREATE TABLE t (data JSON)` parses the type-name
        // verbatim into `sqlite_master` and `PRAGMA table_info`. Treat
        // that as a hint that the column holds JSON content and route
        // it through `SqlType::Json` (which lowers to TEXT on SQLite
        // anyway).
        "json" | "jsonb" => Some(SqlType::Json),
        // Django's DecimalField declares its SQLite columns as `decimal`;
        // `numeric` is the SQL-standard spelling. Both map to umbral's
        // Decimal (rendered as `rust_decimal::Decimal`). NOTE: Decimal is
        // Postgres-only at v1 (sqlx has no SQLite Encode/Decode for it),
        // so a model carrying this field passes the boot check only
        // against Postgres. That's deliberate: inspectdb emits the
        // faithful type and lets the backend system check surface the
        // SQLite limitation, rather than silently downgrading to a lossy
        // f64. Width parameters (`decimal(9,6)`) are already stripped by
        // the `split_once('(')` above.
        "decimal" | "numeric" => Some(SqlType::Decimal),
        "blob" | "bytea" => Some(SqlType::Bytes),
        _ => None,
    }
}

// `derive_table_name` (was `to_snake_case`) and `pascal_case` (now
// `pascal_case_from_table`) are imported from `umbral_casing` at the top
// of this file. The local copies were removed in the gaps2 #77 refactor.

/// Render the introspected schema as the contents of a `models.rs`
/// file. The output is one `#[derive(Model)]` struct per table, with
/// fields in declaration order and the `#[umbral(table = "…")]`
/// attribute set when the struct name differs from the SQL table.
///
/// Structs are emitted in alphabetical order by struct name so a
/// re-run against an unchanged schema produces a byte-identical file.
/// Field-type rendering uses fully-qualified `chrono::*` / `uuid::*`
/// paths so no extra `use` lines are needed at the top of the file.
pub fn render_models(schema: &IntrospectedSchema) -> String {
    render_models_with(schema, None, false)
}

/// Django's canonical user table, and the umbral-auth type it maps onto (same
/// `auth_user` table, `id: i64` PK). Under `--framework django` the generated
/// file does NOT re-declare this table; FKs to it point at umbral's `AuthUser`
/// and an import at the top lets the operator swap in a custom user model.
const DJANGO_USER_TABLE: &str = "auth_user";
const DJANGO_USER_STRUCT: &str = "AuthUser";

/// [`render_models`] with a source [`Framework`] whose *reference* conventions
/// are undone (e.g. `--framework django` strips the `<app>_` prefix off FK
/// target structs and maps `auth_user` to `AuthUser`). FK field names keep
/// their real `_id` column — umbral's own idiom. `with_table_names` additionally
/// strips the `<app>_` prefix off **struct names**, preserving the real table
/// with a `#[umbral(table)]` macro (emitted in [`render_one_struct`]).
/// Resolve every table to its final Rust struct name, applying the same rules
/// the model renderer uses: under Django `auth_user` maps to the external
/// `AuthUser`; with `--with-table-names` the `<app>_` prefix is stripped (with a
/// collision fallback to the full pascal name). Shared so the model file, the
/// migration snapshot, and M2M target resolution all agree on names.
pub(crate) fn resolve_struct_names(
    schema: &IntrospectedSchema,
    framework: Option<Framework>,
    with_table_names: bool,
) -> std::collections::HashMap<String, String> {
    let django = framework == Some(Framework::Django);
    // Django "app labels" — the leading `<app>_` segment shared by table names.
    let app_labels: std::collections::HashSet<String> = schema
        .tables
        .iter()
        .filter_map(|t| t.table.split_once('_').map(|(app, _)| app.to_string()))
        .collect();

    let mut struct_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for t in &schema.tables {
        let name = if django && t.table == DJANGO_USER_TABLE {
            DJANGO_USER_STRUCT.to_string()
        } else if with_table_names {
            django_struct_name(&t.table, &app_labels)
        } else {
            t.name.clone()
        };
        *counts.entry(name.clone()).or_default() += 1;
        struct_names.insert(t.table.clone(), name);
    }
    // Collision fallback: revert every table whose stripped name is shared to
    // its full pascal name (the external AuthUser is exempt — it's canonical).
    for t in &schema.tables {
        let name = &struct_names[&t.table];
        if counts[name] > 1 && !(django && t.table == DJANGO_USER_TABLE) {
            struct_names.insert(t.table.clone(), pascal_case_from_table(&t.table));
        }
    }
    struct_names
}

pub fn render_models_with(
    schema: &IntrospectedSchema,
    framework: Option<Framework>,
    with_table_names: bool,
) -> String {
    let django = framework == Some(Framework::Django);
    let struct_names = resolve_struct_names(schema, framework, with_table_names);

    // Does the schema reference Django's auth_user (own it, or FK at it)? If so,
    // emit the swap-your-user import.
    let uses_auth_user = django
        && schema.tables.iter().any(|t| {
            t.table == DJANGO_USER_TABLE
                || t.columns
                    .iter()
                    .any(|c| c.fk_target.as_deref() == Some(DJANGO_USER_TABLE))
        });

    let mut out = String::new();
    out.push_str(HEADER);
    if uses_auth_user {
        out.push_str(AUTH_USER_IMPORT);
    }

    let mut tables: Vec<&IntrospectedTable> = schema.tables.iter().collect();
    tables.sort_by(|a, b| struct_names[&a.table].cmp(&struct_names[&b.table]));

    for table in tables {
        // Django's auth_user is provided by umbral-auth; don't re-declare it.
        if django && table.table == DJANGO_USER_TABLE {
            continue;
        }
        out.push('\n');
        out.push_str(&render_one_struct(table, &struct_names));
    }
    out
}

/// The struct name for a table under Django: strip a leading `<app>_` app-label
/// prefix, then pascal-case (`communities_community` -> `Community`,
/// `communities_community_categories` -> `CommunityCategories`). A table whose
/// leading segment isn't a detected app label keeps its full name.
fn django_struct_name(table: &str, app_labels: &std::collections::HashSet<String>) -> String {
    let model_part = table
        .split_once('_')
        .filter(|(app, rest)| app_labels.contains(*app) && !rest.is_empty())
        .map(|(_, rest)| rest)
        .unwrap_or(table);
    pascal_case_from_table(model_part)
}

/// The import block emitted at the top of a Django-imported file. Maps
/// `auth_user` onto umbral-auth's built-in user and tells the operator how to
/// swap in a custom one.
const AUTH_USER_IMPORT: &str = "\
// This schema references Django's `auth_user`, mapped to umbral-auth's built-in
// `AuthUser` (same `auth_user` table). If you use a CUSTOM user model, replace
// the line below with your own, e.g. `use crate::models::MyUser as AuthUser;`.
use umbral_auth::AuthUser;
";

/// Two-line module doc plus the single facade import every generated
/// file needs. Kept as a constant so the empty-schema path emits
/// exactly the header and nothing else.
const HEADER: &str = "\
//! Generated by `umbral inspectdb`. Wire each struct into your App
//! builder with `.model::<StructName>()`. Re-run `inspectdb` to
//! regenerate; edits made by hand will be lost.

use umbral::prelude::*;
";

/// Render a single `#[derive(Model)]` struct for one introspected table.
/// The `#[umbral(table = "...")]` attribute is emitted only when the
/// derive's auto-derived table name (snake_case of the struct name)
/// doesn't equal the SQL table name. For the typical snake_case shape
/// The temporal SQL types whose current-timestamp default is an `auto_now_add`
/// and whose `created*`/`updated*` name (under Django) implies a timestamp.
fn is_temporal(ty: SqlType) -> bool {
    matches!(ty, SqlType::Timestamptz | SqlType::Date | SqlType::Time)
}

/// True when a raw DB default expresses "the current time" — SQLite's
/// `CURRENT_TIMESTAMP` and Postgres's `now()` / `CURRENT_TIMESTAMP` /
/// `LOCALTIMESTAMP`, tolerating a trailing `()` and a `::type` cast.
fn is_current_timestamp_default(raw: &str) -> bool {
    let s = raw.trim();
    let s = s.split("::").next().unwrap_or(s).trim();
    let s = s.trim_end_matches("()").trim();
    s.eq_ignore_ascii_case("CURRENT_TIMESTAMP")
        || s.eq_ignore_ascii_case("now")
        || s.eq_ignore_ascii_case("LOCALTIMESTAMP")
}

/// Reduce a raw DB default to a constant umbral can re-emit as
/// `#[umbral(default = "...")]`, or `None` when it can't — a sequence
/// (`nextval(...)`), a function call (`gen_random_uuid()`), or NULL. Strips a
/// Postgres `::type` cast and unwraps a single-quoted string literal so
/// `'active'::character varying` -> `active` (umbral re-quotes on emit).
fn clean_constant_default(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("null") {
        return None;
    }
    let s = s.split("::").next().unwrap_or(s).trim();
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        // A quoted string literal — accept its inner text, un-doubling the
        // SQL `''` escape. Can't be an expression, so it's always safe.
        return Some(s[1..s.len() - 1].replace("''", "'"));
    }
    // A bare token: a number (`0`, `-1`, `3.14`) or boolean (`true`/`false`).
    // Anything carrying `(` (a function / sequence) or other punctuation umbral
    // can't represent as a literal is dropped.
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Some(s.to_string());
    }
    None
}

/// Turn raw recovered defaults + framework naming into umbral's semantic field
/// attributes (`auto_now_add` / `auto_now` / `default`). Run once over the
/// introspected schema so the model renderer and the initial migration agree.
///
/// - A `CURRENT_TIMESTAMP` / `now()` default on a temporal column becomes
///   `auto_now_add`: umbral emits the correct per-backend default itself
///   (`CURRENT_TIMESTAMP` on SQLite, `now()` on Postgres), so carrying the raw
///   expression as a `#[umbral(default)]` literal (which umbral would quote)
///   would be wrong.
/// - Under `--framework django`, a `created*` timestamp with no recoverable
///   default becomes `auto_now_add` and an `updated*` / `modified*` one becomes
///   `auto_now` — Django keeps these in Python, leaving no DB default behind.
/// - Every other default is reduced to a constant literal, or dropped when
///   umbral can't represent it (see [`clean_constant_default`]).
pub fn apply_recovered_conventions(schema: &mut IntrospectedSchema, framework: Option<Framework>) {
    let django = framework == Some(Framework::Django);
    for table in &mut schema.tables {
        for col in &mut table.columns {
            if let Some(raw) = col.default.take() {
                if is_temporal(col.ty) && is_current_timestamp_default(&raw) {
                    col.auto_now_add = true;
                } else {
                    col.default = clean_constant_default(&raw);
                }
            }
            // Django's Python-managed timestamps leave no DB default; recover
            // them by name, but never override a default we actually found.
            if django && is_temporal(col.ty) && col.default.is_none() && !col.auto_now_add {
                let lower = col.name.to_ascii_lowercase();
                if lower.starts_with("created")
                    || lower.starts_with("added")
                    || lower == "date_joined"
                {
                    col.auto_now_add = true;
                } else if !col.auto_now
                    && (lower.starts_with("updated")
                        || lower.starts_with("modified")
                        || lower.starts_with("changed"))
                {
                    col.auto_now = true;
                }
            }
        }
    }
}

/// Strip Django's `_id` suffix off foreign-key COLUMNS (`author_id` ->
/// `author`), matching how umbral models are actually written
/// (`pub author: ForeignKey<AuthUser>` in `examples/shop`, accessed as
/// `post.author`). Because inspectdb targets a *fresh* database with fresh
/// migrations — not the source DB — the new column is simply named `author`,
/// so no `#[sqlx(rename)]` is needed; umbral maps the field name to the column.
///
/// Only real FK columns are touched, and only when the stripped name is free
/// (no other column already owns it, no two FKs collide on it) so the rename is
/// always unambiguous. Composite `unique_together` / index groups that
/// referenced the old `author_id` are rewritten to `author` in lockstep, or the
/// generated `#[umbral(unique_together = [[...]])]` would name a column that no
/// longer exists.
fn strip_django_fk_id_suffix(schema: &mut IntrospectedSchema) {
    for table in &mut schema.tables {
        // Propose `old -> new` renames, guarding against collisions.
        let mut renames: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let existing: std::collections::HashSet<&str> =
            table.columns.iter().map(|c| c.name.as_str()).collect();
        let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
        for col in &table.columns {
            if col.fk_target.is_none() {
                continue;
            }
            if let Some(base) = col.name.strip_suffix("_id") {
                if !base.is_empty() && !existing.contains(base) && claimed.insert(base.to_string())
                {
                    renames.insert(col.name.clone(), base.to_string());
                }
            }
        }
        if renames.is_empty() {
            continue;
        }
        for col in &mut table.columns {
            if let Some(new) = renames.get(&col.name) {
                col.name = new.clone();
            }
        }
        for group in table
            .unique_together
            .iter_mut()
            .chain(table.indexes.iter_mut())
        {
            for c in group.iter_mut() {
                if let Some(new) = renames.get(c) {
                    *c = new.clone();
                }
            }
        }
    }
}

/// Pick the owner side of a Django M2M join table. Django names the table
/// `<owner_table>_<field>`, so the owner is the FK target that prefixes the
/// join-table name and the remainder is the field. Returns
/// `(owner_table, field_name, target_table)`, or `None` when neither target
/// prefixes the name (a non-standard through table we can't safely fold).
fn pick_m2m_owner(join_table: &str, ta: &str, tb: &str) -> Option<(String, String, String)> {
    let try_owner = |owner: &str, target: &str| -> Option<(String, String, String)> {
        join_table
            .strip_prefix(&format!("{owner}_"))
            .filter(|field| !field.is_empty())
            .map(|field| (owner.to_string(), field.to_string(), target.to_string()))
    };
    match (try_owner(ta, tb), try_owner(tb, ta)) {
        // Both targets prefix the name (e.g. one is a prefix of the other, or a
        // self-M2M) — prefer the longer, more specific owner table.
        (Some(ra), Some(rb)) => Some(if ta.len() >= tb.len() { ra } else { rb }),
        (Some(r), None) | (None, Some(r)) => Some(r),
        (None, None) => None,
    }
}

/// Fold Django M2M join tables into `M2M<T>` fields on their owner model. A
/// pure junction — exactly two FK columns, every other column just the
/// surrogate PK — named `<owner_table>_<field>` becomes `owner.field:
/// M2M<Target>`, and the join table is dropped from the schema (umbral
/// auto-generates its own junction from the field). Django-only: without the
/// naming convention the field name can't be recovered, so other schemas keep
/// the join table as a plain model. Skips a table whose owner is `auth_user`
/// (external, not re-declared) or whose stripped field would collide with an
/// existing column on the owner.
fn detect_m2m_relations(
    schema: &mut IntrospectedSchema,
    framework: Option<Framework>,
    with_table_names: bool,
) {
    if framework != Some(Framework::Django) {
        return;
    }
    let struct_names = resolve_struct_names(schema, framework, with_table_names);
    let owner_cols: std::collections::HashMap<String, std::collections::HashSet<String>> = schema
        .tables
        .iter()
        .map(|t| {
            (
                t.table.clone(),
                t.columns.iter().map(|c| c.name.clone()).collect(),
            )
        })
        .collect();

    let mut to_remove: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut additions: Vec<(String, IntrospectedM2M)> = Vec::new();
    for t in &schema.tables {
        let fk_cols: Vec<&IntrospectedColumn> =
            t.columns.iter().filter(|c| c.fk_target.is_some()).collect();
        // A pure junction: exactly two FKs, everything else just the PK.
        let is_junction = fk_cols.len() == 2
            && t.columns
                .iter()
                .all(|c| c.fk_target.is_some() || c.primary_key);
        if !is_junction {
            continue;
        }
        let ta = fk_cols[0].fk_target.as_deref().unwrap();
        let tb = fk_cols[1].fk_target.as_deref().unwrap();
        let Some((owner_table, field, target_table)) = pick_m2m_owner(&t.table, ta, tb) else {
            continue;
        };
        // Owner `auth_user` isn't re-declared, so we can't hang a field on it.
        if owner_table == DJANGO_USER_TABLE {
            continue;
        }
        // Don't shadow a real column on the owner.
        if owner_cols
            .get(&owner_table)
            .is_some_and(|cols| cols.contains(&field))
        {
            continue;
        }
        let target_name = struct_names
            .get(&target_table)
            .cloned()
            .unwrap_or_else(|| pascal_case_from_table(&target_table));
        additions.push((
            owner_table,
            IntrospectedM2M {
                field_name: field,
                target_table,
                target_name,
            },
        ));
        to_remove.insert(t.table.clone());
    }

    for (owner_table, m2m) in additions {
        if let Some(owner) = schema.tables.iter_mut().find(|t| t.table == owner_table) {
            owner.m2m.push(m2m);
        }
    }
    schema.tables.retain(|t| !to_remove.contains(&t.table));
}

/// (`blog_post` -> `BlogPost` -> derive computes `"blog_post"`), the
/// attribute is redundant and is left off. For unusual SQL casings
/// (`POSTS` -> `Posts` -> derive computes `"posts"` not `"POSTS"`),
/// the attribute is emitted and the M3.1 derive picks it up to
/// override the default. See `umbral-macros/src/lib.rs` for the
/// attribute parser.
fn render_one_struct(
    table: &IntrospectedTable,
    struct_names: &std::collections::HashMap<String, String>,
) -> String {
    // The resolved struct name for this table (app-prefix-stripped under Django,
    // full pascal otherwise), and the same resolution for FK targets.
    let this_struct = struct_names
        .get(&table.table)
        .cloned()
        .unwrap_or_else(|| table.name.clone());
    let resolve_target = |target: &str| -> String {
        struct_names
            .get(target)
            .cloned()
            .unwrap_or_else(|| pascal_case_from_table(target))
    };

    let mut out = String::new();
    // `sqlx::FromRow` is required (the `Model` trait bounds it as a supertrait),
    // and `Model` also requires `serde::Serialize` + `DeserializeOwned` (a
    // `ForeignKey<T>` needs `T: DeserializeOwned`), so both serde derives are
    // mandatory for the generated file to compile.
    out.push_str(
        "#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]\n",
    );
    // Emit `#[umbral(table)]` whenever the struct name doesn't snake_case back
    // to the SQL table — always true once the app prefix is stripped.
    if to_snake_case(&this_struct) != table.table {
        out.push_str(&format!("#[umbral(table = \"{}\")]\n", table.table));
    }
    // Struct-level composite index attributes: multi-column UNIQUE constraints
    // and multi-column indexes recovered from the schema.
    if let Some(attr) = composite_groups_attr("unique_together", &table.unique_together) {
        out.push_str(&attr);
    }
    if let Some(attr) = composite_groups_attr("indexes", &table.indexes) {
        out.push_str(&attr);
    }
    out.push_str(&format!("pub struct {this_struct} {{\n"));
    for column in &table.columns {
        // Per-column attributes recovered from the schema: single-column
        // UNIQUE / index constraints become `#[umbral(unique)]` /
        // `#[umbral(index)]` so a re-migrate rebuilds them.
        if column.unique {
            out.push_str("    #[umbral(unique)]\n");
        }
        if column.index {
            out.push_str("    #[umbral(index)]\n");
        }
        // Recovered temporal semantics / constant default. `auto_now_add` and
        // `auto_now` are mutually exclusive with a literal default (a
        // current-timestamp default is lifted to `auto_now_add` upstream).
        if column.auto_now_add {
            out.push_str("    #[umbral(auto_now_add)]\n");
        } else if column.auto_now {
            out.push_str("    #[umbral(auto_now)]\n");
        } else if let Some(def) = &column.default {
            out.push_str(&format!(
                "    #[umbral(default = \"{}\")]\n",
                def.replace('\\', "\\\\").replace('"', "\\\"")
            ));
        }
        // A primary key not named `id` must be marked so the derive can find
        // it (Django's `authtoken_token.key`, `django_session.session_key`, …).
        if column.primary_key && column.name != "id" {
            out.push_str("    #[umbral(primary_key)]\n");
        }
        // PostGIS: emit the recovered subtype + SRID so the geometry column
        // round-trips as `geometry(Point, 4326)` rather than the unconstrained
        // base type.
        if let Some(attr) = geometry_attr(column.ty) {
            out.push_str(&format!("    {attr}\n"));
        }
        // A FK field keeps its REAL column name (`author_id`), not a prettified
        // `author`: umbral uses the field name as the column name, so this
        // avoids a `#[sqlx(rename)]` on every foreign key and keeps the index /
        // field reading clearly against the actual column. Only the target
        // STRUCT name is app-prefix-stripped (`ForeignKey<Author>`).
        let (desired, ty) = match &column.fk_target {
            Some(target) => {
                let target_struct = resolve_target(target);
                let ty = if column.nullable {
                    format!("Option<ForeignKey<{target_struct}>>")
                } else {
                    format!("ForeignKey<{target_struct}>")
                };
                (column.name.clone(), ty)
            }
            None => (
                column.name.clone(),
                render_field_type(column.ty, column.nullable),
            ),
        };
        // Escape a Rust keyword / otherwise-invalid identifier (a column named
        // `type`, `match`, …) by suffixing `_`. Whenever the Rust field name
        // ends up different from the DB column, bind them with `#[sqlx(rename)]`
        // so `FromRow` and umbral's column name both resolve to the real column.
        let field_name = safe_field_ident(&desired);
        if field_name != column.name {
            out.push_str(&format!("    #[sqlx(rename = \"{}\")]\n", column.name));
        }
        out.push_str(&format!("    pub {field_name}: {ty},\n"));
    }
    // Many-to-many fields recovered from Django join tables. `M2M<T>` has no
    // column on this table — umbral auto-generates the junction — so they're
    // emitted after the real columns. `M2M<T, P>`'s parent-PK generic `P`
    // defaults to `i64`; a non-i64 owner PK (Django's `i32` AutoField, a UUID /
    // slug PK) must spell it out or the derive's `set_parent_id(id: P)` won't
    // typecheck.
    let parent_pk_ty = table
        .columns
        .iter()
        .find(|c| c.primary_key)
        .map(|c| render_field_type(c.ty, false));
    for m2m in &table.m2m {
        let field = safe_field_ident(&m2m.field_name);
        let target = resolve_target(&m2m.target_table);
        let ty = match parent_pk_ty.as_deref() {
            Some(pk) if pk != "i64" => format!("M2M<{target}, {pk}>"),
            _ => format!("M2M<{target}>"),
        };
        out.push_str(&format!("    pub {field}: {ty},\n"));
    }
    out.push_str("}\n");
    out
}

/// The Rust reserved words that can't be a bare field identifier. A column with
/// one of these names is suffixed with `_` (`type` -> `type_`) and bound to the
/// real column via `#[sqlx(rename)]`.
fn is_rust_keyword(s: &str) -> bool {
    matches!(
        s,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "box"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
    )
}

/// Render a `#[umbral(<name> = [["a","b"], ["c"]])]` struct-level attribute from
/// a list of column-name groups, or `None` when there are no groups. Used for
/// `unique_together` and `indexes`.
fn composite_groups_attr(name: &str, groups: &[Vec<String>]) -> Option<String> {
    if groups.is_empty() {
        return None;
    }
    let rendered = groups
        .iter()
        .map(|g| {
            let cols = g
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{cols}]")
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("#[umbral({name} = [{rendered}])]\n"))
}

/// The `#[umbral(geometry|geography = "<kind>", srid = N)]` attribute for a
/// PostGIS column, or `None` for a non-spatial column. Renders the subtype
/// recovered from the catalog so the column round-trips with its real shape.
fn geometry_attr(ty: SqlType) -> Option<String> {
    use crate::orm::GeometryKind;
    let (base, spec) = match ty {
        SqlType::Geometry(s) => ("geometry", s),
        SqlType::Geography(s) => ("geography", s),
        _ => return None,
    };
    let kind = match spec.kind {
        GeometryKind::Geometry => "geometry",
        GeometryKind::Point => "point",
        GeometryKind::LineString => "linestring",
        GeometryKind::Polygon => "polygon",
        GeometryKind::MultiPoint => "multipoint",
        GeometryKind::MultiLineString => "multilinestring",
        GeometryKind::MultiPolygon => "multipolygon",
        GeometryKind::GeometryCollection => "geometrycollection",
    };
    Some(format!(
        "#[umbral({base} = \"{kind}\", srid = {})]",
        spec.srid
    ))
}

/// Turn a column name into a valid, non-keyword Rust field identifier.
fn safe_field_ident(name: &str) -> String {
    if is_rust_keyword(name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// Map `(SqlType, nullable)` to the Rust type string the derive macro's
/// `classify_field_type` accepts. Mirrors the table in
/// `umbral-macros/src/lib.rs` (see `FieldKind` for the full catalogue).
fn render_field_type(ty: SqlType, nullable: bool) -> String {
    let base = match ty {
        SqlType::SmallInt => "i16".to_string(),
        SqlType::Integer => "i32".to_string(),
        SqlType::BigInt => "i64".to_string(),
        SqlType::Real => "f32".to_string(),
        SqlType::Double => "f64".to_string(),
        SqlType::Boolean => "bool".to_string(),
        SqlType::Text => "String".to_string(),
        SqlType::Date => "chrono::NaiveDate".to_string(),
        SqlType::Time => "chrono::NaiveTime".to_string(),
        SqlType::Timestamptz => "chrono::DateTime<chrono::Utc>".to_string(),
        SqlType::Uuid => "uuid::Uuid".to_string(),
        SqlType::Json => "serde_json::Value".to_string(),
        // Recurse through the element's SqlType. Wrapping in `Vec<...>`
        // matches the derive's catalogue: a `Vec<i64>` declares an
        // `Array(ArrayElement::BigInt)` field.
        SqlType::Array(elem) => format!("Vec<{}>", render_field_type(elem.to_sql_type(), false)),
        // Phase 4.4: Postgres network address types. Both `Inet` and
        // `Cidr` round-trip through `ipnetwork::IpNetwork`; `MacAddr`
        // uses the `mac_address` crate.
        SqlType::Inet => "ipnetwork::IpNetwork".to_string(),
        SqlType::Cidr => "ipnetwork::IpNetwork".to_string(),
        SqlType::MacAddr => "mac_address::MacAddress".to_string(),
        // gaps2 #70: text-backed Postgres types surface as `String`.
        // inspectdb can't recover which `#[umbral(...)]` attr produced
        // the column (the attr lives only in the source model, not the
        // DB), so the generated model is a plain `String`; the user
        // re-adds `#[umbral(xml)]` / `#[umbral(ltree)]` / `#[umbral(bit)]`
        // if they want the native type back on a re-migrate.
        SqlType::Xml => "String".to_string(),
        SqlType::Ltree => "String".to_string(),
        SqlType::Bit => "String".to_string(),
        SqlType::FullText => "umbral::orm::TsVector".to_string(),
        // ForeignKey inspectdb renders as i64 for now; the FK relationship
        // introspection that would emit ForeignKey<T> is deferred.
        SqlType::ForeignKey => "i64".to_string(),
        // BLOB / BYTEA columns surface as Vec<u8> in user code.
        SqlType::Bytes => "Vec<u8>".to_string(),
        // BUG-10: NUMERIC introspection renders as
        // `rust_decimal::Decimal`. inspectdb reads the column type
        // from Postgres' `information_schema`; the resulting
        // model imports use this exact path.
        SqlType::Decimal => "rust_decimal::Decimal".to_string(),
        SqlType::DecimalN(_) => "rust_decimal::Decimal".to_string(),
        // Arbitrary-precision decimal renders as `bigdecimal::BigDecimal`.
        // inspectdb never *emits* BigDecimal on its own — it maps every DB
        // `numeric`/`decimal` column to the friendlier `rust_decimal::Decimal`
        // (see the type classifier) — so this arm only fires if a snapshot
        // already carries BigDecimal from a hand-written model. Kept here so
        // the render stays total over `SqlType`.
        SqlType::BigDecimal => "bigdecimal::BigDecimal".to_string(),
        // PostGIS geometry/geography both surface as the `postgis`-feature
        // `Geometry` newtype; the subtype + SRID ride the `#[umbral(...)]`
        // attribute the model renderer emits alongside this type.
        SqlType::Geometry(_) | SqlType::Geography(_) => "umbral::orm::gis::Geometry".to_string(),
    };
    let base = base.as_str();
    if nullable {
        format!("Option<{base}>")
    } else {
        base.to_string()
    }
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
            display: t.name.clone(),
            icon: "database".to_string(),
            database: None,
            singleton: false,
            unique_together: Vec::new(),
            indexes: Vec::new(),
            ordering: Vec::new(),
            // Recovered many-to-many relations; drives the junction snapshot.
            m2m_relations: t
                .m2m
                .iter()
                .map(|r| crate::migrate::M2MRelation {
                    field_name: r.field_name.clone(),
                    target_table: r.target_table.clone(),
                    target_name: r.target_name.clone(),
                })
                .collect(),
            soft_delete: false,
            audited: false,
            // inspectdb introspects TABLES; a view it finds becomes a plain model
            // with `view: None`, i.e. the framework will not try to manage it.
            view: None,
            materialized: false,
            // inspectdb has no plugin attribute to read; default to "app".
            app_label: "app".to_string(),
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));

    let mut operations: Vec<Operation> = schema
        .tables
        .iter()
        .map(|t| Operation::CreateTable {
            table: t.table.clone(),
            columns: t.columns.iter().map(Column::from).collect(),
            unique_together: t.unique_together.clone(),
            indexes: t.indexes.clone(),
        })
        .collect();

    // Emit a junction table per recovered M2M relation (the join table it came
    // from was dropped from the schema). `<parent_table>_<field>` is umbral's
    // junction naming, matching what `M2M<T>` autogenerates on a re-migrate.
    let pk_of = |table_name: &str| -> (String, SqlType) {
        schema
            .tables
            .iter()
            .find(|t| t.table == table_name)
            .and_then(|t| t.columns.iter().find(|c| c.primary_key))
            .map(|c| (c.name.clone(), c.ty))
            .unwrap_or_else(|| ("id".to_string(), SqlType::BigInt))
    };
    for t in &schema.tables {
        for m2m in &t.m2m {
            let (parent_col, parent_ty) = pk_of(&t.table);
            let (child_col, child_ty) = pk_of(&m2m.target_table);
            operations.push(Operation::CreateM2MTable {
                junction_table: format!("{}_{}", t.table, m2m.field_name),
                parent_table: t.table.clone(),
                parent_col,
                child_table: m2m.target_table.clone(),
                child_col,
                parent_ty,
                child_ty,
            });
        }
    }

    MigrationFile {
        id: INITIAL_MIGRATION_ID.to_string(),
        plugin: INSPECTED_PLUGIN_NAME.to_string(),
        depends_on: Vec::new(),
        operations,
        snapshot_after: Snapshot { models },
        replaces: Vec::new(),
    }
}

/// Write `models.rs` and the initial migration to `output`. Creates
/// `output/` and `output/migrations/<INSPECTED_PLUGIN_NAME>/` as
/// needed. Returns the report carrying the table / column counts and
/// the paths.
///
/// The migration is pretty-printed so the file diffs cleanly when a
/// later `makemigrations` writes the next migration alongside.
pub async fn write_outputs(
    output: &Path,
    models_src: &str,
    migration: &MigrationFile,
) -> Result<InspectReport, InspectError> {
    std::fs::create_dir_all(output)?;

    let models_path = output.join("models.rs");
    std::fs::write(&models_path, models_src)?;

    let plugin_dir = output.join("migrations").join(INSPECTED_PLUGIN_NAME);
    std::fs::create_dir_all(&plugin_dir)?;

    let migration_path = plugin_dir.join(format!("{}.json", migration.id));
    let json = serde_json::to_string_pretty(migration)?;
    std::fs::write(&migration_path, json)?;

    let (tables, columns) =
        migration
            .operations
            .iter()
            .fold((0usize, 0usize), |(t, c), op| match op {
                Operation::CreateTable { columns, .. } => (t + 1, c + columns.len()),
                Operation::CreateM2MTable { .. } => (t + 1, c + 2),
                Operation::CreateView { .. }
                | Operation::DropView { .. }
                | Operation::DropTable { .. }
                | Operation::DropM2MTable { .. }
                | Operation::AddColumn { .. }
                | Operation::DropColumn { .. }
                | Operation::AlterColumn { .. }
                | Operation::RenameTable { .. }
                | Operation::RenameColumn { .. }
                | Operation::SetColumnComment { .. }
                | Operation::AddIndex { .. }
                | Operation::DropIndex { .. }
                | Operation::RunSql { .. } => (t, c),
            });

    Ok(InspectReport {
        tables,
        columns,
        models_path,
        migration_path,
    })
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
            // Recovered foreign key: the referenced table, so the migration
            // re-emits `REFERENCES "<target>"("id")`.
            fk_target: c.fk_target.clone(),
            noform: false,
            privileged: false,
            private: false,
            secret: false,
            db_constraint: true,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: Vec::new(),
            choice_labels: Vec::new(),
            // Recovered constant default (`''` when none / unrepresentable), so
            // the initial migration re-emits the DDL `DEFAULT` clause.
            default: c.default.clone().unwrap_or_default(),
            is_multichoice: false,
            // Recovered single-column UNIQUE / index constraints.
            unique: c.unique,
            on_delete: crate::orm::FkAction::NoAction,
            on_update: crate::orm::FkAction::NoAction,
            index: c.index,
            // Recovered temporal semantics — a re-migrate rebuilds the correct
            // per-backend default (CURRENT_TIMESTAMP / now()).
            auto_now_add: c.auto_now_add,
            auto_uuid: false,
            auto_now: c.auto_now,
            auto_user_add: false,
            auto_user: false,
            trim: false,
            lowercase: false,
            case_insensitive: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: SqlType, primary_key: bool, nullable: bool) -> IntrospectedColumn {
        IntrospectedColumn {
            name: name.to_string(),
            ty,
            primary_key,
            nullable,
            fk_target: None,
            unique: false,
            index: false,
            default: None,
            auto_now_add: false,
            auto_now: false,
        }
    }

    #[test]
    fn empty_schema_renders_header_only() {
        let out = render_models(&IntrospectedSchema { tables: Vec::new() });
        assert_eq!(out, HEADER);
    }

    #[test]
    fn snake_case_table_skips_attribute_when_derive_round_trips() {
        let schema = IntrospectedSchema {
            tables: vec![IntrospectedTable {
                table: "blog_post".to_string(),
                name: "BlogPost".to_string(),
                columns: vec![
                    col("id", SqlType::BigInt, true, false),
                    col("title", SqlType::Text, false, false),
                ],

                unique_together: Vec::new(),
                indexes: Vec::new(),
                m2m: Vec::new(),
            }],
        };
        let out = render_models(&schema);
        // `BlogPost` snake_cases to `blog_post` via the derive, so the
        // attribute is redundant and is left off. This keeps the
        // generated file compatible with the M3 derive, which doesn't
        // yet recognise `#[umbral(...)]` attributes.
        assert!(!out.contains("#[umbral(table"));
        assert!(out.contains("pub struct BlogPost {"));
        assert!(out.contains("pub id: i64,"));
        assert!(out.contains("pub title: String,"));
    }

    #[test]
    fn lowercase_single_word_table_skips_attribute() {
        // `post` -> `Post` -> derive snake_cases to `"post"`, matches
        // the source table verbatim, so the attribute is left off.
        let schema = IntrospectedSchema {
            tables: vec![IntrospectedTable {
                table: "post".to_string(),
                name: "Post".to_string(),
                columns: vec![col("id", SqlType::BigInt, true, false)],
                unique_together: Vec::new(),
                indexes: Vec::new(),
                m2m: Vec::new(),
            }],
        };
        let out = render_models(&schema);
        assert!(!out.contains("#[umbral(table"));
        assert!(out.contains("pub struct Post {"));
    }

    #[test]
    fn non_round_tripping_table_name_keeps_attribute() {
        // SQL tables with names the derive's snake_case won't reach
        // (e.g. uppercase, runs of capitals, leading digits) need the
        // explicit attribute. This case is rare in real ports but
        // the renderer should still cover it for the derive's eventual
        // attribute-support landing.
        let schema = IntrospectedSchema {
            tables: vec![IntrospectedTable {
                table: "POSTS".to_string(),
                name: "Posts".to_string(),
                columns: vec![col("id", SqlType::BigInt, true, false)],
                unique_together: Vec::new(),
                indexes: Vec::new(),
                m2m: Vec::new(),
            }],
        };
        let out = render_models(&schema);
        assert!(out.contains("#[umbral(table = \"POSTS\")]"));
    }

    #[test]
    fn nullable_column_wraps_in_option() {
        let schema = IntrospectedSchema {
            tables: vec![IntrospectedTable {
                table: "post".to_string(),
                name: "Post".to_string(),
                columns: vec![
                    col("id", SqlType::BigInt, true, false),
                    col("published_at", SqlType::Timestamptz, false, true),
                ],

                unique_together: Vec::new(),
                indexes: Vec::new(),
                m2m: Vec::new(),
            }],
        };
        let out = render_models(&schema);
        assert!(out.contains("pub published_at: Option<chrono::DateTime<chrono::Utc>>,"));
    }

    #[test]
    fn type_catalogue_renders_each_sql_type() {
        let schema = IntrospectedSchema {
            tables: vec![IntrospectedTable {
                table: "kitchen_sink".to_string(),
                name: "KitchenSink".to_string(),
                columns: vec![
                    col("id", SqlType::BigInt, true, false),
                    col("small", SqlType::SmallInt, false, false),
                    col("medium", SqlType::Integer, false, false),
                    col("real_v", SqlType::Real, false, false),
                    col("double_v", SqlType::Double, false, false),
                    col("flag", SqlType::Boolean, false, false),
                    col("note", SqlType::Text, false, false),
                    col("day", SqlType::Date, false, false),
                    col("clock", SqlType::Time, false, false),
                    col("at", SqlType::Timestamptz, false, false),
                    col("uid", SqlType::Uuid, false, false),
                ],

                unique_together: Vec::new(),
                indexes: Vec::new(),
                m2m: Vec::new(),
            }],
        };
        let out = render_models(&schema);
        for expected in [
            "pub id: i64,",
            "pub small: i16,",
            "pub medium: i32,",
            "pub real_v: f32,",
            "pub double_v: f64,",
            "pub flag: bool,",
            "pub note: String,",
            "pub day: chrono::NaiveDate,",
            "pub clock: chrono::NaiveTime,",
            "pub at: chrono::DateTime<chrono::Utc>,",
            "pub uid: uuid::Uuid,",
        ] {
            assert!(out.contains(expected), "missing field render: {expected}");
        }
    }

    #[test]
    fn structs_are_sorted_by_name() {
        let schema = IntrospectedSchema {
            tables: vec![
                IntrospectedTable {
                    table: "zebra".to_string(),
                    name: "Zebra".to_string(),
                    columns: vec![col("id", SqlType::BigInt, true, false)],
                    unique_together: Vec::new(),
                    indexes: Vec::new(),
                    m2m: Vec::new(),
                },
                IntrospectedTable {
                    table: "antelope".to_string(),
                    name: "Antelope".to_string(),
                    columns: vec![col("id", SqlType::BigInt, true, false)],
                    unique_together: Vec::new(),
                    indexes: Vec::new(),
                    m2m: Vec::new(),
                },
            ],
        };
        let out = render_models(&schema);
        let antelope_at = out.find("struct Antelope").expect("Antelope rendered");
        let zebra_at = out.find("struct Zebra").expect("Zebra rendered");
        assert!(antelope_at < zebra_at);
    }

    #[test]
    fn header_carries_the_regen_warning_and_facade_import() {
        let out = render_models(&IntrospectedSchema { tables: Vec::new() });
        assert!(out.contains("Generated by `umbral inspectdb`"));
        assert!(out.contains("edits made by hand will be lost"));
        assert!(out.contains("use umbral::prelude::*;"));
    }

    // --------------------------------------------------------------- //
    // SQLite type-mapping coverage.                                    //
    // --------------------------------------------------------------- //

    /// Django's `PositiveIntegerField` family declares its SQLite columns
    /// with an `unsigned` qualifier (`smallint unsigned`, `integer
    /// unsigned`, `bigint unsigned`). SQLite ignores the qualifier for
    /// affinity and Django range-caps the value to the signed max, so the
    /// mapper strips the qualifier and routes to the base signed type
    /// instead of raising `UnsupportedColumnType`. Regression test for a
    /// port from a Django-managed schema failing on the first such column.
    #[test]
    fn map_sqlite_type_strips_signedness_qualifier() {
        assert_eq!(
            map_sqlite_type("smallint unsigned"),
            Some(SqlType::SmallInt)
        );
        assert_eq!(map_sqlite_type("integer unsigned"), Some(SqlType::Integer));
        assert_eq!(map_sqlite_type("bigint unsigned"), Some(SqlType::BigInt));
        // Case-insensitive and MySQL-style `signed` qualifier too.
        assert_eq!(map_sqlite_type("INTEGER UNSIGNED"), Some(SqlType::Integer));
        assert_eq!(map_sqlite_type("int signed"), Some(SqlType::Integer));
        // Plain types are unaffected.
        assert_eq!(map_sqlite_type("integer"), Some(SqlType::Integer));
    }

    /// Django's DecimalField declares SQLite columns as `decimal`;
    /// `numeric` is the standard spelling. Both map to `SqlType::Decimal`
    /// (Postgres-only at v1, but inspectdb emits the faithful type rather
    /// than a lossy f64). Width parameters are stripped like any other.
    #[test]
    fn map_sqlite_type_maps_decimal_and_numeric() {
        assert_eq!(map_sqlite_type("decimal"), Some(SqlType::Decimal));
        assert_eq!(map_sqlite_type("numeric"), Some(SqlType::Decimal));
        assert_eq!(map_sqlite_type("DECIMAL(9,6)"), Some(SqlType::Decimal));
        assert_eq!(map_sqlite_type("numeric(10, 2)"), Some(SqlType::Decimal));
    }

    // --------------------------------------------------------------- //
    // Postgres type-mapping coverage (Phase 3).                        //
    // --------------------------------------------------------------- //

    /// Every variant of the M5 SqlType catalogue has a mapping from
    /// the canonical Postgres `information_schema.columns.data_type`
    /// value back to the variant. Lockstep with
    /// `crate::backend::PostgresBackend::map_type` — if a SqlType
    /// variant lands, both `map_type` (outbound) and `map_postgres_type`
    /// (inbound) need an arm.
    #[test]
    fn map_postgres_type_covers_the_full_catalogue() {
        assert_eq!(map_postgres_type("smallint"), Some(SqlType::SmallInt));
        assert_eq!(map_postgres_type("integer"), Some(SqlType::Integer));
        assert_eq!(map_postgres_type("bigint"), Some(SqlType::BigInt));
        assert_eq!(map_postgres_type("real"), Some(SqlType::Real));
        assert_eq!(map_postgres_type("double precision"), Some(SqlType::Double));
        assert_eq!(map_postgres_type("boolean"), Some(SqlType::Boolean));
        assert_eq!(map_postgres_type("text"), Some(SqlType::Text));
        assert_eq!(
            map_postgres_type("character varying"),
            Some(SqlType::Text),
            "VARCHAR maps to Text",
        );
        assert_eq!(
            map_postgres_type("character"),
            Some(SqlType::Text),
            "CHAR maps to Text",
        );
        assert_eq!(map_postgres_type("date"), Some(SqlType::Date));
        assert_eq!(
            map_postgres_type("time without time zone"),
            Some(SqlType::Time),
        );
        assert_eq!(
            map_postgres_type("time with time zone"),
            Some(SqlType::Time)
        );
        assert_eq!(
            map_postgres_type("timestamp without time zone"),
            Some(SqlType::Timestamptz),
        );
        assert_eq!(
            map_postgres_type("timestamp with time zone"),
            Some(SqlType::Timestamptz),
        );
        assert_eq!(map_postgres_type("uuid"), Some(SqlType::Uuid));
        // Phase 4: both `json` and `jsonb` round-trip to the portable
        // `SqlType::Json` (DDL renders as `jsonb` on Postgres, TEXT on
        // SQLite).
        assert_eq!(map_postgres_type("json"), Some(SqlType::Json));
        assert_eq!(map_postgres_type("jsonb"), Some(SqlType::Json));
        // Phase 4.4: Postgres network address types.
        assert_eq!(map_postgres_type("inet"), Some(SqlType::Inet));
        assert_eq!(map_postgres_type("cidr"), Some(SqlType::Cidr));
        assert_eq!(map_postgres_type("macaddr"), Some(SqlType::MacAddr));
        // BLOB / BYTEA — Vec<u8> in Rust.
        assert_eq!(map_postgres_type("bytea"), Some(SqlType::Bytes));
        // NUMERIC / DECIMAL — information_schema reports both as `numeric`.
        assert_eq!(map_postgres_type("numeric"), Some(SqlType::Decimal));
        assert_eq!(map_postgres_type("decimal"), Some(SqlType::Decimal));
    }

    /// Postgres-specific types umbral doesn't model yet surface as
    /// `None` so the caller produces `UnsupportedColumnType` with the
    /// raw type string preserved. The lookup most likely to bite a port
    /// now is `ARRAY`; the user fixes by hand or waits for the catalogue
    /// to grow.
    ///
    /// Note `json`/`jsonb` are NOT on this list — Phase 4's `Json`
    /// SqlType variant maps both back to `SqlType::Json`. Likewise
    /// `inet`/`cidr`/`macaddr` left this list when Phase 4.4 added
    /// the matching SqlType variants, and `numeric`/`bytea` left once
    /// `SqlType::Decimal` / `SqlType::Bytes` shipped. The companion arms
    /// in `map_postgres_type` are covered by
    /// `map_postgres_type_covers_the_full_catalogue` above.
    #[test]
    fn map_postgres_type_returns_none_for_postgres_only_types() {
        // `numeric` and `bytea` USED to be off-catalogue and returned
        // None; once SqlType::Decimal / SqlType::Bytes shipped they
        // started routing to those variants. Asserted in the positive
        // `map_postgres_type_covers_the_full_catalogue` test instead.
        assert_eq!(map_postgres_type("ARRAY"), None);
    }

    /// The mapping is case-insensitive on the input but matches against
    /// the canonical lowercase form information_schema reports. Whether
    /// the operator's DB returns `INTEGER` (uppercase, from a quoted
    /// type) or `integer` shouldn't matter.
    #[test]
    fn map_postgres_type_is_case_insensitive_on_input() {
        assert_eq!(map_postgres_type("INTEGER"), Some(SqlType::Integer));
        assert_eq!(map_postgres_type("Bigint"), Some(SqlType::BigInt));
        assert_eq!(map_postgres_type("UUID"), Some(SqlType::Uuid));
    }

    /// Surrounding whitespace doesn't break the lookup. Trimming
    /// matches `map_sqlite_type`'s `trim()`; both functions parse
    /// values straight from a sqlx row and the trim is a cheap
    /// safety net.
    #[test]
    fn map_postgres_type_trims_whitespace() {
        assert_eq!(map_postgres_type("  bigint  "), Some(SqlType::BigInt));
    }
}
