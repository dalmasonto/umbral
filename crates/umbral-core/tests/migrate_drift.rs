//! Gap-24 drift-detection integration tests.
//!
//! Exercises the four-state drift model, `fake_apply`, `fake_initial`,
//! `detect_drift`, and `run_checked_in` against an in-memory SQLite pool.
//!
//! Two shapes of test live here:
//!
//! - **Pure drift-detection** tests use `detect_drift` / `detect_all_drift`
//!   directly with hand-built `HashSet`s and `TempDir`s. No pool or App
//!   boot required; these are plain `#[test]` functions that run in
//!   parallel safely.
//!
//! - **Pool-needing** tests (`run_checked_in`, `fake_apply_in`,
//!   `fake_initial_in`) share one `OnceCell<()>` boot exactly like
//!   `tests/migrate.rs` does. The shared pool is the process-wide
//!   `OnceLock`; each test uses **unique** table and migration names so
//!   their side-effects on the shared `umbral_migrations` table never
//!   collide.
//!
//! Functions under test (all from `umbral::migrate`):
//!   `detect_drift` / `detect_all_drift` / `DriftReport`
//!   `MigrationStatus` variants
//!   `run_checked_in`
//!   `fake_apply_in`
//!   `fake_initial_in`

#![allow(dead_code, private_interfaces)]

use std::collections::HashSet;
use std::path::Path;

use tokio::sync::{Mutex, OnceCell};

use umbral::migrate::{
    APP_PLUGIN_NAME, Column, DriftReport, MigrateError, MigrationEntry, MigrationFile,
    MigrationStatus, Operation, Snapshot, detect_drift, fake_apply_in, fake_initial_in,
    run_checked_in,
};
use umbral::orm::SqlType;
use umbral_core::orm::Post;

// =========================================================================
// Shared boot for pool-needing tests (same pattern as tests/migrate.rs).
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();

/// Mutex that serializes every pool-needing test within this binary.
/// SQLite in-memory pools don't handle concurrent DDL well; acquiring
/// the lock before each pool-needing test prevents "database schema is
/// locked" failures. The same pattern the `backup.rs` tests use.
static POOL_LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .build()
            .expect("App::build() should succeed");
    })
    .await;
}

// =========================================================================
// Helpers
// =========================================================================

/// Write a minimal `MigrationFile` (one CreateTable op for `table`) into
/// `<dir>/<plugin>/<id>.json`, creating the directory as needed.
fn write_migration(dir: &Path, plugin: &str, id: &str, table: &str) {
    let plugin_dir = dir.join(plugin);
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin_dir");
    let file = MigrationFile {
        id: id.to_string(),
        plugin: plugin.to_string(),
        depends_on: Vec::new(),
        operations: vec![Operation::CreateTable {
            table: table.to_string(),
            columns: vec![
                Column {
                    name: "id".to_string(),
                    ty: SqlType::BigInt,
                    primary_key: true,
                    nullable: false,
                    fk_target: None,
                    noform: false,
                    db_constraint: true,
                    noedit: false,
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
                    name: "title".to_string(),
                    ty: SqlType::Text,
                    primary_key: false,
                    nullable: false,
                    fk_target: None,
                    noform: false,
                    db_constraint: true,
                    noedit: false,
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
                    help: String::new(),
                    example: String::new(),
                    widget: None,
                    supported_backends: Vec::new(),
                    min: None,
                    max: None,
                    text_format: ::core::option::Option::None,
                    slug_from: ::core::option::Option::None,
                },
            ],
            unique_together: Vec::new(),
            indexes: Vec::new(),
        }],
        snapshot_after: Snapshot::default(),
    };
    let json = serde_json::to_string_pretty(&file).expect("serialize");
    std::fs::write(plugin_dir.join(format!("{id}.json")), json).expect("write migration");
}

/// Build a `HashSet<(String, String)>` from a slice of `(&str, &str)`.
fn set(pairs: &[(&str, &str)]) -> HashSet<(String, String)> {
    pairs
        .iter()
        .map(|(p, n)| (p.to_string(), n.to_string()))
        .collect()
}

