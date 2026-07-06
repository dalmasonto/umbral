#![allow(dead_code)]

//! gaps2 #100 — `squashmigrations` end-to-end (SQLite, always-run).
//!
//! The safety-critical proof: squashing a plugin's history is **non-destructive**
//! and never double-applies.
//!
//! 1. Two schema migrations (`0001` creates `gadget`, `0002` creates `widget`)
//!    apply against a fresh DB — the "already-deployed" starting point. Real
//!    rows go in.
//! 2. `squash_in` collapses them into one optimized squash file whose ops are
//!    the diff from an EMPTY schema to the final snapshot (both `CreateTable`s),
//!    keeping the originals on disk (`replaces = [0001, 0002]`).
//! 3. Re-running `migrate` against that already-migrated DB records the squash
//!    WITHOUT re-running any DDL (RecordOnly) — the tables are not recreated and
//!    the rows from step 1 survive. This is the whole point: a squash on a live
//!    database must not drop and rebuild its tables.
//! 4. A further `migrate` is a clean no-op (everything Skip).
//!
//! This file owns its own boot/pool (separate test binary) so the process-global
//! registry/pool `OnceLock`s are exclusively ours.

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use umbral::migrate::{MigrationFile, Snapshot, SquashOutcome, diff, run_in, squash_in};

/// Two independent (FK-free) models so the plugin `app` has a two-table,
/// two-migration history to squash.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "gadget")]
struct Gadget {
    id: i64,
    label: String,
}

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
            .model::<Gadget>()
            .model::<Widget>()
            .build()
            .expect("App::build happy path");
    })
    .await;
}

/// Hand-author a two-migration history for `app` in `dir`, mirroring exactly
/// what `make` would emit across two model additions: `0001` creates the first
/// table, `0002` creates the second. Returns nothing — the files are on disk.
fn write_two_migration_history(dir: &std::path::Path) {
    let all = Snapshot::current_for("app").models; // sorted by name → [gadget, widget]
    assert_eq!(all.len(), 2, "both models registered");
    let snap1 = Snapshot {
        models: vec![all[0].clone()],
    };
    let snap2 = Snapshot { models: all };

    let empty = Snapshot::default();
    let file_0001 = MigrationFile {
        id: "0001_create_gadget".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: diff(&empty, &snap1).expect("diff empty→snap1"),
        snapshot_after: snap1.clone(),
        replaces: Vec::new(),
    };
    let file_0002 = MigrationFile {
        id: "0002_create_widget".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: diff(&snap1, &snap2).expect("diff snap1→snap2"),
        snapshot_after: snap2,
        replaces: Vec::new(),
    };

    let plugin_dir = dir.join("app");
    std::fs::create_dir_all(&plugin_dir).expect("create app dir");
    for f in [&file_0001, &file_0002] {
        let json = serde_json::to_string_pretty(f).expect("serialize migration");
        std::fs::write(plugin_dir.join(format!("{}.json", f.id)), json).expect("write migration");
    }
}

fn sqlite_pool() -> sqlx::SqlitePool {
    match umbral::db::pool_dispatched() {
        umbral::db::DbPool::Sqlite(p) => p.clone(),
        umbral::db::DbPool::Postgres(_) => unreachable!("test pool is sqlite"),
    }
}

async fn tracked(pool: &sqlx::SqlitePool, name: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM umbral_migrations WHERE plugin = 'app' AND name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("count tracking rows")
}

