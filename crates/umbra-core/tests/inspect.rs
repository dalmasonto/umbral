//! End-to-end coverage for the M6 `inspectdb` pipeline: introspect a
//! SQLite database, render a `models.rs` plus an initial migration
//! JSON, and (optionally) record the result against
//! `umbra_migrations`.
//!
//! Two pool strategies live side by side. The introspect-only tests
//! (cases 1–3) open their own private `sqlx::SqlitePool` and call
//! [`umbra::inspect::introspect_pool`] directly; they never touch the
//! ambient pool the framework publishes, so each runs in isolation
//! regardless of test order. The end-to-end tests (cases 5–6) drive
//! the public [`umbra::inspect::inspectdb`] entry point, which reads
//! the process-wide pool, so they share a `OnceCell`-driven
//! `SEEDED` initialiser that boots `App::build()` once and seeds the
//! ambient pool with the `post` / `tag` fixture tables exactly once.
//!
//! Mirrors `tests/migrate.rs` in shape: shared boot via a `OnceCell`,
//! `tempfile::tempdir()` for per-test filesystem isolation, raw SQL
//! for fixture seeding so the inspect coverage is decoupled from any
//! change to the M5 migrate pipeline.
//!
//! See `crates/umbra-core/src/inspect.rs` for the surface this
//! exercises and `docs/specs/07-inspectdb.md` for the M6 v1 scope.

use std::path::PathBuf;

use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::OnceCell;

use umbra::inspect::{
    INITIAL_MIGRATION_ID, INSPECTED_PLUGIN_NAME, InspectOptions, IntrospectedColumn,
    IntrospectedSchema, IntrospectedTable, inspectdb, introspect_pool, render_models,
};
use umbra::migrate::{MigrationFile, Operation};
use umbra::orm::{Post, SqlType};

// --------------------------------------------------------------------- //
// Shared App boot. App::build() writes the pool, the model registry,    //
// the active backend, and the settings into process-wide OnceLocks, so  //
// we can only run it once per test binary.                              //
// --------------------------------------------------------------------- //

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");
        let pool = umbra::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite should always connect");

        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .build()
            .expect("App::build() should succeed on the happy path");
    })
    .await;
}

// --------------------------------------------------------------------- //
// Shared "seeded ambient pool" state. The end-to-end tests both want    //
// the ambient pool populated with the `post` / `tag` fixture tables;    //
// seeding twice would error on "table already exists". One OnceCell    //
// drives the seed exactly once.                                         //
// --------------------------------------------------------------------- //

static SEEDED: OnceCell<()> = OnceCell::const_new();

async fn seeded_ambient_pool() {
    boot().await;
    SEEDED
        .get_or_init(|| async {
            let pool = umbra::db::pool();
            seed_post_and_tag(&pool).await;
        })
        .await;
}

/// The fixture: two tables with the column shapes case #2 pins. Used
/// both by the introspect-only tests (against a private pool) and by
/// the end-to-end tests (against the ambient pool).
async fn seed_post_and_tag(pool: &SqlitePool) {
    sqlx::query(
        "CREATE TABLE post (id INTEGER PRIMARY KEY, title TEXT NOT NULL, published_at TIMESTAMP)",
    )
    .execute(pool)
    .await
    .expect("seed `post` should succeed against a fresh pool");

    sqlx::query("CREATE TABLE tag (id BIGINT PRIMARY KEY, name TEXT NOT NULL, uuid UUID)")
        .execute(pool)
        .await
        .expect("seed `tag` should succeed against a fresh pool");
}

/// Open a private in-memory SQLite pool for the introspect-only tests.
async fn fresh_pool() -> SqlitePool {
    SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect")
}

// --------------------------------------------------------------------- //
// Case 1. NoTables sentinel: a fresh DB has nothing to introspect.      //
// --------------------------------------------------------------------- //

/// `introspect_pool` against a freshly opened in-memory pool returns an
/// empty `IntrospectedSchema`. That empty result is precisely the
/// condition `inspectdb` checks before short-circuiting with
/// `InspectError::NoTables`, so verifying the empty schema here covers
/// the same contract without needing to override the ambient pool.
#[tokio::test]
async fn introspect_pool_returns_empty_schema_on_a_fresh_database() {
    let pool = fresh_pool().await;

    let schema = introspect_pool(&pool)
        .await
        .expect("introspecting an empty pool should succeed");

    assert!(
        schema.tables.is_empty(),
        "fresh DB has no tables; got {:?}",
        schema.tables,
    );
}

// --------------------------------------------------------------------- //
// Case 2. The shape of the introspected schema. Pins type mapping,     //
// nullability, primary keys, and the PascalCase struct-name rule.      //
// --------------------------------------------------------------------- //

