// The local `Comment` model is private but `#[derive(Model)]` emits a `pub
// const` column module that references it. The same pattern that
// `tests/type_catalogue.rs` uses to silence the lint at the file level.
#![allow(dead_code, private_interfaces)]

//! End-to-end coverage for the M5 migration engine: the declare → migrate →
//! change → migrate loop against an in-memory SQLite pool and per-test
//! temp directories.
//!
//! `umbral-core::migrate` keeps its model registry, DB pool, and active
//! backend behind process-wide `OnceLock`s, so every test in this file
//! shares one boot of `App::builder().build()` (registered models: `Post`
//! plus the local `Comment` defined below).
//!
//! Two shapes of test live here. The make-only tests (`make_in_*`, the
//! hand-written-prior tests) each use a private `tempfile::tempdir()` and
//! never touch the shared pool, so they can run in parallel without
//! stepping on one another. The run-side tests share one further
//! `OnceCell<TempDir>` that holds a tempdir into which `make_in` + `run_in`
//! has been driven exactly once; the apply path mutates the shared pool
//! (creating model tables, writing into `umbral_migrations`), so a second
//! `run_in` against a fresh tempdir would collide on "table already
//! exists". Sharing the post-apply state is therefore correct, not a
//! workaround: it's the same loop a real binary sees across CLI
//! invocations.
//!
//! See `crates/umbral-core/src/migrate.rs` for the surface this exercises
//! and `docs/specs/06-migration-engine.md` for the target spec.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::OnceCell;

use umbral::migrate::{
    APP_PLUGIN_NAME, Column, MigrateError, MigrationFile, ModelMeta, Operation, Snapshot, diff,
    make_in, registered_models, run_in, show_in,
};
use umbral::orm::Model;
use umbral_core::orm::{FieldSpec, Post, SqlType};

// --------------------------------------------------------------------- //
// A second, local test model. Two registered models lets us prove:      //
//   - first `make_in` emits one CreateTable per model (not just one)    //
//   - the suffix collapses to `_auto` when ops > 1 (not `_create_<t>`)  //
//   - the hand-written-prior-snapshot diff is deterministic across      //
//     multiple creates (sorted by model name)                           //
// Declared at module scope so the derive's sibling column module can    //
// resolve `super::Comment`, mirroring the type_catalogue.rs convention. //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Comment {
    id: i64,
    body: String,
    posted_at: Option<DateTime<Utc>>,
}

// --------------------------------------------------------------------- //
// One-shot boot. `App::builder().build()` writes the model registry,    //
// the SQLite pool, the active backend, and the settings into their      //
// `OnceLock`s, so we can only call it once per test binary. A           //
// `tokio::sync::OnceCell<()>` serializes any number of `#[tokio::test]` //
// entry points to a single initialisation.                              //
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
            .model::<Comment>()
            .build()
            .expect("App::build() should succeed on the happy path");
    })
    .await;
}

// --------------------------------------------------------------------- //
// Shared "already migrated" tempdir. `run_in` mutates the process-wide  //
// pool (creates model tables, writes umbral_migrations rows), so the     //
// run-side tests share one tempdir whose make+run has already happened. //
// The OnceCell ensures the make+run executes exactly once per binary.   //
// --------------------------------------------------------------------- //

/// Column name added by the M8 AddColumn end-to-end seed (see below).
/// Picked to be unique enough that no other test will collide on it.
const M8_ADD_COLUMN_NAME: &str = "m8_add_column_summary";

/// Migration id (and filename stem) for the same seed.
const M8_ADD_COLUMN_MIGRATION_ID: &str = "0002_add_post_m8_add_column_summary";

/// Demo table the M5.1 AlterColumn end-to-end seed alters.
const M5_1_ALTER_TABLE: &str = "m5_1_alter_demo";

/// Migration id for the M5.1 AlterColumn seed.
const M5_1_ALTER_MIGRATION_ID: &str = "0003_alter_m5_1_alter_demo_note";

static MIGRATED: OnceCell<TempDir> = OnceCell::const_new();

