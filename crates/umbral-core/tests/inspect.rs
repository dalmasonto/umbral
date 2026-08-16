//! End-to-end coverage for the M6 `inspectdb` pipeline: introspect a
//! SQLite database, render a `models.rs` plus an initial migration
//! JSON, and (optionally) record the result against
//! `umbral_migrations`.
//!
//! Two pool strategies live side by side. The introspect-only tests
//! (cases 1–3) open their own private `sqlx::SqlitePool` and call
//! [`umbral::inspect::introspect_pool`] directly; they never touch the
//! ambient pool the framework publishes, so each runs in isolation
//! regardless of test order. The end-to-end tests (cases 5–6) drive
//! the public [`umbral::inspect::inspectdb`] entry point, which reads
//! the process-wide pool, so they share a `OnceCell`-driven
//! `SEEDED` initialiser that boots `App::build()` once and seeds the
//! ambient pool with the `post` / `tag` fixture tables exactly once.
//!
//! Mirrors `tests/migrate.rs` in shape: shared boot via a `OnceCell`,
//! `tempfile::tempdir()` for per-test filesystem isolation, raw SQL
//! for fixture seeding so the inspect coverage is decoupled from any
//! change to the M5 migrate pipeline.
//!
//! See `crates/umbral-core/src/inspect.rs` for the surface this
//! exercises and `docs/specs/07-inspectdb.md` for the M6 v1 scope.

use std::path::PathBuf;

use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::OnceCell;

use umbral::inspect::{
    INITIAL_MIGRATION_ID, INSPECTED_PLUGIN_NAME, InspectOptions, IntrospectedColumn,
    IntrospectedSchema, IntrospectedTable, inspectdb, introspect_pool, render_models,
};
use umbral::migrate::{MigrationFile, Operation};
use umbral::orm::{Post, SqlType};

// --------------------------------------------------------------------- //
// Shared App boot. App::build() writes the pool, the model registry,    //
// the active backend, and the settings into process-wide OnceLocks, so  //
// we can only run it once per test binary.                              //
// --------------------------------------------------------------------- //

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite should always connect");

        umbral::App::builder()
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
            let pool = umbral::db::pool();
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
// Case 3. The skip list: `sqlite_*` and `umbral_migrations` never show  //
// up in the introspected schema.                                       //
// --------------------------------------------------------------------- //

/// Seed one user table and the umbral tracking table; assert neither
/// internal table appears in the result.
#[tokio::test]
async fn introspect_pool_skips_sqlite_internals_and_umbral_migrations() {
    let pool = fresh_pool().await;

    sqlx::query("CREATE TABLE widget (id INTEGER PRIMARY KEY, label TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("seed `widget` should succeed");

    // `umbral_migrations` matches the layout the M5 engine uses. We
    // CREATE it by hand instead of running migrate's private
    // `ensure_tracking_table` so the inspect tests don't pull a
    // dependency on the migrate module's internals.
    sqlx::query(
        "CREATE TABLE umbral_migrations (
            plugin TEXT NOT NULL,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL,
            snapshot_hash TEXT NOT NULL,
            PRIMARY KEY (plugin, name)
        )",
    )
    .execute(&pool)
    .await
    .expect("seed `umbral_migrations` should succeed");

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
        !names.contains(&"umbral_migrations"),
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
/// and OMIT the `#[umbral(table = "...")]` attribute in both cases
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
                    fk_target: None,
                    unique: false,
                    index: false,
                    default: None,
                    auto_now_add: false,
                    auto_now: false,
                }],

                unique_together: Vec::new(),
                indexes: Vec::new(),
            },
            IntrospectedTable {
                table: "blog_post".to_string(),
                name: "BlogPost".to_string(),
                columns: vec![IntrospectedColumn {
                    name: "id".to_string(),
                    ty: SqlType::BigInt,
                    primary_key: true,
                    nullable: false,
                    fk_target: None,
                    unique: false,
                    index: false,
                    default: None,
                    auto_now_add: false,
                    auto_now: false,
                }],

                unique_together: Vec::new(),
                indexes: Vec::new(),
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
        !out.contains("#[umbral(table"),
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
        source: None,
        framework: None,
        with_table_names: false,
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
/// migration in `umbral_migrations` and `show_in` against the produced
/// migrations dir reports zero pending.
#[tokio::test]
async fn inspectdb_with_mark_applied_records_the_initial_migration() {
    seeded_ambient_pool().await;
    let tmp: TempDir = tempfile::tempdir().expect("create tempdir");

    let opts = InspectOptions {
        source: None,
        framework: None,
        with_table_names: false,
        output: tmp.path().to_path_buf(),
        mark_applied: true,
    };
    let _report = inspectdb(opts).await.expect("inspectdb should succeed");

    // One row in `umbral_migrations` keyed by (app, 0001_initial).
    let pool = umbral::db::pool();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT plugin, name FROM umbral_migrations WHERE plugin = ? AND name = ?")
            .bind(INSPECTED_PLUGIN_NAME)
            .bind(INITIAL_MIGRATION_ID)
            .fetch_all(&pool)
            .await
            .expect("select from umbral_migrations should succeed");
    assert_eq!(
        rows.len(),
        1,
        "exactly one row for ({INSPECTED_PLUGIN_NAME}, {INITIAL_MIGRATION_ID}); got {rows:?}",
    );

    // The migration lives under `<tmp>/migrations/<plugin>/0001_initial.json`,
    // which is the layout `show_in` reads when handed `<tmp>/migrations`.
    let migrations_root: PathBuf = tmp.path().join("migrations");
    let pending = umbral::migrate::show_in(&migrations_root)
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
                    fk_target: None,
                    unique: false,
                    index: false,
                    default: None,
                    auto_now_add: false,
                    auto_now: false,
                },
                IntrospectedColumn {
                    name: "body".to_string(),
                    ty: SqlType::Text,
                    primary_key: false,
                    nullable: true,
                    fk_target: None,
                    unique: false,
                    index: false,
                    default: None,
                    auto_now_add: false,
                    auto_now: false,
                },
            ],

            unique_together: Vec::new(),
            indexes: Vec::new(),
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