/// Mark one migration applied in the SHARED pool's `umbral_migrations`.
/// Also ensures the tracking table exists (it is created by
/// `run_in_sqlite_checked` / `ensure_tracking_table_sqlite`, but the
/// tests that insert ghost rows before calling `run_checked_in` need the
/// table to exist earlier).
/// Callers must use unique names to avoid cross-test pollution.
async fn mark_applied_shared(plugin: &str, name: &str) {
    let pool = umbral::db::pool();
    // Idempotent DDL — safe to run even if the table already exists.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS umbral_migrations (\
            plugin TEXT NOT NULL, \
            name TEXT NOT NULL, \
            applied_at TEXT NOT NULL, \
            snapshot_hash TEXT NOT NULL, \
            PRIMARY KEY (plugin, name)\
         )",
    )
    .execute(&pool)
    .await
    .expect("ensure umbral_migrations table");
    sqlx::query(
        "INSERT OR IGNORE INTO umbral_migrations (plugin, name, applied_at, snapshot_hash) \
         VALUES (?, ?, '2026-01-01T00:00:00Z', 'deadbeef')",
    )
    .bind(plugin)
    .bind(name)
    .execute(&pool)
    .await
    .expect("mark_applied_shared");
}

// =========================================================================
// Pure drift-detection tests (no pool required)
// =========================================================================

/// `detect_drift` must classify a migration as `AppliedButMissing` when it
/// appears in the tracking set but has no file on disk. `DriftReport::
/// has_critical_drift()` must return true.
#[test]
fn drift_detected_when_tracking_table_has_migration_missing_on_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // "0001_initial" is in the applied set but no file exists.
    let applied = set(&[(APP_PLUGIN_NAME, "0001_initial")]);
    let plugin_dir = tmp.path().join(APP_PLUGIN_NAME);
    // Don't create the plugin_dir — it's deliberately absent.

    let entries = detect_drift(APP_PLUGIN_NAME, &applied, &plugin_dir)
        .expect("detect_drift should succeed even when dir is absent");

    assert_eq!(
        entries.len(),
        1,
        "one entry expected (the missing migration); got {entries:?}",
    );
    assert_eq!(entries[0].plugin, APP_PLUGIN_NAME);
    assert_eq!(entries[0].name, "0001_initial");
    assert_eq!(
        entries[0].status,
        MigrationStatus::AppliedButMissing,
        "a tracking-set row with no file must be AppliedButMissing",
    );

    let report = DriftReport { entries };
    assert!(
        report.has_critical_drift(),
        "has_critical_drift must be true when any AppliedButMissing entry exists",
    );
    assert_eq!(report.missing_on_disk().len(), 1);
}

/// `detect_drift` must classify a migration as `OutOfOrder` when the file
/// exists on disk with a sequence number lower than the max applied sequence
/// number for the plugin, and the migration is not in the applied set.
///
/// Scenario:
///   Applied: 0001_initial (seq 1) + 0003_later (seq 3)
///   On disk: 0001_initial, 0002_restored, 0003_later
///
/// Result: 0001 → Applied, 0002 → OutOfOrder (seq 2 < max 3), 0003 → Applied.
#[test]
fn detect_drift_classifies_out_of_order_correctly() {
    let tmp = tempfile::tempdir().expect("tempdir");

    write_migration(tmp.path(), APP_PLUGIN_NAME, "0001_initial", "oo_t1");
    write_migration(tmp.path(), APP_PLUGIN_NAME, "0002_restored", "oo_t2");
    write_migration(tmp.path(), APP_PLUGIN_NAME, "0003_later", "oo_t3");

    let applied = set(&[
        (APP_PLUGIN_NAME, "0001_initial"),
        (APP_PLUGIN_NAME, "0003_later"),
    ]);

    let entries = detect_drift(APP_PLUGIN_NAME, &applied, &tmp.path().join(APP_PLUGIN_NAME))
        .expect("detect_drift should not error");

    let find = |name: &str| -> &MigrationEntry {
        entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("entry `{name}` not found in {entries:?}"))
    };

    assert_eq!(find("0001_initial").status, MigrationStatus::Applied);
    assert_eq!(
        find("0002_restored").status,
        MigrationStatus::OutOfOrder,
        "0002 has seq 2 < max_applied_seq 3, so it must be OutOfOrder",
    );
    assert_eq!(find("0003_later").status, MigrationStatus::Applied);
}