async fn migrated_dir() -> &'static Path {
    let dir = MIGRATED
        .get_or_init(|| async {
            boot().await;
            let tmp = tempfile::tempdir().expect("create shared migrated tempdir");
            make_in(tmp.path())
                .await
                .expect("seed make_in should emit the first migration");
            run_in(tmp.path())
                .await
                .expect("seed run_in should apply the first migration");

            // M8 — also drop a hand-crafted `0002_add_post_*.json` into
            // the same plugin dir and apply it. The end-to-end AddColumn
            // test (`run_in_applies_a_hand_crafted_add_column_migration`)
            // reads the resulting state. Folding the DDL into this seed
            // means every later parallel test sees a stable, post-ALTER
            // schema instead of racing against an in-flight ALTER on the
            // shared pool. `snapshot_after = Snapshot::current()` keeps
            // the existing `make_in_returns_no_changes_*` test happy:
            // the latest snapshot still equals the registry's view.
            let new_col = Column {
                name: M8_ADD_COLUMN_NAME.to_string(),
                ty: SqlType::Text,
                primary_key: false,
                nullable: true,
                fk_target: None,
                noform: false,
                privileged: false,
                db_constraint: true,
                noedit: false,
                auto_user_add: false,
                auto_user: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
                unique: false,
                on_delete: umbral_core::orm::FkAction::NoAction,
                on_update: umbral_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
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
            };
            let file = MigrationFile {
                id: M8_ADD_COLUMN_MIGRATION_ID.to_string(),
                plugin: APP_PLUGIN_NAME.to_string(),
                depends_on: Vec::new(),
                operations: vec![Operation::AddColumn {
                    table: "post".to_string(),
                    column: new_col,
                }],
                snapshot_after: Snapshot::current(),
                replaces: Vec::new(),
            };
            write_prior_migration(tmp.path(), &file);
            let applied = run_in(tmp.path())
                .await
                .expect("seed run_in should apply the M8 AddColumn migration");
            assert_eq!(
                applied, 1,
                "exactly one new migration should apply in the M8 seed; got {applied}",
            );

            // M5.1 — bootstrap a demo table and flip one of its columns
            // from non-nullable to nullable via the AlterColumn op. The
            // demo table is created with raw SQL (no model registered)
            // so the migration engine treats only the AlterColumn op as
            // the schema change, leaving the registry's view of the
            // model set untouched.
            let pool = umbral::db::pool();
            sqlx::query(&format!(
                "CREATE TABLE {M5_1_ALTER_TABLE} (\
                    id INTEGER PRIMARY KEY,\
                    note TEXT NOT NULL\
                 )"
            ))
            .execute(&pool)
            .await
            .expect("seed the M5.1 AlterColumn demo table");
            sqlx::query(&format!(
                "INSERT INTO {M5_1_ALTER_TABLE} (id, note) VALUES (1, 'hello')"
            ))
            .execute(&pool)
            .await
            .expect("seed a row in the M5.1 AlterColumn demo table");

            let new_columns = vec![
                Column {
                    name: "id".to_string(),
                    ty: SqlType::BigInt,
                    primary_key: true,
                    nullable: false,
                    fk_target: None,
                    noform: false,
                    privileged: false,
                    db_constraint: true,
                    noedit: false,
                    auto_user_add: false,
                    auto_user: false,
                    is_string_repr: false,
                    max_length: 0,
                    choices: Vec::new(),
                    choice_labels: Vec::new(),
                    default: String::new(),
                    is_multichoice: false,
                    unique: false,
                    on_delete: umbral_core::orm::FkAction::NoAction,
                    on_update: umbral_core::orm::FkAction::NoAction,
                    index: false,
                    auto_now_add: false,
                    auto_now: false,
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
                },
                Column {
                    name: "note".to_string(),
                    ty: SqlType::Text,
                    primary_key: false,
                    nullable: true,
                    fk_target: None,
                    noform: false,
                    privileged: false,
                    db_constraint: true,
                    noedit: false,
                    auto_user_add: false,
                    auto_user: false,
                    is_string_repr: false,
                    max_length: 0,
                    choices: Vec::new(),
                    choice_labels: Vec::new(),
                    default: String::new(),
                    is_multichoice: false,
                    unique: false,
                    on_delete: umbral_core::orm::FkAction::NoAction,
                    on_update: umbral_core::orm::FkAction::NoAction,
                    index: false,
                    auto_now_add: false,
                    auto_now: false,
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
                },
            ];
            let file = MigrationFile {
                id: M5_1_ALTER_MIGRATION_ID.to_string(),
                plugin: APP_PLUGIN_NAME.to_string(),
                depends_on: Vec::new(),
                operations: vec![Operation::AlterColumn {
                    table: M5_1_ALTER_TABLE.to_string(),
                    column: "note".to_string(),
                    new_columns,
                    prev_columns: None,
                    unique_together: Vec::new(),
                    indexes: Vec::new(),
                }],
                snapshot_after: Snapshot::current(),
                replaces: Vec::new(),
            };
            write_prior_migration(tmp.path(), &file);
            let applied = run_in(tmp.path())
                .await
                .expect("seed run_in should apply the M5.1 AlterColumn migration");
            assert_eq!(
                applied, 1,
                "exactly one new migration should apply in the M5.1 seed; got {applied}",
            );

            tmp
        })
        .await;
    dir.path()
}

// --------------------------------------------------------------------- //
// Helpers around the on-disk layout. `make_in` and friends write into   //
// `<dir>/<APP_PLUGIN_NAME>/`, so tests that hand-craft a prior file     //
// need to mkdir that subdir first.                                      //
// --------------------------------------------------------------------- //

/// Path to the plugin dir under a base temp dir.
fn plugin_dir(base: &Path) -> PathBuf {
    base.join(APP_PLUGIN_NAME)
}

/// Write `file` into `<base>/<plugin>/<file.id>.json`, creating the plugin
/// dir if needed. Used to seed a "prior state" for diff-driven tests.
fn write_prior_migration(base: &Path, file: &MigrationFile) {
    let dir = plugin_dir(base);
    std::fs::create_dir_all(&dir).expect("create plugin dir");
    let path = dir.join(format!("{}.json", file.id));
    let json = serde_json::to_string_pretty(file).expect("serialize MigrationFile");
    std::fs::write(&path, json).expect("write migration file");
}

/// Read and parse a migration file written by `make_in`.
fn read_migration_file(path: &Path) -> MigrationFile {
    let text = std::fs::read_to_string(path).expect("read migration file");
    serde_json::from_str(&text).expect("parse migration file")
}

/// Build a `MigrationFile` whose `snapshot_after` is `snapshot` and whose
/// operations are empty. Used as the "previous state" stand-in: the
/// engine reads `snapshot_after` off the latest file as the diff
/// baseline, so the ops don't matter for the make-side tests.
fn migration_with_snapshot(id: &str, snapshot: Snapshot) -> MigrationFile {
    MigrationFile {
        id: id.to_string(),
        plugin: APP_PLUGIN_NAME.to_string(),
        depends_on: Vec::new(),
        operations: Vec::new(),
        snapshot_after: snapshot,
        replaces: Vec::new(),
    }
}

/// Every registered model's table name, sorted. Lets a test pin "every
/// model produced a CreateTable" without hard-coding the list.
fn registered_table_names() -> Vec<String> {
    let mut names: Vec<String> = registered_models()
        .iter()
        .map(|m| m.table.clone())
        .collect();
    names.sort();
    names
}

/// Pull the table names out of a CreateTable-only op list, sorted. Used
/// to compare against `registered_table_names()` without depending on
/// the diff's internal ordering.
fn create_table_names(ops: &[Operation]) -> Vec<String> {
    let mut names: Vec<String> = ops
        .iter()
        .filter_map(|op| match op {
            Operation::CreateTable { table, .. } => Some(table.clone()),
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
            | Operation::CreateM2MTable { .. }
            | Operation::AddIndex { .. }
            | Operation::DropIndex { .. }
            | Operation::RunSql { .. } => None,
        })
        .collect();
    names.sort();
    names
}

/// Direct handle to the ambient pool. `run_in` and `show_in` already read
/// it; tests use this to assert against `umbral_migrations` and the
/// created tables.
fn pool() -> SqlitePool {
    umbral::db::pool()
}

// --------------------------------------------------------------------- //
// Make-side tests. Each uses a private tempdir; none calls run_in, so   //
// the shared pool stays untouched and these tests don't interact.       //
// --------------------------------------------------------------------- //

/// `make_in` against a directory whose latest snapshot already equals the
/// live registry's current snapshot should report `NoChanges`. The
/// previous file's `snapshot_after` matches `Snapshot::current()`, so the
/// diff is empty and the engine refuses to write an empty migration.
#[tokio::test]
async fn make_in_returns_no_changes_when_latest_snapshot_matches_current() {
    boot().await;
    let tmp = tempfile::tempdir().expect("create tempdir");

    write_prior_migration(
        tmp.path(),
        &migration_with_snapshot("0001_seed", Snapshot::current()),
    );

    let err = make_in(tmp.path())
        .await
        .expect_err("identical snapshots should produce NoChanges");
    assert!(
        matches!(err, MigrateError::NoChanges),
        "expected MigrateError::NoChanges, got {err:?}",
    );
}