/// `inspectdb` with an explicit `source` URL introspects THAT database, not the
/// ambient pool — the `umbral inspectdb <db>` onboarding path. The ambient pool
/// (booted with `post`) is deliberately different from the source file's single
/// `widget` table, so a source that is honoured produces `Widget`, never `Post`.
#[tokio::test]
async fn inspectdb_honors_an_explicit_source_database() {
    boot().await; // publishes the ambient `post` pool — which we must NOT read.

    // A standalone SQLite file with one table the ambient pool doesn't have.
    let src_dir = TempDir::new().expect("temp dir for the source db");
    let src_path = src_dir.path().join("foreign.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", src_path.display());
    let src_pool = umbral::db::connect_sqlite(&url)
        .await
        .expect("open the source file db");
    sqlx::query("CREATE TABLE widget (id INTEGER PRIMARY KEY, label TEXT NOT NULL)")
        .execute(&src_pool)
        .await
        .expect("seed the source schema");

    let out_dir = TempDir::new().expect("temp dir for output");
    let opts = InspectOptions {
        source: Some(url.clone()),
        framework: None,
        with_table_names: false,
        output: out_dir.path().to_path_buf(),
        mark_applied: false,
    };
    let report = inspectdb(opts)
        .await
        .expect("inspectdb against a source db");

    let models = std::fs::read_to_string(out_dir.path().join("models.rs")).expect("models.rs");
    assert!(
        models.contains("pub struct Widget"),
        "must introspect the SOURCE db's `widget` table; got:\n{models}"
    );
    assert!(
        !models.contains("pub struct Post"),
        "must NOT read the ambient `post` pool when a source is given; got:\n{models}"
    );
    assert_eq!(report.tables, 1, "the source db has exactly one table");
}