/// Seed `post` and `tag` against a private pool and assert the
/// resulting `IntrospectedSchema` matches the M6 v1 type catalogue:
/// `INTEGER` -> `Integer`, `TEXT` -> `Text`, `TIMESTAMP` ->
/// `Timestamptz`, `BIGINT` -> `BigInt`, `UUID` -> `Uuid`. Nullability
/// follows the absence of `NOT NULL`; primary-key membership follows
/// the `PRIMARY KEY` clause.
#[tokio::test]
async fn introspect_pool_maps_types_nullability_and_primary_keys() {
    let pool = fresh_pool().await;
    seed_post_and_tag(&pool).await;

    let schema = introspect_pool(&pool)
        .await
        .expect("introspecting a seeded pool should succeed");

    // Tables come back sorted by name. `post` precedes `tag` lexically.
    assert_eq!(
        schema
            .tables
            .iter()
            .map(|t| t.table.as_str())
            .collect::<Vec<_>>(),
        vec!["post", "tag"],
        "tables should be sorted by name",
    );

    // PascalCase struct names follow `pascal_case(table)`.
    assert_eq!(schema.tables[0].name, "Post");
    assert_eq!(schema.tables[1].name, "Tag");

    // `post`: id INTEGER PK, title TEXT NOT NULL, published_at TIMESTAMP
    let post = &schema.tables[0];
    assert_eq!(post.columns.len(), 3);
    let post_id = &post.columns[0];
    assert_eq!(post_id.name, "id");
    assert_eq!(post_id.ty, SqlType::Integer);
    assert!(post_id.primary_key, "id is the primary key");
    assert!(
        !post_id.nullable,
        "INTEGER PRIMARY KEY is logically non-nullable even though PRAGMA \
         reports notnull = 0 for the ROWID-alias case",
    );
    let post_title = &post.columns[1];
    assert_eq!(post_title.name, "title");
    assert_eq!(post_title.ty, SqlType::Text);
    assert!(!post_title.nullable, "TEXT NOT NULL is not nullable");
    assert!(!post_title.primary_key);
    let post_published_at = &post.columns[2];
    assert_eq!(post_published_at.name, "published_at");
    assert_eq!(post_published_at.ty, SqlType::Timestamptz);
    assert!(
        post_published_at.nullable,
        "TIMESTAMP without NOT NULL is nullable",
    );
    assert!(!post_published_at.primary_key);

    // `tag`: id BIGINT PK, name TEXT NOT NULL, uuid UUID
    let tag = &schema.tables[1];
    assert_eq!(tag.columns.len(), 3);
    let tag_id = &tag.columns[0];
    assert_eq!(tag_id.name, "id");
    assert_eq!(tag_id.ty, SqlType::BigInt);
    assert!(tag_id.primary_key);
    assert!(
        !tag_id.nullable,
        "BIGINT PRIMARY KEY is logically non-nullable"
    );
    let tag_name = &tag.columns[1];
    assert_eq!(tag_name.name, "name");
    assert_eq!(tag_name.ty, SqlType::Text);
    assert!(!tag_name.nullable);
    let tag_uuid = &tag.columns[2];
    assert_eq!(tag_uuid.name, "uuid");
    assert_eq!(tag_uuid.ty, SqlType::Uuid);
    assert!(tag_uuid.nullable, "UUID without NOT NULL is nullable",);
}

// --------------------------------------------------------------------- //
// Case 3. The skip list: `sqlite_*` and `umbra_migrations` never show  //
// up in the introspected schema.                                       //
// --------------------------------------------------------------------- //