/// First-ever `make_in` (empty dir) should emit one CreateTable per
/// registered model, write `0001_<suffix>.json`, and stamp
/// `snapshot_after` equal to `Snapshot::current()`. With two models the
/// suffix collapses to `auto` because there's no single dominant table.
#[tokio::test]
async fn first_make_in_emits_create_table_for_every_registered_model() {
    boot().await;
    let tmp = tempfile::tempdir().expect("create tempdir");

    let written = make_in(tmp.path())
        .await
        .expect("first make_in against empty dir should write a migration");
    assert_eq!(
        written.len(),
        1,
        "one plugin (`app`) registered, one file expected; got {} paths",
        written.len(),
    );
    let path = written.first().expect("at least one written path");

    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .expect("filename is utf-8");
    assert!(
        filename.starts_with("0001_"),
        "first migration filename should be zero-padded `0001_*`, got {filename}",
    );
    assert!(
        filename.ends_with(".json"),
        "migration filenames are JSON, got {filename}",
    );
    // Two models -> suffix is `auto` (no single dominant op).
    assert_eq!(
        filename, "0001_auto.json",
        "two-model first migration should land on `_auto`, got {filename}",
    );

    let file = read_migration_file(path);
    assert_eq!(file.id, "0001_auto");
    assert_eq!(file.plugin, APP_PLUGIN_NAME);
    assert!(
        file.depends_on.is_empty(),
        "first migration has no predecessors, got {:?}",
        file.depends_on,
    );

    // One CreateTable per registered model, no other op kinds.
    assert_eq!(
        file.operations.len(),
        registered_models().len(),
        "one op per registered model, got {} ops for {} models",
        file.operations.len(),
        registered_models().len(),
    );
    for op in &file.operations {
        assert!(
            matches!(op, Operation::CreateTable { .. }),
            "first migration should only contain CreateTable ops, got {op:?}",
        );
    }
    assert_eq!(
        create_table_names(&file.operations),
        registered_table_names(),
        "every registered model's table should appear exactly once",
    );

    // The snapshot stamped into the file is the registry's current view.
    assert_eq!(
        file.snapshot_after,
        Snapshot::current(),
        "snapshot_after should equal the live Snapshot::current()",
    );
}

/// A hand-written prior snapshot that's empty (no models) forces the
/// next `make_in` to diff the registry against nothing, producing one
/// CreateTable per registered model in a `0002_*.json` file. This is the
/// "change a model, re-run make" half of the loop, simulated by editing
/// the latest snapshot on disk rather than touching the locked registry.
#[tokio::test]
async fn hand_written_empty_prior_snapshot_drives_a_diff_of_all_models() {
    boot().await;
    let tmp = tempfile::tempdir().expect("create tempdir");

    // Seed an empty prior snapshot as `0001_prior.json`.
    write_prior_migration(
        tmp.path(),
        &migration_with_snapshot("0001_prior", Snapshot { models: Vec::new() }),
    );

    let written = make_in(tmp.path())
        .await
        .expect("empty prior snapshot should drive a non-empty diff");
    assert_eq!(
        written.len(),
        1,
        "one plugin (`app`) registered, one file expected; got {} paths",
        written.len(),
    );
    let path = written.first().expect("at least one written path");

    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .expect("filename is utf-8");
    assert!(
        filename.starts_with("0002_"),
        "second migration should be numbered 0002, got {filename}",
    );

    let file = read_migration_file(path);
    assert_eq!(file.operations.len(), registered_models().len());
    for op in &file.operations {
        assert!(
            matches!(op, Operation::CreateTable { .. }),
            "every op should be a CreateTable, got {op:?}",
        );
    }
    assert_eq!(
        create_table_names(&file.operations),
        registered_table_names(),
        "every registered model's table should appear once in the diff",
    );
    assert_eq!(
        file.snapshot_after,
        Snapshot::current(),
        "snapshot_after should be the live current snapshot",
    );
}

// --------------------------------------------------------------------- //
// Run-side tests. All share the `MIGRATED` tempdir: it carries the      //
// post-apply state of the registry against the shared pool. Each test   //
// observes that state (no DDL writes outside the OnceCell's init).      //
// --------------------------------------------------------------------- //

/// `run_in`'s seed pass should create every registered model's table and
/// insert one row per migration into `umbral_migrations` with populated
/// `snapshot_hash` and `applied_at`. The hash should match
/// `Snapshot::current().hash()` because the registry hasn't moved since.
#[tokio::test]
async fn run_in_applies_pending_migrations_and_creates_tables() {
    let dir = migrated_dir().await;
    // Touch the dir so the unused-let lint doesn't fire; the assertions
    // below operate on the shared pool the seed populated.
    let _ = dir;

    let pool = pool();
    for table in registered_table_names() {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(&table)
                .fetch_optional(&pool)
                .await
                .expect("sqlite_master query should succeed");
        assert!(
            row.is_some(),
            "expected table `{table}` to exist after run_in, but sqlite_master has no row",
        );
    }

    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT plugin, name, applied_at, snapshot_hash FROM umbral_migrations ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("select from umbral_migrations should succeed");
    // `migrated_dir()` now seeds three migrations: the autogenerated
    // `0001_auto` for the model registry, the hand-crafted
    // `0002_add_post_*` (M8 AddColumn), and the hand-crafted
    // `0003_alter_*` (M5.1 AlterColumn). All three must appear here
    // as proof that `run_in` applied each one.
    assert_eq!(
        rows.len(),
        3,
        "tracking table should hold one row per seeded migration; got {}",
        rows.len(),
    );

    let (plugin, name, applied_at, snapshot_hash) = &rows[0];
    assert_eq!(plugin, APP_PLUGIN_NAME);
    assert_eq!(name, "0001_auto");
    assert!(
        !applied_at.is_empty(),
        "applied_at should be populated, got an empty string",
    );
    assert_eq!(
        snapshot_hash,
        &Snapshot::current().hash(),
        "snapshot_hash should equal the live snapshot's hash",
    );

    let (plugin, name, applied_at, _) = &rows[1];
    assert_eq!(plugin, APP_PLUGIN_NAME);
    assert_eq!(name, M8_ADD_COLUMN_MIGRATION_ID);
    assert!(
        !applied_at.is_empty(),
        "M8 AddColumn applied_at should be populated, got an empty string",
    );

    let (plugin, name, applied_at, _) = &rows[2];
    assert_eq!(plugin, APP_PLUGIN_NAME);
    assert_eq!(name, M5_1_ALTER_MIGRATION_ID);
    assert!(
        !applied_at.is_empty(),
        "M5.1 AlterColumn applied_at should be populated, got an empty string",
    );
}

