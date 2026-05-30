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

use sqlx::{Row, SqlitePool};

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
        columns.push(IntrospectedColumn {
            name,
            ty,
            primary_key: pk != 0,
            nullable: notnull == 0,
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
/// attribute is redundant and is left off so the generated code
/// compiles against the M3 derive (which does not yet recognise
/// `#[umbra(...)]` attributes; see `umbra-macros/src/lib.rs` §M3
/// constraints). The attribute lands as derive support grows.
fn render_one_struct(table: &IntrospectedTable) -> String {
    let mut out = String::new();
    out.push_str("#[derive(Debug, Clone, Model)]\n");
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
        SqlType::SmallInt => "i16",
        SqlType::Integer => "i32",
        SqlType::BigInt => "i64",
        SqlType::Real => "f32",
        SqlType::Double => "f64",
        SqlType::Boolean => "bool",
        SqlType::Text => "String",
        SqlType::Date => "chrono::NaiveDate",
        SqlType::Time => "chrono::NaiveTime",
        SqlType::Timestamptz => "chrono::DateTime<chrono::Utc>",
        SqlType::Uuid => "uuid::Uuid",
    };
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
                | Operation::DropColumn { .. } => (t, c),
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
}
