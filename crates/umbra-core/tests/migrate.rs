// The local `Comment` model is private but `#[derive(Model)]` emits a `pub
// const` column module that references it. The same pattern that
// `tests/type_catalogue.rs` uses to silence the lint at the file level.
#![allow(dead_code, private_interfaces)]

//! End-to-end coverage for the M5 migration engine: the declare → migrate →
//! change → migrate loop against an in-memory SQLite pool and per-test
//! temp directories.
//!
//! `umbra-core::migrate` keeps its model registry, DB pool, and active
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
//! (creating model tables, writing into `umbra_migrations`), so a second
//! `run_in` against a fresh tempdir would collide on "table already
//! exists". Sharing the post-apply state is therefore correct, not a
//! workaround: it's the same loop a real binary sees across CLI
//! invocations.
//!
//! See `crates/umbra-core/src/migrate.rs` for the surface this exercises
//! and `docs/specs/06-migration-engine.md` for the target spec.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::OnceCell;

use umbra::migrate::{
    APP_PLUGIN_NAME, MigrateError, MigrationFile, Operation, Snapshot, make_in, registered_models,
    run_in, show_in,
};
use umbra::orm::Model;
use umbra_core::orm::{FieldSpec, Post, SqlType};

// --------------------------------------------------------------------- //
// A second, local test model. Two registered models lets us prove:      //
//   - first `make_in` emits one CreateTable per model (not just one)    //
//   - the suffix collapses to `_auto` when ops > 1 (not `_create_<t>`)  //
//   - the hand-written-prior-snapshot diff is deterministic across      //
//     multiple creates (sorted by model name)                           //
// Declared at module scope so the derive's sibling column module can    //
// resolve `super::Comment`, mirroring the type_catalogue.rs convention. //
// --------------------------------------------------------------------- //

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
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
            umbra::Settings::from_env().expect("figment defaults always load in a test env");
        let pool = umbra::db::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should always connect");

        umbra::App::builder()
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
// pool (creates model tables, writes umbra_migrations rows), so the     //
// run-side tests share one tempdir whose make+run has already happened. //
// The OnceCell ensures the make+run executes exactly once per binary.   //
// --------------------------------------------------------------------- //

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
            Operation::DropTable { .. }
            | Operation::AddColumn { .. }
            | Operation::DropColumn { .. } => None,
        })
        .collect();
    names.sort();
    names
}

/// Direct handle to the ambient pool. `run_in` and `show_in` already read
/// it; tests use this to assert against `umbra_migrations` and the
/// created tables.
fn pool() -> SqlitePool {
    umbra::db::pool()
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
/// insert one row per migration into `umbra_migrations` with populated
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
        "SELECT plugin, name, applied_at, snapshot_hash FROM umbra_migrations ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("select from umbra_migrations should succeed");
    assert_eq!(
        rows.len(),
        1,
        "tracking table should hold exactly one row after the seed migration; got {}",
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
}

/// `run_in` is idempotent: calling it again against the already-applied
/// dir returns zero and never inserts a duplicate tracking row.
#[tokio::test]
async fn run_in_is_idempotent_on_a_second_call() {
    let dir = migrated_dir().await;

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM umbra_migrations")
        .fetch_one(&pool())
        .await
        .expect("count tracking rows");

    let applied = run_in(dir).await.expect("re-running run_in should succeed");
    assert_eq!(
        applied, 0,
        "second run_in should be a no-op, got {applied} applications",
    );

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM umbra_migrations")
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
