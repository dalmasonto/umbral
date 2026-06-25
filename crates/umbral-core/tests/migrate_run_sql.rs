#![allow(dead_code, private_interfaces)]

//! Core (SQLite, always-run) coverage for `Operation::RunSql` — the
//! hand-authored raw-SQL **data** migration shipped for gaps2 #69.
//!
//! Proves the four invariants the data-migration capability rests on:
//!
//! 1. A schema migration creates a table; a SECOND, hand-authored
//!    migration carrying only a `RunSql` op INSERTs/UPDATEs rows. The
//!    data lands.
//! 2. The data migration is recorded once in `umbral_migrations`; a
//!    re-run is a no-op (idempotent — the tracking table guards it).
//! 3. `read_migration_file` round-trips a `RunSql` op (serde).
//! 4. `make_empty_in` writes a no-op migration whose `snapshot_after`
//!    equals the prior snapshot, so the next `make` detects no change
//!    (a data migration never disturbs the model-snapshot chain).
//!
//! This file owns its own boot + pool (a separate test binary), so the
//! process-global registry / pool / backend `OnceLock`s are exclusively
//! ours — no interference with `tests/migrate.rs`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use umbral::migrate::{MigrationFile, Operation, Snapshot, make_empty_in, make_in, run_in};

/// A tiny model so the plugin (`app`) has a real table to migrate +
/// later mutate with a `RunSql` op.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "widget")]
struct Widget {
    id: i64,
    name: String,
    active: bool,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings load in test env");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite connects");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Widget>()
            .build()
            .expect("App::build happy path");
    })
    .await;
}

/// Author a data-migration file `0002_run_sql.json` for the `app`
/// plugin: forward SQL seeds a row + flips a flag; `snapshot_after`
/// equals the prior snapshot (a data migration has NO schema effect).
fn write_run_sql_migration(dir: &Path, snapshot: Snapshot) {
    let file = MigrationFile {
        id: "0002_run_sql".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: vec![
            Operation::RunSql {
                sql: "INSERT INTO widget (id, name, active) VALUES (1, 'seeded', 0)".to_string(),
                reverse_sql: Some("DELETE FROM widget WHERE id = 1".to_string()),
            },
            Operation::RunSql {
                sql: "UPDATE widget SET active = 1 WHERE name = 'seeded'".to_string(),
                reverse_sql: None,
            },
        ],
        snapshot_after: snapshot,
    };
    let plugin_dir = dir.join("app");
    std::fs::create_dir_all(&plugin_dir).expect("create app dir");
    let json = serde_json::to_string_pretty(&file).expect("serialize RunSql migration");
    std::fs::write(plugin_dir.join("0002_run_sql.json"), json).expect("write RunSql migration");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_sql_data_migration_applies_and_is_idempotent() {
    boot().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();

    // 1) Schema migration: creates `widget` (auto-detected).
    make_in(dir).await.expect("make schema migration");
    run_in(dir).await.expect("apply schema migration");

    // The prior snapshot (after 0001) — its `snapshot_after` is what the
    // data migration carries forward unchanged.
    let prior = {
        let f =
            std::fs::read_to_string(dir.join("app").join("0001_create_widget.json")).expect("read 0001");
        serde_json::from_str::<MigrationFile>(&f).expect("parse 0001").snapshot_after
    };

    // 2) Hand-author the RunSql data migration and apply it.
    write_run_sql_migration(dir, prior.clone());
    let applied = run_in(dir).await.expect("apply data migration");
    assert_eq!(applied, 1, "exactly the data migration applied this run");

    // The data landed: one seeded row, flag flipped to active.
    let pool = match umbral::db::pool_dispatched() {
        umbral::db::DbPool::Sqlite(p) => p,
        umbral::db::DbPool::Postgres(_) => unreachable!("test pool is sqlite"),
    };
    let (name, active): (String, bool) =
        sqlx::query_as("SELECT name, active FROM widget WHERE id = 1")
            .fetch_one(pool)
            .await
            .expect("seeded row exists");
    assert_eq!(name, "seeded");
    assert!(active, "the second RunSql op flipped active to true");

    // 3) Recorded once.
    let tracked: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM umbral_migrations WHERE plugin = 'app' AND name = '0002_run_sql'")
            .fetch_one(pool)
            .await
            .expect("count tracking rows");
    assert_eq!(tracked, 1, "data migration recorded exactly once");

    // 4) Re-run is a no-op: the tracking table guards re-application, so
    //    the seed isn't doubled (a second INSERT of id=1 would error).
    let again = run_in(dir).await.expect("re-run is clean");
    assert_eq!(again, 0, "idempotent: nothing re-applies");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM widget")
        .fetch_one(pool)
        .await
        .expect("count widget rows");
    assert_eq!(rows, 1, "the seed row was not duplicated on re-run");

    eprintln!("run_sql_data_migration_applies_and_is_idempotent: PASS");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_sql_op_round_trips_through_migration_file() {
    // Serde round-trip of a `RunSql` op through the on-disk file format.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    write_run_sql_migration(dir, Snapshot::default());

    let path = dir.join("app").join("0002_run_sql.json");
    let json = std::fs::read_to_string(&path).expect("read file");
    let parsed: MigrationFile = serde_json::from_str(&json).expect("parse RunSql migration");

    assert_eq!(parsed.operations.len(), 2);
    match &parsed.operations[0] {
        Operation::RunSql { sql, reverse_sql } => {
            assert!(sql.starts_with("INSERT INTO widget"));
            assert_eq!(reverse_sql.as_deref(), Some("DELETE FROM widget WHERE id = 1"));
        }
        other => panic!("expected RunSql, got {other:?}"),
    }
    match &parsed.operations[1] {
        Operation::RunSql { reverse_sql, .. } => {
            assert!(reverse_sql.is_none(), "None reverse_sql round-trips");
        }
        other => panic!("expected RunSql, got {other:?}"),
    }

    eprintln!("run_sql_op_round_trips_through_migration_file: PASS");
}

#[tokio::test(flavor = "multi_thread")]
async fn make_empty_writes_a_noop_migration_that_does_not_drift() {
    boot().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();

    // Establish the baseline 0001 schema migration.
    make_in(dir).await.expect("make 0001");

    // `--empty app` writes 0002 with current snapshot + no ops.
    let path = make_empty_in(dir, "app").await.expect("make_empty");
    assert!(path.ends_with("0002_empty.json"));

    let file: MigrationFile =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read empty"))
            .expect("parse empty");
    assert!(file.operations.is_empty(), "empty migration has no ops");

    // snapshot_after equals the prior 0001's snapshot — no schema change.
    let prior: MigrationFile = serde_json::from_str(
        &std::fs::read_to_string(dir.join("app").join("0001_create_widget.json"))
            .expect("read 0001"),
    )
    .expect("parse 0001");
    assert_eq!(
        file.snapshot_after.hash(),
        prior.snapshot_after.hash(),
        "an empty migration carries the schema snapshot forward unchanged"
    );

    // The next `make` against the same registry detects NO change (the
    // empty migration didn't disturb the snapshot chain).
    match make_in(dir).await {
        Err(umbral::migrate::MigrateError::NoChanges) => {}
        Ok(paths) => panic!("expected NoChanges, got {paths:?}"),
        Err(e) => panic!("expected NoChanges, got {e}"),
    }

    eprintln!("make_empty_writes_a_noop_migration_that_does_not_drift: PASS");
}