/// A migration that is on disk with a sequence number higher than the last
/// applied migration must be classified as `Pending` (the normal case).
#[test]
fn detect_drift_classifies_pending_correctly() {
    let tmp = tempfile::tempdir().expect("tempdir");

    write_migration(tmp.path(), APP_PLUGIN_NAME, "0001_initial", "p_t1");
    write_migration(tmp.path(), APP_PLUGIN_NAME, "0002_pending", "p_t2");

    let applied = set(&[(APP_PLUGIN_NAME, "0001_initial")]);

    let entries = detect_drift(APP_PLUGIN_NAME, &applied, &tmp.path().join(APP_PLUGIN_NAME))
        .expect("detect_drift should not error");

    let find = |name: &str| -> &MigrationEntry {
        entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("entry `{name}` not found in {entries:?}"))
    };

    assert_eq!(find("0001_initial").status, MigrationStatus::Applied);
    assert_eq!(
        find("0002_pending").status,
        MigrationStatus::Pending,
        "0002 has seq 2 > max_applied_seq 1 and is not in the DB, so it must be Pending",
    );
}

/// `DriftReport` must NOT report critical drift when the only drift is
/// `OutOfOrder` or `Pending` — those are non-critical states.
#[test]
fn drift_report_no_critical_drift_for_out_of_order_or_pending() {
    let tmp = tempfile::tempdir().expect("tempdir");

    write_migration(tmp.path(), APP_PLUGIN_NAME, "0001_initial", "ncd_t1");
    write_migration(tmp.path(), APP_PLUGIN_NAME, "0002_pending", "ncd_t2");

    let applied = set(&[(APP_PLUGIN_NAME, "0001_initial")]);

    let entries = detect_drift(APP_PLUGIN_NAME, &applied, &tmp.path().join(APP_PLUGIN_NAME))
        .expect("detect_drift should not error");

    let report = DriftReport { entries };
    assert!(
        !report.has_critical_drift(),
        "OutOfOrder and Pending must not be critical drift; got {:?}",
        report.entries,
    );
}

// =========================================================================
// Pool-needing tests (shared boot via OnceCell)
// =========================================================================

/// `run_checked_in` with `allow_drift = false` must return
/// `MigrateError::DriftDetected` when there is a migration in the tracking
/// table that has no corresponding file on disk.
#[tokio::test]
async fn run_checked_in_errors_on_drift_without_allow_drift() {
    boot().await;
    let _lock = POOL_LOCK.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");

    // Mark a ghost migration as applied (unique name to avoid cross-test pollution).
    mark_applied_shared(APP_PLUGIN_NAME, "0001_ghost_rce").await;

    // Write a genuinely-pending migration.
    write_migration(
        tmp.path(),
        APP_PLUGIN_NAME,
        "0002_pending_rce",
        "rce_pending_table",
    );

    let err = run_checked_in(tmp.path(), false)
        .await
        .expect_err("should error on drift without allow_drift");

    match err {
        MigrateError::DriftDetected { missing } => {
            let has_ghost = missing
                .iter()
                .any(|(p, n)| p == APP_PLUGIN_NAME && n == "0001_ghost_rce");
            assert!(
                has_ghost,
                "DriftDetected must name the ghost migration; got {missing:?}",
            );
        }
        other => panic!("expected MigrateError::DriftDetected, got {other:?}"),
    }
}

/// `run_checked_in` with `allow_drift = true` must NOT error and must apply
/// the genuinely-pending migrations even when some applied migrations are
/// missing on disk.
#[tokio::test]
async fn run_checked_in_proceeds_with_allow_drift() {
    boot().await;
    let _lock = POOL_LOCK.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");

    // Mark another ghost migration (unique name).
    mark_applied_shared(APP_PLUGIN_NAME, "0001_ghost_rad").await;

    // Write a genuinely-pending migration (unique table name).
    write_migration(
        tmp.path(),
        APP_PLUGIN_NAME,
        "0002_pending_rad",
        "rad_allow_table",
    );

    let n = run_checked_in(tmp.path(), true)
        .await
        .expect("run_checked_in with allow_drift should not error");

    // At least the pending migration for this test was applied (n may be
    // higher if other pending migrations were in the directory — but this
    // tempdir is fresh so exactly one migration is in it).
    assert!(
        n >= 1,
        "at least one migration should have been applied; got {n}",
    );

    // The pending migration is tracked.
    let pool = umbral::db::pool();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT plugin, name FROM umbral_migrations WHERE name = '0002_pending_rad'")
            .fetch_all(&pool)
            .await
            .expect("select");
    assert_eq!(
        rows.len(),
        1,
        "0002_pending_rad should be tracked after apply with allow_drift",
    );

    // The table was created (DDL ran).
    let exists: Option<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='rad_allow_table'",
    )
    .fetch_optional(&pool)
    .await
    .expect("sqlite_master check");
    assert!(
        exists.is_some(),
        "rad_allow_table should exist after apply with allow_drift",
    );
}