/// `inspectdb` recovers foreign keys (rendered as `ForeignKey<Target>`) and
/// single-column UNIQUE / index constraints (`#[umbral(unique)]` /
/// `#[umbral(index)]`) from a SQLite source — the "deeper" introspection.
#[tokio::test]
async fn inspectdb_recovers_foreign_keys_and_indexes() {
    boot().await;
    let src_dir = TempDir::new().expect("temp dir for the source db");
    let src_path = src_dir.path().join("blog.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", src_path.display());
    let src = umbral::db::connect_sqlite(&url)
        .await
        .expect("open source db");
    // A parent table and a child with an FK, a unique column, and an indexed one.
    for stmt in [
        "CREATE TABLE author (id INTEGER PRIMARY KEY, email TEXT NOT NULL)",
        "CREATE TABLE post (\
            id INTEGER PRIMARY KEY, \
            title TEXT NOT NULL, \
            slug TEXT NOT NULL, \
            views INTEGER NOT NULL, \
            author_id INTEGER NOT NULL REFERENCES author(id))",
        "CREATE UNIQUE INDEX post_slug_uniq ON post(slug)",
        "CREATE INDEX post_views_idx ON post(views)",
    ] {
        sqlx::query(stmt)
            .execute(&src)
            .await
            .expect("seed source schema");
    }

    let out_dir = TempDir::new().expect("output dir");
    let opts = InspectOptions {
        source: Some(url),
        framework: None,
        with_table_names: false,
        output: out_dir.path().to_path_buf(),
        mark_applied: false,
    };
    inspectdb(opts).await.expect("inspectdb");
    let models = std::fs::read_to_string(out_dir.path().join("models.rs")).expect("models.rs");

    // The FK column renders as ForeignKey<Author> (full column name, no
    // framework prettification), pointing at the parent's generated struct.
    assert!(
        models.contains("pub author_id: ForeignKey<Author>"),
        "FK column must render as ForeignKey<Author>; got:\n{models}"
    );
    // The unique + indexed columns carry their attributes.
    assert!(
        models.contains("#[umbral(unique)]"),
        "single-column unique index must render #[umbral(unique)]; got:\n{models}"
    );
    assert!(
        models.contains("#[umbral(index)]"),
        "single-column index must render #[umbral(index)]; got:\n{models}"
    );
}

/// `inspectdb --framework django` prettifies FK columns: `author_id` becomes a
/// clean `author` field bound to the real column via `#[sqlx(rename)]`, and a
/// FK fields keep their REAL column name (no `#[sqlx(rename)]`) — umbral uses
/// the field name as the column name, so the index/field reads against the
/// actual column. Only the FK *target struct* is app-prefix-stripped.
#[tokio::test]
async fn inspectdb_django_fk_keeps_real_column_name_target_stripped() {
    use umbral::inspect::Framework;
    boot().await;
    let src_dir = TempDir::new().expect("temp dir");
    let src_path = src_dir.path().join("django.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", src_path.display());
    let src = umbral::db::connect_sqlite(&url).await.expect("open db");
    for stmt in [
        "CREATE TABLE blog_category (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE blog_author (id INTEGER PRIMARY KEY, email TEXT NOT NULL)",
        "CREATE TABLE blog_post (\
            id INTEGER PRIMARY KEY, \
            title TEXT NOT NULL, \
            author_id INTEGER NOT NULL REFERENCES blog_author(id), \
            blog_category_id INTEGER REFERENCES blog_category(id))",
    ] {
        sqlx::query(stmt).execute(&src).await.expect("seed");
    }

    let out_dir = TempDir::new().expect("out dir");
    let opts = InspectOptions {
        source: Some(url),
        framework: Some(Framework::Django),
        with_table_names: true,
        output: out_dir.path().to_path_buf(),
        mark_applied: false,
    };
    inspectdb(opts).await.expect("inspectdb --framework django");
    let models = std::fs::read_to_string(out_dir.path().join("models.rs")).expect("models.rs");

    // FK field keeps its real column name `author_id`; the TARGET struct is
    // app-prefix-stripped (blog_author -> Author). No rename.
    assert!(
        models.contains("pub author_id: ForeignKey<Author>"),
        "FK field must keep the real column name -> ForeignKey<Author>; got:\n{models}"
    );
    assert!(
        models.contains("pub blog_category_id: Option<ForeignKey<Category>>"),
        "FK field `blog_category_id` keeps its name -> ForeignKey<Category>; got:\n{models}"
    );
    // No sqlx rename on foreign keys.
    assert!(
        !models.contains("#[sqlx(rename = \"author_id\")]")
            && !models.contains("#[sqlx(rename = \"blog_category_id\")]"),
        "foreign keys must NOT carry #[sqlx(rename)]; got:\n{models}"
    );
}

