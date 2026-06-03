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

use sqlx::{PgPool, Row, SqlitePool};

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
            InspectError::Io(e) => write!(f, "umbra inspectdb: io: {e}"),
            InspectError::Json(e) => write!(f, "umbra inspectdb: json: {e}"),
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
    let schema = match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(pool) => introspect_pool(pool).await?,
        crate::db::DbPool::Postgres(pool) => introspect_pool_pg(pool).await?,
    };
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
pub async fn introspect_pool(pool: &SqlitePool) -> Result<IntrospectedSchema, InspectError> {
    // List user tables in lexical name order. `sqlite_master` carries
    // both tables and indexes; the `type = 'table'` predicate scopes the
    // result to tables. The skip-list takes out SQLite's internal
    // bookkeeping (`sqlite_%`) and umbra's own tracking table, which
    // would otherwise loop back through the migration engine.
    let table_rows = sqlx::query(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' \
           AND name NOT LIKE 'sqlite_%' \
           AND name <> 'umbra_migrations' \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await?;

    let mut tables: Vec<IntrospectedTable> = Vec::with_capacity(table_rows.len());
    for row in table_rows {
        let table: String = row.try_get("name")?;
        let columns = introspect_columns(pool, &table).await?;
        tables.push(IntrospectedTable {
            name: pascal_case(&table),
            table,
            columns,
        });
    }

    Ok(IntrospectedSchema { tables })
}

/// Introspect the schema reachable through the given Postgres pool.
/// Reads `information_schema.tables` for table names,
/// `information_schema.columns` for column descriptors, and joins
/// `information_schema.table_constraints` + `key_column_usage` for
/// the primary-key flag. Scoped to the `public` schema by default;
/// internal Postgres schemas and umbra's own `umbra_migrations`
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
           AND table_name <> 'umbra_migrations' \
         ORDER BY table_name",
    )
    .fetch_all(pool)
    .await?;

    let mut tables: Vec<IntrospectedTable> = Vec::with_capacity(table_rows.len());
    for (table,) in table_rows {
        let columns = introspect_columns_pg(pool, &table).await?;
        tables.push(IntrospectedTable {
            name: pascal_case(&table),
            table,
            columns,
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
/// umbra `SqlType` catalogue. Anything unmapped surfaces as
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
    let column_rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT column_name, data_type, is_nullable, udt_name \
         FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = $1 \
         ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(pool)
    .await?;

    let mut columns: Vec<IntrospectedColumn> = Vec::with_capacity(column_rows.len());
    for (name, data_type, is_nullable, udt_name) in column_rows {
        let ty = if data_type.eq_ignore_ascii_case("ARRAY") {
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
        columns.push(IntrospectedColumn {
            name,
            ty,
            primary_key,
            nullable,
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
/// stem to the umbra `ArrayElement` catalogue.
///
/// Returns `None` if the element type isn't in
/// `umbra::orm::ArrayElement` — chrono types, JSON, network types,
/// and Postgres-specific types like NUMERIC fall outside Phase 4.1's
/// array catalogue.
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
/// umbra `SqlType` catalogue. Postgres normalises the strings, so the
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
        // umbra's Text variant.
        "text" | "character varying" | "character" => Some(SqlType::Text),
        "date" => Some(SqlType::Date),
        // Both timezone variants of TIME land on umbra's Time. The
        // distinction is preserved in the database; the client-side
        // type system doesn't model it yet.
        "time without time zone" | "time with time zone" => Some(SqlType::Time),
        // Likewise both timezone variants of TIMESTAMP land on
        // Timestamptz. The umbra catalogue picks the with-tz variant
        // as the default so chrono::DateTime<Utc> is the natural Rust
        // type for either.
        "timestamp without time zone" | "timestamp with time zone" => Some(SqlType::Timestamptz),
        "uuid" => Some(SqlType::Uuid),
        // Both `json` and `jsonb` round-trip to umbra's portable Json
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
        "tsvector" => Some(SqlType::FullText),
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
    let sql = format!("PRAGMA table_info(\"{}\")", table.replace('"', "\"\""));
    let mut rows = sqlx::query(&sql).fetch_all(pool).await?;
    rows.sort_by_key(|r| r.try_get::<i64, _>("cid").unwrap_or(0));

    let mut columns: Vec<IntrospectedColumn> = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.try_get("name")?;
        let raw_type: String = row.try_get("type")?;
        let notnull: i64 = row.try_get("notnull")?;
        let pk: i64 = row.try_get("pk")?;
        let ty = map_sqlite_type(&raw_type).ok_or_else(|| InspectError::UnsupportedColumnType {
            table: table.to_string(),
            column: name.clone(),
            sql_type: raw_type.clone(),
        })?;
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
        columns.push(IntrospectedColumn {
            name,
            ty,
            primary_key,
            nullable,
        });
    }
    Ok(columns)
}

/// Map a raw SQLite type string to the M6 v1 [`SqlType`] catalogue.
/// Case-insensitive; trailing `(n)` or `(p,s)` width parameters are
/// stripped before matching so `VARCHAR(255)` and `NUMERIC(10,2)` come
/// through as `varchar` and `numeric`. Returns `None` on anything not
/// in the table; the caller turns that into
/// [`InspectError::UnsupportedColumnType`] with the table and column
/// names attached.
fn map_sqlite_type(raw: &str) -> Option<SqlType> {
    let head = match raw.split_once('(') {
        Some((before, _)) => before,
        None => raw,
    };
    let normalised = head.trim().to_ascii_lowercase();
    match normalised.as_str() {
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
        "blob" | "bytea" => Some(SqlType::Bytes),
        _ => None,
    }
}

/// Convert a SQL identifier (typically a table name) into UpperCamelCase
/// for use as a Rust struct name. Splits on `_`, ` `, and `-`, takes the
/// alphanumeric remainder, and uppercases the first character of each
/// segment. `blog_post` becomes `BlogPost`; `auth_user_groups` becomes
/// `AuthUserGroups`. Empty input returns the empty string; the renderer
/// upstream guarantees a non-empty table name.
/// Mirror of `umbra_macros::to_snake_case`. The M3 derive computes a
/// model's `TABLE` const as snake_case of the struct name; the
/// renderer uses this helper to decide whether the source SQL table
/// name round-trips through the derive (so the `#[umbra(table = ...)]`
/// attribute can be omitted). Kept identical to the derive's body so
/// the two agree byte-for-byte.
fn derive_table_name(camel: &str) -> String {
    let chars: Vec<char> = camel.chars().collect();
    let mut out = String::with_capacity(camel.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let prev = if i == 0 { None } else { Some(chars[i - 1]) };
            let next = chars.get(i + 1).copied();
            let prev_lower_or_digit =
                matches!(prev, Some(p) if p.is_ascii_lowercase() || p.is_ascii_digit());
            let run_break = prev.map(|p| p.is_ascii_uppercase()).unwrap_or(false)
                && matches!(next, Some(n) if n.is_ascii_lowercase());
            if i != 0 && (prev_lower_or_digit || run_break) {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn pascal_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut upper_next = true;
    for ch in input.chars() {
        if ch == '_' || ch == ' ' || ch == '-' {
            upper_next = true;
            continue;
        }
        if !ch.is_alphanumeric() {
            continue;
        }
        if upper_next {
            for u in ch.to_uppercase() {
                out.push(u);
            }
            upper_next = false;
        } else {
            for l in ch.to_lowercase() {
                out.push(l);
            }
        }
    }
    out
}

/// Render the introspected schema as the contents of a `models.rs`
/// file. The output is one `#[derive(Model)]` struct per table, with
/// fields in declaration order and the `#[umbra(table = "…")]`
/// attribute set when the struct name differs from the SQL table.
///
/// Structs are emitted in alphabetical order by struct name so a
/// re-run against an unchanged schema produces a byte-identical file.
/// Field-type rendering uses fully-qualified `chrono::*` / `uuid::*`
/// paths so no extra `use` lines are needed at the top of the file.
pub fn render_models(schema: &IntrospectedSchema) -> String {
    let mut out = String::new();
    out.push_str(HEADER);

    let mut tables: Vec<&IntrospectedTable> = schema.tables.iter().collect();
    tables.sort_by(|a, b| a.name.cmp(&b.name));

    for table in tables {
        out.push('\n');
        out.push_str(&render_one_struct(table));
    }
    out
}

/// Two-line module doc plus the single facade import every generated
/// file needs. Kept as a constant so the empty-schema path emits
/// exactly the header and nothing else.
const HEADER: &str = "\
//! Generated by `umbra inspectdb`. Wire each struct into your App
//! builder with `.model::<StructName>()`. Re-run `inspectdb` to
//! regenerate; edits made by hand will be lost.

use umbra::prelude::*;
";

/// Render a single `#[derive(Model)]` struct for one introspected table.
/// The `#[umbra(table = "...")]` attribute is emitted only when the
/// derive's auto-derived table name (snake_case of the struct name)
/// doesn't equal the SQL table name. For the typical Django shape
/// (`blog_post` -> `BlogPost` -> derive computes `"blog_post"`), the
/// attribute is redundant and is left off. For unusual SQL casings
/// (`POSTS` -> `Posts` -> derive computes `"posts"` not `"POSTS"`),
/// the attribute is emitted and the M3.1 derive picks it up to
/// override the default. See `umbra-macros/src/lib.rs` for the
/// attribute parser.
fn render_one_struct(table: &IntrospectedTable) -> String {
    let mut out = String::new();
    // `sqlx::FromRow` is required because the `Model` trait bounds it
    // as a supertrait (see `crates/umbra-core/src/orm/model.rs`).
    // Without it, `#[derive(Model)]` emits an `impl Model` whose
    // sqlx::FromRow supertrait isn't satisfied and the generated file
    // fails to compile.
    out.push_str("#[derive(Debug, Clone, sqlx::FromRow, Model)]\n");
    if derive_table_name(&table.name) != table.table {
        out.push_str(&format!("#[umbra(table = \"{}\")]\n", table.table));
    }
    out.push_str(&format!("pub struct {} {{\n", table.name));
    for column in &table.columns {
        out.push_str(&format!(
            "    pub {}: {},\n",
            column.name,
            render_field_type(column.ty, column.nullable),
        ));
    }
    out.push_str("}\n");
    out
}

/// Map `(SqlType, nullable)` to the Rust type string the derive macro's
/// `classify_field_type` accepts. Mirrors the table in
/// `umbra-macros/src/lib.rs` (see `FieldKind` for the full catalogue).
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
        SqlType::FullText => "umbra::orm::TsVector".to_string(),
        // ForeignKey inspectdb renders as i64 for now; the FK relationship
        // introspection that would emit ForeignKey<T> is deferred.
        SqlType::ForeignKey => "i64".to_string(),
        // BLOB / BYTEA columns surface as Vec<u8> in user code.
        SqlType::Bytes => "Vec<u8>".to_string(),
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
                Operation::DropTable { .. }
                | Operation::AddColumn { .. }
                | Operation::DropColumn { .. }
                | Operation::AlterColumn { .. }
                | Operation::RenameTable { .. } => (t, c),
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
            fk_target: None,
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: Vec::new(),
            choice_labels: Vec::new(),
            default: String::new(),
            is_multichoice: false,
            // inspectdb does not introspect UNIQUE constraints yet
            // (gap #65 ships the declare-side first; inspect-side
            // lands when there's a real porting case that needs it).
            unique: false,
            on_delete: crate::orm::FkAction::NoAction,
            on_update: crate::orm::FkAction::NoAction,
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
            }],
        };
        let out = render_models(&schema);
        // `BlogPost` snake_cases to `blog_post` via the derive, so the
        // attribute is redundant and is left off. This keeps the
        // generated file compatible with the M3 derive, which doesn't
        // yet recognise `#[umbra(...)]` attributes.
        assert!(!out.contains("#[umbra(table"));
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
            }],
        };
        let out = render_models(&schema);
        assert!(!out.contains("#[umbra(table"));
        assert!(out.contains("pub struct Post {"));
    }

    #[test]
    fn non_round_tripping_table_name_keeps_attribute() {
        // SQL tables with names the derive's snake_case won't reach
        // (e.g. uppercase, runs of capitals, leading digits) need the
        // explicit attribute. This case is rare in Django ports but
        // the renderer should still cover it for the derive's eventual
        // attribute-support landing.
        let schema = IntrospectedSchema {
            tables: vec![IntrospectedTable {
                table: "POSTS".to_string(),
                name: "Posts".to_string(),
                columns: vec![col("id", SqlType::BigInt, true, false)],
            }],
        };
        let out = render_models(&schema);
        assert!(out.contains("#[umbra(table = \"POSTS\")]"));
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
                },
                IntrospectedTable {
                    table: "antelope".to_string(),
                    name: "Antelope".to_string(),
                    columns: vec![col("id", SqlType::BigInt, true, false)],
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
        assert!(out.contains("Generated by `umbra inspectdb`"));
        assert!(out.contains("edits made by hand will be lost"));
        assert!(out.contains("use umbra::prelude::*;"));
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
    }

    /// Postgres-specific types umbra doesn't model yet surface as
    /// `None` so the caller produces `UnsupportedColumnType` with the
    /// raw type string preserved. The catalogue lookups most likely to
    /// bite a Django port: numeric, bytea, arrays, network types. The
    /// user fixes by hand or waits for the catalogue to grow.
    ///
    /// Note `json`/`jsonb` are NOT on this list — Phase 4's `Json`
    /// SqlType variant maps both back to `SqlType::Json`. Likewise
    /// `inet`/`cidr`/`macaddr` left this list when Phase 4.4 added
    /// the matching SqlType variants. The companion arms in
    /// `map_postgres_type` are covered by
    /// `map_postgres_type_covers_the_full_catalogue` above.
    #[test]
    fn map_postgres_type_returns_none_for_postgres_only_types() {
        assert_eq!(map_postgres_type("numeric"), None);
        // `bytea` USED to be off-catalogue and returned None; once
        // SqlType::Bytes shipped, `bytea` started routing to it.
        // Asserted in the positive `map_postgres_type_covers_the_full_catalogue`
        // test instead.
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