/// `run_in` is idempotent: calling it again against the already-applied
/// dir returns zero and never inserts a duplicate tracking row.
#[tokio::test]
async fn run_in_is_idempotent_on_a_second_call() {
    let dir = migrated_dir().await;

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM umbral_migrations")
        .fetch_one(&pool())
        .await
        .expect("count tracking rows");

    let applied = run_in(dir).await.expect("re-running run_in should succeed");
    assert_eq!(
        applied, 0,
        "second run_in should be a no-op, got {applied} applications",
    );

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM umbral_migrations")
        .fetch_one(&pool())
        .await
        .expect("count tracking rows");
    assert_eq!(
        before, after,
        "tracking table row count should not change on a no-op run_in",
    );
}

/// After `run_in` has applied everything, a fresh `make_in` against the
/// same dir should report `NoChanges` because the latest file's
/// `snapshot_after` already matches `Snapshot::current()`. This is the
/// `make` half of the up-to-date contract.
#[tokio::test]
async fn make_in_returns_no_changes_after_run_in_against_same_dir() {
    let dir = migrated_dir().await;

    let err = make_in(dir)
        .await
        .expect_err("dir already up to date should produce NoChanges");
    assert!(
        matches!(err, MigrateError::NoChanges),
        "expected MigrateError::NoChanges, got {err:?}",
    );
}

/// `show_in` after `run_in` should report zero pending migrations. The
/// seed pass wrote one file and applied it, so every entry in the dir
/// is now marked `[X]` and pending == 0.
#[tokio::test]
async fn show_in_reports_zero_pending_after_run_in() {
    let dir = migrated_dir().await;

    let pending = show_in(dir).await.expect("show_in should succeed");
    assert_eq!(
        pending, 0,
        "every migration is applied, so pending should be zero; got {pending}",
    );
}

// --------------------------------------------------------------------- //
// Sanity guard on the local test model.                                  //
// --------------------------------------------------------------------- //

/// Sanity check on the live `Comment` model used above: its `FieldSpec`
/// list matches what the derive sees, and it carries a `Text`, an
/// `Option<DateTime<Utc>>` (nullable Timestamptz), and a `BigInt` PK.
/// Keeps drift in the derive's classification from silently breaking
/// the snapshot-shape tests above.
#[test]
fn comment_field_specs_are_what_the_migrate_tests_assume() {
    let by_name: std::collections::HashMap<&str, &FieldSpec> = <Comment as Model>::FIELDS
        .iter()
        .map(|f| (f.name, f))
        .collect();

    let id = by_name.get("id").expect("Comment has an id field");
    assert!(id.primary_key, "id is the primary key");
    assert_eq!(id.ty, SqlType::BigInt);
    assert!(!id.nullable);

    let body = by_name.get("body").expect("Comment has a body field");
    assert_eq!(body.ty, SqlType::Text);
    assert!(!body.nullable);
    assert!(!body.primary_key);

    let posted_at = by_name
        .get("posted_at")
        .expect("Comment has a posted_at field");
    assert_eq!(posted_at.ty, SqlType::Timestamptz);
    assert!(posted_at.nullable, "posted_at is Option<DateTime<Utc>>");
    assert!(!posted_at.primary_key);
}

// --------------------------------------------------------------------- //
// M8 column-level diff coverage. These tests drive `diff` directly with //
// hand-built snapshots; no `App::build()` boot, no `OnceLock` writes,   //
// nothing shared with the make / run fixtures above. They're cheap and  //
// parallel-safe by construction.                                         //
// --------------------------------------------------------------------- //

/// Small constructor for a one-table `Snapshot`. Hand-built snapshots
/// keep the M8 diff tests independent of the live registry; spec 06
/// calls the diff "the engine's contract", and the contract is what we
/// pin here.
fn snapshot_of(model: ModelMeta) -> Snapshot {
    Snapshot {
        models: vec![model],
    }
}

/// Build a `Post`-shaped `ModelMeta` from a column list. The model
/// `name` is what `diff` keys on; pinning it to `"Post"` keeps the
/// table name and the model name stable across the M8 scenarios.
fn post_model(fields: Vec<Column>) -> ModelMeta {
    ModelMeta {
        view: None,
        materialized: false,
        name: "Post".to_string(),
        table: "post".to_string(),
        fields,
        display: "Post".to_string(),
        icon: "database".to_string(),
        database: None,
        singleton: false,
        unique_together: Vec::new(),
        indexes: Vec::new(),
        ordering: Vec::new(),
        m2m_relations: Vec::new(),
        soft_delete: false,
        audited: false,
        app_label: "app".to_string(),
    }
}