/// Regression: the generated models must compile against the real `Model`
/// trait, which surfaced three bugs on a real Django schema — missing serde
/// derives, a PK not named `id` left unmarked, and a Rust-keyword column name.
#[tokio::test]
async fn inspectdb_generates_compilable_models_for_edge_cases() {
    boot().await;
    let src_dir = TempDir::new().expect("temp dir");
    let src_path = src_dir.path().join("edge.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", src_path.display());
    let src = umbral::db::connect_sqlite(&url).await.expect("open db");
    for stmt in [
        // PK not named `id` (Django's authtoken_token.key).
        "CREATE TABLE authtoken_token (key TEXT PRIMARY KEY, name TEXT NOT NULL)",
        // A column that is a Rust keyword.
        "CREATE TABLE main_media (id INTEGER PRIMARY KEY, type TEXT NOT NULL)",
    ] {
        sqlx::query(stmt).execute(&src).await.expect("seed");
    }

    let out_dir = TempDir::new().expect("out");
    let opts = InspectOptions {
        source: Some(url),
        framework: None,
        with_table_names: false,
        output: out_dir.path().to_path_buf(),
        mark_applied: false,
    };
    inspectdb(opts).await.expect("inspectdb");
    let m = std::fs::read_to_string(out_dir.path().join("models.rs")).expect("models.rs");

    // 1. serde derives are present (Model requires DeserializeOwned).
    assert!(
        m.contains("serde::Serialize, serde::Deserialize"),
        "generated derive must include serde; got:\n{m}"
    );
    // 2. a PK not named `id` is marked.
    assert!(
        m.contains("#[umbral(primary_key)]") && m.contains("pub key: String"),
        "a non-`id` PK must be marked #[umbral(primary_key)]; got:\n{m}"
    );
    // 3. a Rust-keyword column is escaped + sqlx-renamed.
    assert!(
        m.contains("#[sqlx(rename = \"type\")]") && m.contains("pub type_: String"),
        "a keyword column `type` must become `type_` bound to `type`; got:\n{m}"
    );
}

/// `--framework django` strips the `<app>_` prefix off struct names, falls back
/// to full names on a collision, skips `auth_user` (mapping FKs to umbral's
/// `AuthUser` with a swap-in import), and keeps `#[umbral(table)]`.
#[tokio::test]
async fn inspectdb_django_strips_app_prefix_and_externalizes_auth_user() {
    use umbral::inspect::Framework;
    boot().await;
    let src_dir = TempDir::new().expect("temp dir");
    let src_path = src_dir.path().join("apps.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", src_path.display());
    let src = umbral::db::connect_sqlite(&url).await.expect("open db");
    for stmt in [
        "CREATE TABLE auth_user (id INTEGER PRIMARY KEY, username TEXT NOT NULL)",
        "CREATE TABLE blog_post (id INTEGER PRIMARY KEY, author_id INTEGER REFERENCES auth_user(id))",
        // A name shared across two apps → collision → both keep full names.
        "CREATE TABLE blog_category (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE store_category (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
    ] {
        sqlx::query(stmt).execute(&src).await.expect("seed");
    }

    let out_dir = TempDir::new().expect("out");
    let opts = InspectOptions {
        source: Some(url),
        framework: Some(Framework::Django),
        with_table_names: true,
        output: out_dir.path().to_path_buf(),
        mark_applied: false,
    };
    inspectdb(opts).await.expect("inspectdb");
    let m = std::fs::read_to_string(out_dir.path().join("models.rs")).expect("models.rs");

    // App prefix stripped: blog_post -> Post (with the real table preserved).
    assert!(
        m.contains("#[umbral(table = \"blog_post\")]") && m.contains("pub struct Post {"),
        "blog_post must become struct Post; got:\n{m}"
    );
    // Collision → full names kept for both `category` tables.
    assert!(
        m.contains("pub struct BlogCategory {") && m.contains("pub struct StoreCategory {"),
        "colliding `category` tables must keep full names; got:\n{m}"
    );
    // auth_user is NOT re-declared, is imported, and FKs point at AuthUser.
    assert!(
        !m.contains("pub struct AuthUser {"),
        "auth_user must not be re-declared; got:\n{m}"
    );
    assert!(
        m.contains("use umbral_auth::AuthUser;"),
        "the auth_user import must be emitted; got:\n{m}"
    );
    assert!(
        m.contains("ForeignKey<AuthUser>"),
        "a FK to auth_user must point at AuthUser; got:\n{m}"
    );
}