/// Seed one user table and the umbra tracking table; assert neither
/// internal table appears in the result.
#[tokio::test]
async fn introspect_pool_skips_sqlite_internals_and_umbra_migrations() {
    let pool = fresh_pool().await;

    sqlx::query("CREATE TABLE widget (id INTEGER PRIMARY KEY, label TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("seed `widget` should succeed");

    // `umbra_migrations` matches the layout the M5 engine uses. We
    // CREATE it by hand instead of running migrate's private
    // `ensure_tracking_table` so the inspect tests don't pull a
    // dependency on the migrate module's internals.
    sqlx::query(
        "CREATE TABLE umbra_migrations (
            plugin TEXT NOT NULL,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL,
            snapshot_hash TEXT NOT NULL,
            PRIMARY KEY (plugin, name)
        )",
    )
    .execute(&pool)
    .await
    .expect("seed `umbra_migrations` should succeed");

    // SQLite auto-creates `sqlite_sequence` when an AUTOINCREMENT
    // column is declared. A standalone INTEGER PRIMARY KEY column
    // (rowid alias) doesn't trigger it, so force the case with an
    // explicit AUTOINCREMENT.
    sqlx::query("CREATE TABLE auto_seq (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("seed `auto_seq` should succeed");

    let schema = introspect_pool(&pool)
        .await
        .expect("introspecting the seeded pool should succeed");

    let names: Vec<&str> = schema.tables.iter().map(|t| t.table.as_str()).collect();
    assert!(
        names.contains(&"widget"),
        "user table `widget` should appear; got {names:?}",
    );
    assert!(
        names.contains(&"auto_seq"),
        "user table `auto_seq` should appear; got {names:?}",
    );
    assert!(
        !names.contains(&"umbra_migrations"),
        "tracking table should be skipped; got {names:?}",
    );
    for name in &names {
        assert!(
            !name.starts_with("sqlite_"),
            "internal sqlite table `{name}` should be skipped",
        );
    }
}

// --------------------------------------------------------------------- //
// Case 4. render_models against a hand-coded schema. Independent of    //
// subagent A's introspection body so the renderer is covered even if   //
// introspection drifts.                                                //
// --------------------------------------------------------------------- //

/// Build an `IntrospectedSchema` by hand with two tables whose
/// PascalCased struct names round-trip cleanly through the derive's
/// snake_case (`post` -> `Post` -> `"post"`, `blog_post` -> `BlogPost`
/// -> `"blog_post"`). The renderer should emit one struct per table
/// and OMIT the `#[umbra(table = "...")]` attribute in both cases
/// since the derive's auto-derived table name already matches.
#[tokio::test]
async fn render_models_omits_table_attribute_when_derive_round_trips() {
    let schema = IntrospectedSchema {
        tables: vec![
            IntrospectedTable {
                table: "post".to_string(),
                name: "Post".to_string(),
                columns: vec![IntrospectedColumn {
                    name: "id".to_string(),
                    ty: SqlType::BigInt,
                    primary_key: true,
                    nullable: false,
                }],
            },
            IntrospectedTable {
                table: "blog_post".to_string(),
                name: "BlogPost".to_string(),
                columns: vec![IntrospectedColumn {
                    name: "id".to_string(),
                    ty: SqlType::BigInt,
                    primary_key: true,
                    nullable: false,
                }],
            },
        ],
    };

    let out = render_models(&schema);

    assert!(
        out.contains("pub struct Post {"),
        "rendered output should declare `pub struct Post`; got:\n{out}",
    );
    assert!(
        out.contains("pub struct BlogPost {"),
        "rendered output should declare `pub struct BlogPost`; got:\n{out}",
    );
    assert!(
        !out.contains("#[umbra(table"),
        "neither struct name needs the attribute (the derive's \
         auto-snake_case of `Post` is `post` and of `BlogPost` is \
         `blog_post`, matching the source tables); the renderer should \
         leave the attribute off so the file compiles against the M3 \
         derive; got:\n{out}",
    );
}

// --------------------------------------------------------------------- //
// Case 5 & 6. End-to-end against the shared ambient pool. Each test    //
// owns its own tempdir for output isolation; the seed runs exactly     //
// once via SEEDED so a second seed wouldn't collide on `table already  //
// exists`.                                                              //
// --------------------------------------------------------------------- //

/// `inspectdb` against the seeded ambient pool writes `models.rs` and
/// the initial migration to the chosen output directory and returns
/// the right counts and paths.
#[tokio::test]
async fn inspectdb_writes_models_and_migration_to_output_directory() {
    seeded_ambient_pool().await;
    let tmp: TempDir = tempfile::tempdir().expect("create tempdir");

    let opts = InspectOptions {
        output: tmp.path().to_path_buf(),
        mark_applied: false,
    };
    let report = inspectdb(opts).await.expect("inspectdb should succeed");

    assert_eq!(report.tables, 2, "post + tag = 2 tables");
    assert_eq!(report.columns, 6, "3 columns each in post + tag = 6");

    let models = std::fs::read_to_string(&report.models_path)
        .expect("models_path should exist after inspectdb");
    assert!(
        models.contains("pub struct Post {"),
        "models.rs should declare struct Post; got:\n{models}",
    );
    assert!(
        models.contains("pub struct Tag {"),
        "models.rs should declare struct Tag; got:\n{models}",
    );

    let migration_text = std::fs::read_to_string(&report.migration_path)
        .expect("migration_path should exist after inspectdb");
    let migration: MigrationFile =
        serde_json::from_str(&migration_text).expect("migration file should parse");
    assert_eq!(migration.id, INITIAL_MIGRATION_ID);
    assert_eq!(migration.plugin, INSPECTED_PLUGIN_NAME);
    assert_eq!(
        migration.operations.len(),
        2,
        "one CreateTable per introspected table",
    );
    let mut tables: Vec<&str> = migration
        .operations
        .iter()
        .map(|op| match op {
            Operation::CreateTable { table, .. } => table.as_str(),
            other => panic!("expected only CreateTable ops, got {other:?}"),
        })
        .collect();
    tables.sort();
    assert_eq!(
        tables,
        vec!["post", "tag"],
        "CreateTable ops should cover both fixture tables",
    );
}

/// `inspectdb` with `mark_applied = true` records the initial
/// migration in `umbra_migrations` and `show_in` against the produced
/// migrations dir reports zero pending.
#[tokio::test]
async fn inspectdb_with_mark_applied_records_the_initial_migration() {
    seeded_ambient_pool().await;
    let tmp: TempDir = tempfile::tempdir().expect("create tempdir");

    let opts = InspectOptions {
        output: tmp.path().to_path_buf(),
        mark_applied: true,
    };
    let _report = inspectdb(opts).await.expect("inspectdb should succeed");

    // One row in `umbra_migrations` keyed by (app, 0001_initial).
    let pool = umbra::db::pool();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT plugin, name FROM umbra_migrations WHERE plugin = ? AND name = ?")
            .bind(INSPECTED_PLUGIN_NAME)
            .bind(INITIAL_MIGRATION_ID)
            .fetch_all(&pool)
            .await
            .expect("select from umbra_migrations should succeed");
    assert_eq!(
        rows.len(),
        1,
        "exactly one row for ({INSPECTED_PLUGIN_NAME}, {INITIAL_MIGRATION_ID}); got {rows:?}",
    );

    // The migration lives under `<tmp>/migrations/<plugin>/0001_initial.json`,
    // which is the layout `show_in` reads when handed `<tmp>/migrations`.
    let migrations_root: PathBuf = tmp.path().join("migrations");
    let pending = umbra::migrate::show_in(&migrations_root)
        .await
        .expect("show_in should succeed");
    assert_eq!(
        pending, 0,
        "0001_initial was marked applied; show_in should report zero pending, got {pending}",
    );
}

/// Regression: `render_models` must emit `sqlx::FromRow` in the derive
/// list and must NOT wrap primary-key column types in `Option<>`.
///
/// Both bugs were found during the M5.1+ end-to-end CLI sweep:
///
/// - The `Model` trait bounds `sqlx::FromRow` as a supertrait, so
///   `#[derive(Debug, Clone, Model)]` alone fails to compile. The
///   renderer must include `sqlx::FromRow` so the generated file
///   builds against the M3 derive without hand-editing.
///
/// - SQLite's `PRAGMA table_info` reports `notnull = 0` for
///   `INTEGER PRIMARY KEY` columns (they're aliases for ROWID, which
///   SQLite manages), but the columns are logically non-nullable. The
///   M3 derive's PK-detection requires a non-`Option` PK field type;
///   wrapping the PK in `Option<T>` made the derive fail.
///
/// `introspect_pool` forces `nullable = false` whenever
/// `primary_key = true`; `render_one_struct` emits the right derive
/// list. This test pins both invariants by string-matching the
/// rendered output.
#[tokio::test]
async fn render_models_emits_fromrow_and_skips_option_on_primary_keys() {
    let schema = IntrospectedSchema {
        tables: vec![IntrospectedTable {
            table: "post".to_string(),
            name: "Post".to_string(),
            columns: vec![
                IntrospectedColumn {
                    name: "id".to_string(),
                    ty: SqlType::BigInt,
                    primary_key: true,
                    nullable: false,
                },
                IntrospectedColumn {
                    name: "body".to_string(),
                    ty: SqlType::Text,
                    primary_key: false,
                    nullable: true,
                },
            ],
        }],
    };

    let out = render_models(&schema);

    assert!(
        out.contains("sqlx::FromRow"),
        "the derive list must include sqlx::FromRow so the generated \
         file compiles against the Model trait's supertrait bound; got:\n{out}",
    );
    assert!(
        out.contains("pub id: i64,"),
        "the primary-key column must render as the bare integer type \
         (the M3 derive requires `id: i32 | i64 | uuid::Uuid`, no Option); got:\n{out}",
    );
    assert!(
        !out.contains("pub id: Option<"),
        "the primary-key column must NEVER be wrapped in Option; got:\n{out}",
    );
    // Sanity: the non-PK nullable column still gets Option.
    assert!(
        out.contains("pub body: Option<String>,"),
        "non-PK nullable columns should still be Option; got:\n{out}",
    );
}