#[tokio::test(flavor = "multi_thread")]
async fn squash_records_without_rebuilding_an_already_migrated_database() {
    boot().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();

    // 1) A deployed database: apply the original two-migration history.
    write_two_migration_history(dir);
    let applied = run_in(dir).await.expect("apply 0001 + 0002");
    assert_eq!(applied, 2, "both original migrations applied");

    let pool = sqlite_pool();
    // Real rows go in through the created tables.
    sqlx::query("INSERT INTO gadget (id, label) VALUES (1, 'g-one')")
        .execute(&pool)
        .await
        .expect("insert gadget");
    sqlx::query("INSERT INTO widget (id, name, active) VALUES (1, 'w-one', 1)")
        .execute(&pool)
        .await
        .expect("insert widget");

    // 2) Squash the history. Non-destructive: originals stay on disk.
    let SquashOutcome { path, id, replaced } =
        squash_in(dir, "app").expect("squash the app history");
    assert_eq!(
        replaced,
        vec![
            "0001_create_gadget".to_string(),
            "0002_create_widget".to_string()
        ],
        "the squash replaces exactly the two originals"
    );
    assert!(path.exists(), "the squash file was written");
    assert!(
        dir.join("app").join("0001_create_gadget.json").exists(),
        "original 0001 is KEPT on disk (non-destructive)"
    );

    // The squash's own ops are the from-empty replay: one CreateTable per table,
    // so a FRESH database would build the whole schema in one shot.
    let squash: MigrationFile =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read squash"))
            .expect("parse squash");
    let creates = squash
        .operations
        .iter()
        .filter(|op| matches!(op, umbral::migrate::Operation::CreateTable { .. }))
        .count();
    assert_eq!(creates, 2, "squash builds both tables from scratch");
    assert_eq!(squash.replaces.len(), 2, "squash carries its replaced set");

    // 3) Re-migrate the ALREADY-migrated DB. The squash's replaced set is fully
    //    applied → RecordOnly: it records itself but runs NO DDL. Critically, the
    //    tables are NOT dropped/recreated and the rows survive.
    let after_squash = run_in(dir)
        .await
        .expect("record-only migrate must not error");
    assert_eq!(after_squash, 1, "only the squash is recorded this run");

    let gadget_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gadget")
        .fetch_one(&pool)
        .await
        .expect("count gadget");
    let widget_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM widget")
        .fetch_one(&pool)
        .await
        .expect("count widget");
    assert_eq!(
        gadget_rows, 1,
        "gadget rows SURVIVED the squash (no rebuild)"
    );
    assert_eq!(
        widget_rows, 1,
        "widget rows SURVIVED the squash (no rebuild)"
    );
    // The specific rows are intact, not just the counts.
    let (label,): (String,) = sqlx::query_as("SELECT label FROM gadget WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("gadget row intact");
    assert_eq!(label, "g-one");

    // The squash is recorded; the shadowed originals were NOT re-recorded here
    // (they were already applied in step 1).
    assert_eq!(tracked(&pool, &id).await, 1, "squash recorded once");
    assert_eq!(
        tracked(&pool, "0001_create_gadget").await,
        1,
        "0001 stays recorded (from step 1), not duplicated"
    );

    // 4) One more migrate is a clean no-op — everything is applied.
    let again = run_in(dir).await.expect("idempotent re-run");
    assert_eq!(again, 0, "nothing left to apply");

    eprintln!("squash_records_without_rebuilding_an_already_migrated_database: PASS");
}

#[tokio::test(flavor = "multi_thread")]
async fn squash_refuses_a_single_migration_history() {
    boot().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();

    // Only 0001 on disk — nothing to collapse.
    let all = Snapshot::current_for("app").models;
    let snap1 = Snapshot {
        models: vec![all[0].clone()],
    };
    let only = MigrationFile {
        id: "0001_create_gadget".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: diff(&Snapshot::default(), &snap1).expect("diff"),
        snapshot_after: snap1,
        replaces: Vec::new(),
    };
    let plugin_dir = dir.join("app");
    std::fs::create_dir_all(&plugin_dir).expect("create app dir");
    std::fs::write(
        plugin_dir.join("0001_create_gadget.json"),
        serde_json::to_string_pretty(&only).expect("serialize"),
    )
    .expect("write");

    match squash_in(dir, "app") {
        Err(umbral::migrate::MigrateError::CannotSquash { plugin, reason }) => {
            assert_eq!(plugin, "app");
            assert!(
                reason.contains("at least 2"),
                "reason names the minimum: {reason}"
            );
        }
        Ok(_) => panic!("a single-migration history must not be squashable"),
        Err(e) => panic!("expected CannotSquash, got {e}"),
    }

    eprintln!("squash_refuses_a_single_migration_history: PASS");
}