/// `--framework django` WITHOUT `--with-table-names` keeps full, round-tripping
/// struct names (`blog_post` -> `BlogPost`) and emits NO `#[umbral(table)]`
/// macro — the noise the flag gates. Django's other conventions (auth_user
/// externalization) still apply; `--with-table-names` is what opts into the
/// app-prefix stripping and the table macro it necessitates.
#[tokio::test]
async fn inspectdb_django_without_table_names_keeps_full_struct_names() {
    use umbral::inspect::Framework;
    boot().await;
    let src_dir = TempDir::new().expect("temp dir");
    let src_path = src_dir.path().join("apps.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", src_path.display());
    let src = umbral::db::connect_sqlite(&url).await.expect("open db");
    for stmt in [
        "CREATE TABLE auth_user (id INTEGER PRIMARY KEY, username TEXT NOT NULL)",
        "CREATE TABLE blog_post (id INTEGER PRIMARY KEY, author_id INTEGER REFERENCES auth_user(id))",
    ] {
        sqlx::query(stmt).execute(&src).await.expect("seed");
    }

    let out_dir = TempDir::new().expect("out");
    let opts = InspectOptions {
        source: Some(url),
        framework: Some(Framework::Django),
        with_table_names: false,
        output: out_dir.path().to_path_buf(),
        mark_applied: false,
    };
    inspectdb(opts).await.expect("inspectdb");
    let m = std::fs::read_to_string(out_dir.path().join("models.rs")).expect("models.rs");

    // Full struct name kept, and NO table macro (the name round-trips).
    assert!(
        m.contains("pub struct BlogPost {"),
        "without --with-table-names the full struct name is kept; got:\n{m}"
    );
    assert!(
        !m.contains("#[umbral(table = \"blog_post\")]"),
        "no table macro when the struct name round-trips; got:\n{m}"
    );
    // Django's auth_user externalization is independent of --with-table-names.
    assert!(
        m.contains("use umbral_auth::AuthUser;") && m.contains("ForeignKey<AuthUser>"),
        "auth_user must still be externalized under --framework django; got:\n{m}"
    );
}

/// A recovered PostGIS geometry column renders as `umbral::orm::gis::Geometry`
/// with the `#[umbral(geometry = "...", srid = N)]` attribute carrying the
/// subtype + SRID (so a re-migrate rebuilds `geometry(Point, 4326)`).
#[test]
fn render_geometry_column_emits_subtype_srid_attribute() {
    use umbral::inspect::{
        IntrospectedColumn, IntrospectedSchema, IntrospectedTable, render_models,
    };
    use umbral::orm::{GeometryKind, GeometrySpec, SqlType};

    let schema = IntrospectedSchema {
        tables: vec![IntrospectedTable {
            table: "facility".into(),
            name: "Facility".into(),
            columns: vec![
                IntrospectedColumn {
                    name: "id".into(),
                    ty: SqlType::BigInt,
                    primary_key: true,
                    nullable: false,
                    fk_target: None,
                    unique: false,
                    index: false,
                    default: None,
                    auto_now_add: false,
                    auto_now: false,
                },
                IntrospectedColumn {
                    name: "location".into(),
                    ty: SqlType::Geometry(GeometrySpec {
                        kind: GeometryKind::Point,
                        srid: 4326,
                    }),
                    primary_key: false,
                    nullable: false,
                    fk_target: None,
                    unique: false,
                    index: false,
                    default: None,
                    auto_now_add: false,
                    auto_now: false,
                },
            ],

            unique_together: Vec::new(),
            indexes: Vec::new(),
        }],
    };
    let out = render_models(&schema);
    assert!(
        out.contains("#[umbral(geometry = \"point\", srid = 4326)]"),
        "must emit the recovered subtype + SRID; got:\n{out}"
    );
    assert!(
        out.contains("pub location: umbral::orm::gis::Geometry"),
        "geometry column renders as the gis::Geometry newtype; got:\n{out}"
    );
}

