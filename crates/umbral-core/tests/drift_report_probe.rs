//! Kikosi #5 / gaps3 #38 — `migrate::drift_report` is the read-only migration
//! status a readiness probe needs.
//!
//! `show()` computes the same thing but prints to stdout and is therefore
//! useless inside a `/readyz` handler. `drift_report()` returns the structured
//! `DriftReport` instead. This binary drives `drift_report_in` against a temp
//! migrations tree and pins the one property a health check depends on:
//! `DriftReport::pending()` lists exactly the on-disk migrations the database
//! has not applied — the migrations that must hold traffic off a fresh pod —
//! and nothing else.
//!
//! `App::build()` publishes process-global `OnceLock`s and panics on a second
//! call, so the whole binary shares one boot.

use std::path::Path;

use tokio::sync::OnceCell;
use umbral::migrate::{
    APP_PLUGIN_NAME, Column, MigrationFile, MigrationStatus, Operation, Snapshot, drift_report_in,
    fake_apply_in,
};
use umbral::orm::SqlType;
use umbral_core::orm::Post;

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .build()
            .expect("App::build");
    })
    .await;
}

/// Write a minimal on-disk migration file under `dir/<plugin>/<id>.json`.
fn write_migration(dir: &Path, plugin: &str, id: &str, table: &str) {
    let plugin_dir = dir.join(plugin);
    std::fs::create_dir_all(&plugin_dir).expect("mkdir");
    let file = MigrationFile {
        id: id.to_string(),
        plugin: plugin.to_string(),
        depends_on: Vec::new(),
        operations: vec![Operation::CreateTable {
            table: table.to_string(),
            columns: vec![Column {
                name: "id".to_string(),
                ty: SqlType::BigInt,
                primary_key: true,
                ..Column::default()
            }],
            unique_together: Vec::new(),
            indexes: Vec::new(),
        }],
        snapshot_after: Snapshot::default(),
        replaces: Vec::new(),
    };
    std::fs::write(
        plugin_dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&file).expect("serialize"),
    )
    .expect("write migration");
}

/// The readiness lifecycle in one deterministic sequence: a fresh on-disk
/// migration is `Pending` (holds traffic off the pod), then after it is recorded
/// it is `Applied` and no longer pending (the pod goes ready).
///
/// One test, not two, on purpose: `fake_apply` writes to the process-global
/// `umbral_migrations` table, which is shared across every test in this binary.
/// Splitting the two steps into separate concurrent tests let one test's applied
/// row change the other's status computation — a `9001` file reads as
/// `OutOfOrder` once a sibling has recorded a higher-numbered `9002`. This is the
/// only test here that writes to the tracking table.
#[tokio::test]
async fn a_pending_migration_becomes_applied() {
    boot().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    write_migration(tmp.path(), APP_PLUGIN_NAME, "9001_probe", "drp_probe");

    // Not yet recorded → Pending, and `pending()` surfaces it.
    let report = drift_report_in(tmp.path()).await.expect("drift_report");
    let pending = report.pending();
    assert_eq!(
        pending.len(),
        1,
        "one unapplied migration; got {:?}",
        report.entries
    );
    assert_eq!(pending[0].plugin, APP_PLUGIN_NAME);
    assert_eq!(pending[0].name, "9001_probe");
    assert_eq!(pending[0].status, MigrationStatus::Pending);

    // Record it (no SQL) → Applied, and nothing pending.
    fake_apply_in(APP_PLUGIN_NAME, "9001_probe", tmp.path())
        .await
        .expect("fake_apply");

    let report = drift_report_in(tmp.path()).await.expect("drift_report");
    assert!(
        report.pending().is_empty(),
        "a recorded migration must not be pending; got {:?}",
        report.entries,
    );
    assert!(
        report
            .entries
            .iter()
            .any(|e| e.name == "9001_probe" && e.status == MigrationStatus::Applied),
        "the recorded migration should show Applied; got {:?}",
        report.entries,
    );
}

/// An empty / absent migrations tree yields zero pending — the "nothing to
/// migrate, serve away" case a readiness probe must treat as ready.
#[tokio::test]
async fn an_empty_tree_has_nothing_pending() {
    boot().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let report = drift_report_in(tmp.path()).await.expect("drift_report");
    assert!(
        report.pending().is_empty(),
        "no migration files means nothing pending; got {:?}",
        report.entries,
    );
}
