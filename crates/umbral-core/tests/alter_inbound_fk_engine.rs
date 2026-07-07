#![allow(dead_code, private_interfaces)]

//! gaps3 #30 — the *engine-driven* reproduction of an `AlterColumn` on a SQLite
//! table with inbound foreign keys AND existing data. The gaps3 #13 test proves
//! the FK-off recipe works when applied by hand; this drives the REAL migration
//! engine (`run_in` → `apply_sqlite_migration_tx`) end to end, exactly as
//! `cargo run -- migrate` does, against a pool built by `connect_sqlite`
//! (foreign_keys=ON, like production).
//!
//! Mirrors the web3clubs_fc repro: a `fixture` hub table with several inbound
//! FKs + seeded rows, altered by a pending migration. Must APPLY, not die with
//! `FOREIGN KEY constraint failed` (SQLite error 787).

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use umbral::migrate::{Column, MigrationFile, Operation, Snapshot, run_in};
use umbral::orm::{FkAction, SqlType};

/// A dummy registered model so `App::build` publishes the ambient pool + the
/// `app` plugin order that `run_in` walks. The `fixture` hub + its children are
/// created by raw SQL below (they model the consumer's schema, not this crate's).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "aife_marker")]
struct Marker {
    id: i64,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("aife.sqlite");
        std::mem::forget(tmp);
        // connect_sqlite = the production pool config (foreign_keys=ON, WAL,
        // busy_timeout), file-backed so raw setup + run_in share one DB.
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = umbral::db::connect_sqlite(&url)
            .await
            .expect("connect_sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Marker>()
            .build()
            .expect("App::build");
    })
    .await;
}

fn col(name: &str, ty: SqlType, primary_key: bool, nullable: bool) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key,
        nullable,
        fk_target: None,
        noform: false,
        privileged: false,
        db_constraint: true,
        noedit: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: FkAction::NoAction,
        on_update: FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        help: String::new(),
        example: String::new(),
        widget: None,
        supported_backends: Vec::new(),
        min: None,
        max: None,
        text_format: None,
        slug_from: None,
    }
}

fn sqlite_pool() -> sqlx::SqlitePool {
    match umbral::db::pool_dispatched() {
        umbral::db::DbPool::Sqlite(p) => p.clone(),
        umbral::db::DbPool::Postgres(_) => unreachable!("test pool is sqlite"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn alter_column_on_hub_table_with_inbound_fks_and_data_applies() {
    boot().await;
    let pool = sqlite_pool();

    // The `fixture` hub table + THREE children that reference it, all seeded —
    // the shape that trips SQLite's rebuild under foreign_keys=ON.
    sqlx::query(
        "CREATE TABLE fixture (\
            id INTEGER PRIMARY KEY,\
            opponent TEXT NOT NULL,\
            status TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("create fixture");
    for child in ["attendance", "goal", "payment"] {
        sqlx::query(&format!(
            "CREATE TABLE {child} (\
                id INTEGER PRIMARY KEY,\
                fixture_id INTEGER NOT NULL REFERENCES fixture(id)\
             )"
        ))
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("create {child}: {e}"));
    }
    // Seed the hub + a child row per table (the data that makes 787 fire).
    sqlx::query("INSERT INTO fixture (id, opponent, status) VALUES (1, 'Rivals FC', 'scheduled')")
        .execute(&pool)
        .await
        .expect("seed fixture");
    for child in ["attendance", "goal", "payment"] {
        sqlx::query(&format!(
            "INSERT INTO {child} (id, fixture_id) VALUES (1, 1)"
        ))
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("seed {child}: {e}"));
    }

    // A pending migration that alters `fixture`: flip `opponent` NOT NULL →
    // nullable (forces the SQLite table-rebuild dance).
    let tmp = tempfile::tempdir().expect("migrations tempdir");
    let dir = tmp.path();
    let plugin_dir = dir.join("app");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir app");
    // Exactly the web3clubs_fc scenario: TWO pending alters on `fixture` in one
    // migration — a nullability flip (real rebuild) AND `status` becoming a
    // Choices enum (a choices-only delta, a no-op on SQLite per gaps3 #24). Both
    // run in one transaction under the FK-off recipe.
    let status_with_choices = {
        let mut c = col("status", SqlType::Text, false, false);
        c.choices = vec!["scheduled".into(), "live".into(), "done".into()];
        c
    };
    let opponent_flip = Operation::AlterColumn {
        table: "fixture".to_string(),
        column: "opponent".to_string(),
        new_columns: vec![
            col("id", SqlType::BigInt, true, false),
            col("opponent", SqlType::Text, false, true), // now nullable
            col("status", SqlType::Text, false, false),
        ],
        prev_columns: Some(vec![
            col("id", SqlType::BigInt, true, false),
            col("opponent", SqlType::Text, false, false),
            col("status", SqlType::Text, false, false),
        ]),
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    let status_to_choices = Operation::AlterColumn {
        table: "fixture".to_string(),
        column: "status".to_string(),
        new_columns: vec![
            col("id", SqlType::BigInt, true, false),
            col("opponent", SqlType::Text, false, true),
            status_with_choices.clone(),
        ],
        prev_columns: Some(vec![
            col("id", SqlType::BigInt, true, false),
            col("opponent", SqlType::Text, false, true),
            col("status", SqlType::Text, false, false),
        ]),
        unique_together: Vec::new(),
        indexes: Vec::new(),
    };
    let migration = MigrationFile {
        id: "0001_fixture_alters".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: vec![opponent_flip, status_to_choices],
        snapshot_after: Snapshot::default(),
        replaces: Vec::new(),
    };
    std::fs::write(
        plugin_dir.join("0001_fixture_alters.json"),
        serde_json::to_string_pretty(&migration).expect("serialize"),
    )
    .expect("write migration");

    // THE REAL PATH: run_in, exactly as `cargo run -- migrate` does. Must not 787.
    run_in(dir)
        .await
        .expect("migrate must apply an AlterColumn on a table with inbound FKs + data");

    // The hub row survived and `opponent` is now nullable; the children + their
    // FKs are intact.
    let (opponent, status): (String, String) =
        sqlx::query_as("SELECT opponent, status FROM fixture WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("fixture row survives the rebuild");
    assert_eq!(opponent, "Rivals FC");
    assert_eq!(status, "scheduled");
    for child in ["attendance", "goal", "payment"] {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {child} WHERE fixture_id = 1"
        ))
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("count {child}: {e}"));
        assert_eq!(n, 1, "{child}'s FK row is intact after the rebuild");
    }
    // The column really is nullable now.
    sqlx::query("INSERT INTO fixture (id, opponent, status) VALUES (2, NULL, 'tbd')")
        .execute(&pool)
        .await
        .expect("opponent is nullable after the alter");

    eprintln!("alter_column_on_hub_table_with_inbound_fks_and_data_applies: PASS");
}