/// inspectdb recovers MULTI-column indexes: a composite UNIQUE index becomes a
/// struct-level `#[umbral(unique_together = [[...]])]` and a composite plain
/// index `#[umbral(indexes = [[...]])]` (single-column ones stay per-field).
#[tokio::test]
async fn inspectdb_recovers_composite_indexes() {
    boot().await;
    let src_dir = TempDir::new().expect("temp dir");
    let src_path = src_dir.path().join("compound.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", src_path.display());
    let src = umbral::db::connect_sqlite(&url).await.expect("open db");
    for stmt in [
        "CREATE TABLE membership (id INTEGER PRIMARY KEY, org_id INTEGER, user_id INTEGER, role TEXT, joined TEXT)",
        // composite UNIQUE (one user per org) and a composite plain index.
        "CREATE UNIQUE INDEX membership_org_user ON membership(org_id, user_id)",
        "CREATE INDEX membership_role_joined ON membership(role, joined)",
    ] {
        sqlx::query(stmt).execute(&src).await.expect("seed");
    }

    let out_dir = TempDir::new().expect("out");
    let opts = InspectOptions {
        source: Some(url),
        framework: None,
        with_table_names: false,
        output: out_dir.path().to_path_buf(),
        mark_applied: false,
    };
    inspectdb(opts).await.expect("inspectdb");
    let m = std::fs::read_to_string(out_dir.path().join("models.rs")).expect("models.rs");

    assert!(
        m.contains(r#"#[umbral(unique_together = [["org_id", "user_id"]])]"#),
        "composite unique index must render unique_together; got:\n{m}"
    );
    assert!(
        m.contains(r#"#[umbral(indexes = [["role", "joined"]])]"#),
        "composite plain index must render indexes; got:\n{m}"
    );
}

/// inspectdb recovers column DEFAULTS from the database: a `CURRENT_TIMESTAMP`
/// default on a timestamp lifts to `#[umbral(auto_now_add)]` (so a re-migrate
/// re-emits the right per-backend default), while constant defaults become
/// `#[umbral(default = "...")]`. A `nextval(...)` sequence default is dropped
/// (the PK's autoincrement is handled separately). The recovered auto_now_add
/// also lands in the initial migration snapshot.
#[tokio::test]
async fn inspectdb_recovers_defaults_and_auto_now_add() {
    boot().await;
    let src_dir = TempDir::new().expect("temp dir");
    let src_path = src_dir.path().join("defaults.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", src_path.display());
    let src = umbral::db::connect_sqlite(&url).await.expect("open db");
    sqlx::query(
        "CREATE TABLE event (\
            id INTEGER PRIMARY KEY, \
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, \
            status TEXT DEFAULT 'active', \
            qty INTEGER DEFAULT 0)",
    )
    .execute(&src)
    .await
    .expect("seed");

    let out_dir = TempDir::new().expect("out");
    let opts = InspectOptions {
        source: Some(url),
        framework: None,
        with_table_names: false,
        output: out_dir.path().to_path_buf(),
        mark_applied: false,
    };
    inspectdb(opts).await.expect("inspectdb");
    let m = std::fs::read_to_string(out_dir.path().join("models.rs")).expect("models.rs");

    // CURRENT_TIMESTAMP on a timestamp -> auto_now_add (NOT a literal default).
    assert!(
        m.contains("#[umbral(auto_now_add)]") && m.contains("pub created_at"),
        "CURRENT_TIMESTAMP default must lift to auto_now_add; got:\n{m}"
    );
    assert!(
        !m.contains(r#"default = "CURRENT_TIMESTAMP""#),
        "the raw CURRENT_TIMESTAMP must not leak as a literal default; got:\n{m}"
    );
    // Constant defaults: string unquoted, integer verbatim.
    assert!(
        m.contains(r#"#[umbral(default = "active")]"#),
        "string default must render unquoted; got:\n{m}"
    );
    assert!(
        m.contains(r#"#[umbral(default = "0")]"#),
        "integer default must render verbatim; got:\n{m}"
    );

    // The migration snapshot carries the recovered auto_now_add too.
    let mig = std::fs::read_to_string(out_dir.path().join("migrations/app/0001_initial.json"))
        .expect("initial migration");
    assert!(
        mig.contains("auto_now_add"),
        "recovered auto_now_add must reach the initial migration; got:\n{mig}"
    );
}

/// Under `--framework django`, Python-managed timestamps leave no DB default,
/// so they're recovered by name: `created*` -> auto_now_add, `updated*` ->
/// auto_now.
#[tokio::test]
async fn inspectdb_django_recovers_auto_now_from_column_names() {
    use umbral::inspect::Framework;
    boot().await;
    let src_dir = TempDir::new().expect("temp dir");
    let src_path = src_dir.path().join("stamps.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", src_path.display());
    let src = umbral::db::connect_sqlite(&url).await.expect("open db");
    sqlx::query(
        "CREATE TABLE blog_post (\
            id INTEGER PRIMARY KEY, \
            created_at TIMESTAMP, \
            updated_at TIMESTAMP)",
    )
    .execute(&src)
    .await
    .expect("seed");

    let out_dir = TempDir::new().expect("out");
    let opts = InspectOptions {
        source: Some(url),
        framework: Some(Framework::Django),
        with_table_names: false,
        output: out_dir.path().to_path_buf(),
        mark_applied: false,
    };
    inspectdb(opts).await.expect("inspectdb");
    let m = std::fs::read_to_string(out_dir.path().join("models.rs")).expect("models.rs");

    assert!(
        m.contains("#[umbral(auto_now_add)]\n    pub created_at"),
        "django `created_at` must become auto_now_add; got:\n{m}"
    );
    assert!(
        m.contains("#[umbral(auto_now)]\n    pub updated_at"),
        "django `updated_at` must become auto_now; got:\n{m}"
    );
}