/// `fake_apply_in` must insert a tracking row without running the migration's
/// DDL. After the call, `umbral_migrations` has the row but the target table
/// does NOT exist.
#[tokio::test]
async fn fake_apply_in_marks_applied_without_schema_change() {
    boot().await;
    let _lock = POOL_LOCK.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");

    write_migration(
        tmp.path(),
        APP_PLUGIN_NAME,
        "0001_fake_fa",
        "fa_target_table",
    );

    fake_apply_in(APP_PLUGIN_NAME, "0001_fake_fa", tmp.path())
        .await
        .expect("fake_apply_in should succeed");

    let pool = umbral::db::pool();

    // Tracking row exists.
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT plugin, name FROM umbral_migrations WHERE name = '0001_fake_fa'")
            .fetch_all(&pool)
            .await
            .expect("select");
    assert_eq!(
        rows.len(),
        1,
        "tracking table should have the fake-applied row; got {rows:?}",
    );

    // The table was NOT created.
    let exists: Option<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='fa_target_table'",
    )
    .fetch_optional(&pool)
    .await
    .expect("sqlite_master check");
    assert!(
        exists.is_none(),
        "fake_apply_in must not run DDL; fa_target_table should not exist",
    );

    // Idempotent — a second call must not insert a duplicate row.
    fake_apply_in(APP_PLUGIN_NAME, "0001_fake_fa", tmp.path())
        .await
        .expect("second fake_apply_in should succeed");
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM umbral_migrations WHERE name = '0001_fake_fa'")
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(
        count, 1,
        "idempotent fake_apply_in must not insert duplicate rows; got {count}",
    );
}

/// `fake_initial_in` must return 0 and NOT insert a tracking row when the
/// tables the `0001_*` migration would create do not yet exist in the DB.
#[tokio::test]
async fn fake_initial_in_skips_when_tables_absent() {
    boot().await;
    let _lock = POOL_LOCK.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");

    // Use a table name that definitely doesn't exist.
    write_migration(
        tmp.path(),
        APP_PLUGIN_NAME,
        "0001_fi_absent",
        "fi_absent_table_xyz",
    );

    let n = fake_initial_in(tmp.path())
        .await
        .expect("fake_initial_in should not error");

    assert_eq!(
        n, 0,
        "fake_initial_in should return 0 when tables are absent; got {n}",
    );

    let pool = umbral::db::pool();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT plugin, name FROM umbral_migrations WHERE name = '0001_fi_absent'")
            .fetch_all(&pool)
            .await
            .expect("select");
    assert!(
        rows.is_empty(),
        "tracking table should not have the row when tables are absent; got {rows:?}",
    );
}

/// `fake_initial_in` must insert a tracking row when the `0001_*`
/// migration's target tables already exist in the DB. No DDL should re-run.
#[tokio::test]
async fn fake_initial_in_marks_applied_when_tables_exist() {
    boot().await;
    let _lock = POOL_LOCK.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");

    write_migration(
        tmp.path(),
        APP_PLUGIN_NAME,
        "0001_fi_exists",
        "fi_existing_tbl",
    );

    // Manually create the target table.
    let pool = umbral::db::pool();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS fi_existing_tbl (id INTEGER PRIMARY KEY, title TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("create fi_existing_tbl");

    let n = fake_initial_in(tmp.path())
        .await
        .expect("fake_initial_in should succeed");

    assert_eq!(
        n, 1,
        "fake_initial_in should return 1 when tables exist; got {n}",
    );

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT plugin, name FROM umbral_migrations WHERE name = '0001_fi_exists'")
            .fetch_all(&pool)
            .await
            .expect("select");
    assert_eq!(
        rows.len(),
        1,
        "tracking table should have the row after fake_initial_in; got {rows:?}",
    );

    // Idempotent.
    let n2 = fake_initial_in(tmp.path())
        .await
        .expect("second fake_initial_in should succeed");
    assert_eq!(
        n2, 0,
        "second call should be a no-op (already in tracking table); got {n2}",
    );
}