/// A non-nullable `BigInt` primary-key column named `id`. The fixed
/// prefix every Post-shaped snapshot in the M8 tests carries.
fn id_column() -> Column {
    Column {
        name: "id".to_string(),
        ty: SqlType::BigInt,
        primary_key: true,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
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

/// A non-nullable `Text` column with the given name. The body of every
/// Post-shaped non-pk field in these tests.
fn text_column(name: &str) -> Column {
    Column {
        name: name.to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
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

/// Nullable sibling of `text_column`. Used by tests that exercise the
/// post-gap-97 AddColumn path: a NOT NULL column without a default is
/// rejected at diff time, so adding `body: Option<String>` is the safe
/// shape for "new field on an existing table."
fn nullable_text_column(name: &str) -> Column {
    let mut c = text_column(name);
    c.nullable = true;
    c
}

/// M8 — `diff` emits one `AddColumn` when a new (nullable) field
/// appears on an existing model. The previous snapshot has
/// `Post { id, title }`; the current has `Post { id, title, body }`.
/// Spec 06 §"What shipped at M8 v1" calls this the headline case for
/// the column-level diff. Gap 97: the column has to be nullable (or
/// carry a default / auto_now*) — adding a NOT NULL column without a
/// default to an existing table would fail on every populated row.
#[test]
fn diff_emits_add_column_for_a_new_field() {
    let previous = snapshot_of(post_model(vec![id_column(), text_column("title")]));
    let current = snapshot_of(post_model(vec![
        id_column(),
        text_column("title"),
        nullable_text_column("body"),
    ]));

    let ops = diff(&previous, &current).expect("a nullable AddColumn is always safe");
    assert_eq!(
        ops.len(),
        1,
        "one new field should produce exactly one op; got {ops:?}",
    );
    match &ops[0] {
        Operation::AddColumn { table, column } => {
            assert_eq!(table, "post");
            assert_eq!(column.name, "body");
            assert_eq!(column.ty, SqlType::Text);
            assert!(column.nullable);
            assert!(!column.primary_key);
        }
        other => panic!("expected Operation::AddColumn, got {other:?}"),
    }
}

/// Gap 97 — adding a NOT NULL column without a default to an
/// existing table is rejected at diff time. SQLite + Postgres would
/// reject the ADD; we surface the same failure with actionable
/// guidance so the user picks one of: make the field Optional, add a
/// default, or add auto_now_add.
#[test]
fn diff_rejects_not_null_add_column_without_default() {
    let previous = snapshot_of(post_model(vec![id_column(), text_column("title")]));
    let current = snapshot_of(post_model(vec![
        id_column(),
        text_column("title"),
        text_column("body"), // NOT NULL, no default — should fail
    ]));

    let err = diff(&previous, &current).expect_err("NOT NULL without default must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("body"),
        "error names the offending column: {msg}"
    );
    assert!(
        msg.contains("default") || msg.contains("Option") || msg.contains("auto_now"),
        "error guides the user: {msg}",
    );
}

/// Gap 97 — a NOT NULL field that carries `default = "..."` is fine
/// (the migration backfills existing rows from the DEFAULT).
#[test]
fn diff_accepts_not_null_add_column_with_default() {
    let previous = snapshot_of(post_model(vec![id_column(), text_column("title")]));
    let mut body_with_default = text_column("body");
    body_with_default.default = "draft".to_string();
    let current = snapshot_of(post_model(vec![
        id_column(),
        text_column("title"),
        body_with_default,
    ]));

    let ops = diff(&previous, &current).expect("default unblocks NOT NULL add");
    assert_eq!(ops.len(), 1);
    assert!(matches!(&ops[0], Operation::AddColumn { .. }));
}

/// M8 — `diff` emits one `DropColumn` when a field is removed from an
/// existing model. The previous snapshot has `Post { id, title, body }`;
/// the current drops `body`.
#[test]
fn diff_emits_drop_column_for_a_removed_field() {
    let previous = snapshot_of(post_model(vec![
        id_column(),
        text_column("title"),
        text_column("body"),
    ]));
    let current = snapshot_of(post_model(vec![id_column(), text_column("title")]));

    let ops = diff(&previous, &current).expect("DropColumn is always safe");
    assert_eq!(
        ops.len(),
        1,
        "one removed field should produce exactly one op; got {ops:?}",
    );
    match &ops[0] {
        Operation::DropColumn { table, column } => {
            assert_eq!(table, "post");
            assert_eq!(column, "body");
        }
        other => panic!("expected Operation::DropColumn, got {other:?}"),
    }
}

/// Gap 88 — a single column rename is detected by the diff engine
/// and emitted as `RenameColumn`, not as the legacy `DropColumn` +
/// `AddColumn` pair. Triggered when exactly one drop and one add
/// share identical column shapes (sans name). Anything more
/// ambiguous (multi-rename, shape mismatch) falls back to drop+add.
#[test]
fn diff_emits_rename_column_for_a_single_renamed_column() {
    let previous = snapshot_of(post_model(vec![
        id_column(),
        text_column("title"),
        text_column("body"),
    ]));
    let current = snapshot_of(post_model(vec![
        id_column(),
        text_column("title"),
        text_column("content"),
    ]));

    let ops = diff(&previous, &current).expect("rename is always safe");
    assert_eq!(
        ops.len(),
        1,
        "single rename should produce exactly one RenameColumn; got {ops:?}",
    );
    match &ops[0] {
        Operation::RenameColumn {
            table,
            from,
            to,
            column,
        } => {
            assert_eq!(table, "post");
            assert_eq!(from, "body");
            assert_eq!(to, "content");
            let c = column.as_ref().expect("column shape carried");
            assert_eq!(c.name, "content");
            assert_eq!(c.ty, SqlType::Text);
        }
        other => panic!("expected Operation::RenameColumn, got {other:?}"),
    }
}

/// Gap 88 — when shapes don't match, fall back to the legacy
/// drop+add. Here `body: Text` drops and `count: Integer` adds; the
/// types differ so the heuristic stays out of the way (avoids
/// silently inferring a rename against the user's intent).
#[test]
fn diff_falls_back_to_drop_and_add_when_shapes_differ() {
    let previous = snapshot_of(post_model(vec![
        id_column(),
        text_column("title"),
        text_column("body"),
    ]));
    let mut count_col = nullable_text_column("count");
    count_col.ty = SqlType::Integer;
    let current = snapshot_of(post_model(vec![
        id_column(),
        text_column("title"),
        count_col,
    ]));

    let ops = diff(&previous, &current).expect("drop+add is always safe");
    assert_eq!(
        ops.len(),
        2,
        "shape mismatch falls back to drop+add: {ops:?}"
    );
    assert!(
        matches!(&ops[0], Operation::DropColumn { column, .. } if column == "body"),
        "drop comes first: {ops:?}",
    );
    assert!(
        matches!(&ops[1], Operation::AddColumn { column, .. } if column.name == "count"),
        "add comes second: {ops:?}",
    );
}

/// M8 — an in-place column type change surfaces as
/// `MigrateError::UnsafeAlter`. SQLite can't `ALTER COLUMN TYPE`
/// without a table-recreation dance, and that dance is deferred past
/// M8 v1; the engine refuses the migration so the user hand-writes a
/// data-preserving step.
#[test]
fn diff_returns_unsafe_alter_for_a_type_change() {
    let previous = snapshot_of(post_model(vec![id_column(), text_column("title")]));
    let current = snapshot_of(post_model(vec![
        id_column(),
        Column {
            name: "title".to_string(),
            ty: SqlType::Integer,
            primary_key: false,
            nullable: false,
            fk_target: None,
            noform: false,
            privileged: false,
            db_constraint: true,
            noedit: false,
            auto_user_add: false,
            auto_user: false,
            is_string_repr: false,
            max_length: 0,
            choices: Vec::new(),
            choice_labels: Vec::new(),
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: umbral_core::orm::FkAction::NoAction,
            on_update: umbral_core::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
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
        },
    ]));

    let err = diff(&previous, &current).expect_err("a type change must be UnsafeAlter");
    match err {
        MigrateError::UnsafeAlter {
            model,
            column,
            reason,
        } => {
            assert_eq!(model, "Post");
            assert_eq!(column, "title");
            assert!(
                reason.contains("type"),
                "UnsafeAlter reason should call out the type change; got {reason:?}",
            );
        }
        other => panic!("expected MigrateError::UnsafeAlter, got {other:?}"),
    }
}

/// M5.1 — flipping a column's nullable flag emits an `AlterColumn`
/// op carrying the full new column list. The render layer turns that
/// into the SQLite table-recreation dance; M5.1 only ships the
/// nullable case (type / pk changes still UnsafeAlter).
#[test]
fn diff_emits_alter_column_for_a_nullable_flip() {
    let previous = snapshot_of(post_model(vec![id_column(), text_column("title")]));
    let current = snapshot_of(post_model(vec![
        id_column(),
        Column {
            name: "title".to_string(),
            ty: SqlType::Text,
            primary_key: false,
            nullable: true,
            fk_target: None,
            noform: false,
            privileged: false,
            db_constraint: true,
            noedit: false,
            auto_user_add: false,
            auto_user: false,
            is_string_repr: false,
            max_length: 0,
            choices: Vec::new(),
            choice_labels: Vec::new(),
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: umbral_core::orm::FkAction::NoAction,
            on_update: umbral_core::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
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
        },
    ]));

    let ops = diff(&previous, &current).expect("a nullable flip must emit AlterColumn");
    assert_eq!(
        ops.len(),
        1,
        "exactly one op per changed column; got {ops:?}"
    );
    match &ops[0] {
        Operation::AlterColumn {
            table,
            column,
            new_columns,
            prev_columns: _,
            ..
        } => {
            assert_eq!(table, "post");
            assert_eq!(column, "title");
            assert_eq!(
                new_columns.len(),
                2,
                "new_columns must carry the full post-change schema; got {new_columns:?}",
            );
            let title = new_columns
                .iter()
                .find(|c| c.name == "title")
                .expect("title column should be in new_columns");
            assert!(title.nullable, "title's new nullable flag must be true");
        }
        other => panic!("expected Operation::AlterColumn, got {other:?}"),
    }
}

/// Gap #64 — BigInt → Text is in the safe-cast whitelist; the diff
/// must emit an AlterColumn carrying both snapshots (so the Postgres
/// renderer can produce the `USING <col>::text` clause) rather than
/// the legacy `UnsafeAlter` error.
#[test]
fn diff_emits_alter_column_for_safe_type_cast_bigint_to_text() {
    let prev_user_id = Column {
        name: "user_id".to_string(),
        ty: SqlType::BigInt,
        primary_key: false,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
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
    };
    let mut curr_user_id = prev_user_id.clone();
    curr_user_id.ty = SqlType::Text;

    let previous = snapshot_of(post_model(vec![id_column(), prev_user_id]));
    let current = snapshot_of(post_model(vec![id_column(), curr_user_id]));

    let ops = diff(&previous, &current)
        .expect("BigInt -> Text is in the safe-cast whitelist; must NOT be UnsafeAlter");
    assert_eq!(ops.len(), 1, "one op per changed column; got {ops:?}");
    match &ops[0] {
        Operation::AlterColumn {
            column,
            prev_columns,
            ..
        } => {
            assert_eq!(column, "user_id");
            let prev = prev_columns
                .as_ref()
                .expect("safe-cast AlterColumn must carry prev_columns for Postgres render");
            let prev_col = prev
                .iter()
                .find(|c| c.name == "user_id")
                .expect("prev_columns must include the changed column");
            assert_eq!(
                prev_col.ty,
                SqlType::BigInt,
                "prev snapshot must record the pre-change type",
            );
        }
        other => panic!("expected AlterColumn, got {other:?}"),
    }
}

/// Gap #64 boundary case — Text → BigInt is NOT in the whitelist
/// (parse can fail at runtime on non-numeric rows). The diff must
/// still refuse with UnsafeAlter so the user is forced to write the
/// data-preserving migration.
#[test]
fn diff_still_refuses_text_to_bigint_as_unsafe() {
    let prev_value = Column {
        name: "value".to_string(),
        ty: SqlType::Text,
        primary_key: false,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: umbral_core::orm::FkAction::NoAction,
        on_update: umbral_core::orm::FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
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
    };
    let mut curr_value = prev_value.clone();
    curr_value.ty = SqlType::BigInt;

    let previous = snapshot_of(post_model(vec![id_column(), prev_value]));
    let current = snapshot_of(post_model(vec![id_column(), curr_value]));

    let err = diff(&previous, &current)
        .expect_err("Text -> BigInt must remain UnsafeAlter; parse can fail");
    match err {
        MigrateError::UnsafeAlter { column, reason, .. } => {
            assert_eq!(column, "value");
            assert!(
                reason.contains("safe-cast whitelist") || reason.contains("not in the safe"),
                "error message should explain the whitelist policy; got {reason}",
            );
        }
        other => panic!("expected UnsafeAlter, got {other:?}"),
    }
}

/// M8 — end-to-end: a hand-crafted `0002_add_<column>.json` carrying
/// one `AddColumn` op against the seeded `post` table applies cleanly
/// via `run_in`, registers in `umbral_migrations`, and the new column
/// shows up in `PRAGMA table_info`.
///
/// The shared `migrated_dir()` init writes this migration into the
/// MIGRATED tempdir and applies it as part of the one-shot seed, so the
/// DDL serializes against the other run-side tests' reads on the shared
/// pool. By the time this test inspects the state, the ALTER has
/// committed; the assertions read `PRAGMA table_info(post)` and the
/// tracking table.
///
/// This test is also the proof that subagent A's `render_operation`
/// body for `AddColumn` works: if it still returned the scaffold
/// placeholder, the rendered SQL would be a SQL comment and the column
/// would never appear in `PRAGMA table_info`.
#[tokio::test]
async fn run_in_applies_a_hand_crafted_add_column_migration() {
    // Driving the shared seed is what applies the AddColumn DDL.
    let _ = migrated_dir().await;

    let columns: Vec<(i64, String, String, i64, Option<String>, i64)> =
        sqlx::query_as("PRAGMA table_info(post)")
            .fetch_all(&pool())
            .await
            .expect("PRAGMA table_info(post) should succeed");
    let names: Vec<&str> = columns.iter().map(|c| c.1.as_str()).collect();
    assert!(
        names.contains(&M8_ADD_COLUMN_NAME),
        "AddColumn op should add `{M8_ADD_COLUMN_NAME}` to `post`; got columns {names:?}",
    );

    let tracked: Vec<(String, String)> =
        sqlx::query_as("SELECT plugin, name FROM umbral_migrations WHERE name = ?")
            .bind(M8_ADD_COLUMN_MIGRATION_ID)
            .fetch_all(&pool())
            .await
            .expect("select from umbral_migrations should succeed");
    assert_eq!(
        tracked.len(),
        1,
        "tracking table should record the AddColumn migration; got {tracked:?}",
    );
    assert_eq!(tracked[0].0, APP_PLUGIN_NAME);
}

/// M5.1 — end-to-end: the `AlterColumn` seed in `migrated_dir()`
/// runs the SQLite table-recreation dance against `m5_1_alter_demo`
/// and flips `note` from non-nullable to nullable while preserving
/// the row data.
///
/// The seed creates the table, inserts one row, then applies an
/// AlterColumn migration. By the time this test runs, the dance has
/// committed; the assertions read `PRAGMA table_info` and the seed
/// row to prove the column changed nullable and the data survived.
#[tokio::test]
async fn run_in_applies_an_alter_column_nullable_flip() {
    let _ = migrated_dir().await;
    let pool = pool();

    // The `note` column is nullable in the rebuilt table.
    let columns: Vec<(i64, String, String, i64, Option<String>, i64)> =
        sqlx::query_as(&format!("PRAGMA table_info({M5_1_ALTER_TABLE})"))
            .fetch_all(&pool)
            .await
            .expect("PRAGMA table_info should succeed against the rebuilt table");
    let note = columns
        .iter()
        .find(|c| c.1 == "note")
        .expect("note column should still exist after the recreation");
    assert_eq!(
        note.3, 0,
        "note's `notnull` flag should be 0 after the AlterColumn flip; got column {note:?}"
    );

    // Row data survived the dance.
    let rows: Vec<(i64, Option<String>)> = sqlx::query_as(&format!(
        "SELECT id, note FROM {M5_1_ALTER_TABLE} ORDER BY id"
    ))
    .fetch_all(&pool)
    .await
    .expect("SELECT from rebuilt table");
    assert_eq!(rows, vec![(1, Some("hello".to_string()))]);

    // The migration is recorded in the tracking table.
    let tracked: Vec<(String, String)> =
        sqlx::query_as("SELECT plugin, name FROM umbral_migrations WHERE name = ?")
            .bind(M5_1_ALTER_MIGRATION_ID)
            .fetch_all(&pool)
            .await
            .expect("select from umbral_migrations");
    assert_eq!(
        tracked.len(),
        1,
        "tracking table should record the M5.1 AlterColumn migration; got {tracked:?}"
    );
    assert_eq!(tracked[0].0, APP_PLUGIN_NAME);
}

/// Regression: a `Model` with `id: i64` (BigInt PK) must render as
/// `INTEGER PRIMARY KEY AUTOINCREMENT` on SQLite, not `bigint PRIMARY
/// KEY`. Otherwise an `INSERT INTO t (other_cols) VALUES (...)`
/// without an explicit id value fails the NOT NULL constraint, since
/// only the exact text `INTEGER` triggers SQLite's ROWID-alias
/// auto-increment behaviour.
///
/// Pinning the invariant via a behavioural assertion: insert two rows
/// without explicit ids and confirm SQLite assigned monotonically
/// increasing PKs (1, 2). The shared seed already created `post` via
/// the M5 CreateTable op; this test layers an INSERT pair on top.
#[tokio::test]
async fn create_table_emits_integer_pk_so_inserts_auto_increment() {
    let _ = migrated_dir().await;
    let pool = pool();

    // Brand-new table for this test so the inserts don't collide with
    // the seeded `post` rows or row counts the other tests assert on.
    sqlx::query(
        "CREATE TABLE m51_pk_probe (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("create m51_pk_probe");

    // The behavioural check applies to any auto-increment-shaped DDL:
    // an INSERT that omits id should succeed and SQLite should assign
    // 1, 2, ... .
    sqlx::query("INSERT INTO m51_pk_probe (label) VALUES (?)")
        .bind("alpha")
        .execute(&pool)
        .await
        .expect("insert without explicit id should succeed");
    sqlx::query("INSERT INTO m51_pk_probe (label) VALUES (?)")
        .bind("beta")
        .execute(&pool)
        .await
        .expect("second insert without explicit id should succeed");

    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, label FROM m51_pk_probe ORDER BY id")
        .fetch_all(&pool)
        .await
        .expect("select assigned ids");
    assert_eq!(
        rows,
        vec![(1, "alpha".to_string()), (2, "beta".to_string())]
    );

    // And the engine-rendered `post` table picks up the same shape: a
    // sqlite_master row mentioning `INTEGER` and `AUTOINCREMENT` on
    // the id column. Case-insensitive grep because sea-query's output
    // is lowercase and the tests should survive a rendering tweak.
    let post_ddl: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'post'")
            .fetch_one(&pool)
            .await
            .expect("select post DDL");
    let lower = post_ddl.to_ascii_lowercase();
    assert!(
        lower.contains("integer") && lower.contains("autoincrement"),
        "post DDL must use INTEGER PRIMARY KEY AUTOINCREMENT for the SQLite ROWID-alias \
         mechanic to fire; got: {post_ddl}",
    );
    assert!(
        !lower.contains("bigint"),
        "post DDL must NOT use BIGINT for the PK on SQLite (would defeat auto-increment); \
         got: {post_ddl}",
    );
}

// --------------------------------------------------------------------- //
// BUG-16 step 1: `diff` emits CreateM2MTable / DropM2MTable when a       //
// model gains or loses an M2M field.                                     //
// --------------------------------------------------------------------- //

use umbral_core::migrate::M2MRelation;

fn tag_model() -> ModelMeta {
    ModelMeta {
        view: None,
        materialized: false,
        name: "Tag".to_string(),
        table: "tag".to_string(),
        fields: vec![id_column(), text_column("name")],
        display: "Tag".to_string(),
        icon: "database".to_string(),
        database: None,
        singleton: false,
        unique_together: Vec::new(),
        indexes: Vec::new(),
        ordering: Vec::new(),
        m2m_relations: Vec::new(),
        soft_delete: false,
        audited: false,
        app_label: "app".to_string(),
    }
}

#[test]
fn diff_emits_create_m2m_table_when_a_field_is_added() {
    let prev = Snapshot {
        models: vec![post_model(vec![id_column()]), tag_model()],
    };
    let mut post_with_tags = post_model(vec![id_column()]);
    post_with_tags.m2m_relations.push(M2MRelation {
        field_name: "tags".to_string(),
        target_table: "tag".to_string(),
        target_name: "Tag".to_string(),
    });
    let curr = Snapshot {
        models: vec![post_with_tags, tag_model()],
    };

    let ops = umbral::migrate::diff(&prev, &curr).expect("diff");

    let create_m2m: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            Operation::CreateM2MTable {
                junction_table,
                parent_table,
                child_table,
                parent_col,
                child_col,
                parent_ty,
                child_ty,
            } => Some((
                junction_table.clone(),
                parent_table.clone(),
                child_table.clone(),
                parent_col.clone(),
                child_col.clone(),
                *parent_ty,
                *child_ty,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        create_m2m,
        vec![(
            "post_tags".to_string(),
            "post".to_string(),
            "tag".to_string(),
            "id".to_string(),
            "id".to_string(),
            umbral_core::orm::SqlType::BigInt,
            umbral_core::orm::SqlType::BigInt,
        )],
        "expected one CreateM2MTable for post.tags → tag with i64 PKs both sides; got {ops:?}",
    );
}

#[test]
fn diff_emits_drop_m2m_table_when_a_field_is_removed() {
    let mut post_with_tags = post_model(vec![id_column()]);
    post_with_tags.m2m_relations.push(M2MRelation {
        field_name: "tags".to_string(),
        target_table: "tag".to_string(),
        target_name: "Tag".to_string(),
    });
    let prev = Snapshot {
        models: vec![post_with_tags, tag_model()],
    };
    let curr = Snapshot {
        models: vec![post_model(vec![id_column()]), tag_model()],
    };

    let ops = umbral::migrate::diff(&prev, &curr).expect("diff");

    let drops: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            Operation::DropM2MTable { junction_table } => Some(junction_table.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        drops,
        vec!["post_tags".to_string()],
        "expected one DropM2MTable for post_tags; got {ops:?}",
    );
}

#[test]
fn create_m2m_table_renders_typed_pk_columns_per_backend() {
    use umbral::migrate::render_operation_for;
    use umbral_core::orm::SqlType;

    // Parent with i64 PK, child with String PK — the BUG-16 phase 2
    // motivating case (Group / Permission in umbral-permissions).
    let op = Operation::CreateM2MTable {
        junction_table: "group_permissions".to_string(),
        parent_table: "permissions_group".to_string(),
        parent_col: "id".to_string(),
        child_table: "permissions_permission".to_string(),
        child_col: "codename".to_string(),
        parent_ty: SqlType::BigInt,
        child_ty: SqlType::Text,
    };
    let sqlite_sql = render_operation_for(&op, "sqlite")
        .into_iter()
        .next()
        .unwrap();
    assert!(
        sqlite_sql.contains("\"parent_id\" INTEGER NOT NULL")
            && sqlite_sql.contains("\"child_id\" TEXT NOT NULL"),
        "SQLite junction must respect per-side PK types; got: {sqlite_sql}",
    );
    let pg_sql = render_operation_for(&op, "postgres")
        .into_iter()
        .next()
        .unwrap();
    assert!(
        pg_sql.contains("\"parent_id\" BIGINT NOT NULL")
            && pg_sql.contains("\"child_id\" TEXT NOT NULL"),
        "Postgres junction must use BIGINT + TEXT for i64+String PKs; got: {pg_sql}",
    );
}

#[test]
fn diff_rejects_m2m_pointing_at_unregistered_table() {
    let mut post_with_orphan = post_model(vec![id_column()]);
    post_with_orphan.m2m_relations.push(M2MRelation {
        field_name: "ghosts".to_string(),
        target_table: "nonexistent_table".to_string(),
        target_name: "Ghost".to_string(),
    });
    let curr = Snapshot {
        models: vec![post_with_orphan],
    };
    let err = umbral::migrate::diff(&Snapshot::default(), &curr)
        .expect_err("diff should refuse to emit DDL referencing an unregistered table");
    let msg = format!("{err}");
    assert!(
        msg.contains("nonexistent_table"),
        "error must name the missing target table; got: {msg}",
    );
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "mm_soft", soft_delete)]
struct SoftThing {
    id: i64,
    name: String,
    #[umbral(index)]
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "mm_hard")]
struct HardThing {
    id: i64,
    name: String,
}

#[test]
fn model_meta_carries_soft_delete_flag() {
    let soft = umbral::migrate::ModelMeta::for_::<SoftThing>();
    let hard = umbral::migrate::ModelMeta::for_::<HardThing>();
    assert!(soft.soft_delete, "soft_delete model must carry the flag");
    assert!(!hard.soft_delete, "non-soft-delete model must not");
}

// --------------------------------------------------------------------- //
// Feature #65: `checkmigrations` zero-downtime safety classification.    //
// End-to-end through the public loader: a pending migration file on disk //
// is read and each operation classified by tier. A high sequence number  //
// (0099) keeps the fixture `Pending` regardless of what other tests in   //
// this binary already applied to the shared pool.                        //
// --------------------------------------------------------------------- //

#[tokio::test]
async fn check_pending_safety_classifies_a_pending_migration_off_disk() {
    boot().await;

    let tmp = tempfile::tempdir().expect("create tempdir");
    let app_dir = tmp.path().join(APP_PLUGIN_NAME);
    std::fs::create_dir_all(&app_dir).expect("mkdir app/");

    // One pending migration carrying one op per safety tier.
    let fixture = MigrationFile {
        id: "0099_safety_fixture".to_string(),
        plugin: APP_PLUGIN_NAME.to_string(),
        depends_on: Vec::new(),
        operations: vec![
            // Brand-new table -> Safe.
            Operation::CreateTable {
                table: "audit_log".to_string(),
                columns: vec![id_column()],
                unique_together: Vec::new(),
                indexes: Vec::new(),
            },
            // NOT NULL column, no default -> Warning.
            Operation::AddColumn {
                table: "post".to_string(),
                column: text_column("safety_note"),
            },
            // Column drop -> Unsafe (data loss).
            Operation::DropColumn {
                table: "post".to_string(),
                column: "legacy_field".to_string(),
            },
        ],
        snapshot_after: Snapshot::default(),
        replaces: Vec::new(),
    };
    std::fs::write(
        app_dir.join("0099_safety_fixture.json"),
        serde_json::to_string_pretty(&fixture).expect("serialize fixture"),
    )
    .expect("write fixture migration");

    let classified = umbral::migrate::check_pending_safety_in(tmp.path())
        .await
        .expect("safety check should load and classify the pending file");

    let ours: Vec<_> = classified
        .iter()
        .filter(|c| c.migration == "0099_safety_fixture")
        .collect();
    assert_eq!(ours.len(), 3, "all three ops classified: {classified:?}");

    let safe = ours
        .iter()
        .filter(|c| c.safety == umbral::migrate::OpSafety::Safe)
        .count();
    let warn = ours.iter().filter(|c| c.safety.is_warning()).count();
    let unsafe_ = ours.iter().filter(|c| c.safety.is_unsafe()).count();
    assert_eq!((safe, warn, unsafe_), (1, 1, 1), "one op per tier");

    // The destructive op names the column it drops, with the expand-contract hint.
    let drop = ours.iter().find(|c| c.safety.is_unsafe()).unwrap();
    assert!(
        drop.safety.reason().contains("legacy_field"),
        "drop reason names the column: {}",
        drop.safety.reason()
    );
}
