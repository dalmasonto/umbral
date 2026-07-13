//! Audit core-migrate #3 / #18 — `backup::load` must restore tables in
//! FK-topological order (not the dump's alphabetical order) so a child
//! table whose name sorts before its parent still loads, and must reject
//! a dump that carries two entries for the same table.
//!
//! `dump()` writes tables alphabetically: `ord_child` before
//! `ord_parent`. With `foreign_keys = ON` (the pool default) a restore
//! that follows that order fails the FK check on the first child row.
//! The fix topo-sorts by `fk_target` before loading and wraps the whole
//! restore in one transaction.

#![allow(dead_code, private_interfaces)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use umbral::backup::{BackupError, Dump, ModelDump, dump, load};
use umbral::orm::ForeignKey;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ord_parent")]
struct Parent {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ord_child")]
struct Child {
    id: i64,
    parent: ForeignKey<Parent>,
    note: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Parent>()
            .model::<Child>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

#[tokio::test]
async fn load_restores_fk_children_after_parents() {
    boot().await;
    let pool = umbral::db::pool();

    sqlx::query("INSERT INTO ord_parent (id, name) VALUES (1, 'root')")
        .execute(&pool)
        .await
        .expect("seed parent");
    sqlx::query("INSERT INTO ord_child (id, parent, note) VALUES (1, 1, 'leaf')")
        .execute(&pool)
        .await
        .expect("seed child");

    // dump() sorts by table name, so `ord_child` is written BEFORE
    // `ord_parent` — exactly the order that breaks a naive restore.
    let snapshot = dump().await.expect("dump");
    let tables: Vec<&str> = snapshot.models.iter().map(|m| m.table.as_str()).collect();
    assert_eq!(
        tables,
        vec!["ord_child", "ord_parent"],
        "dump is alphabetical: child must precede parent to exercise the bug",
    );

    // Wipe (child first — FK), then restore from the child-first dump.
    sqlx::query("DELETE FROM ord_child")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ord_parent")
        .execute(&pool)
        .await
        .unwrap();

    let report = load(&snapshot).await.expect("load restores in FK order");
    assert_eq!(report.rows_loaded, 2, "both rows restored; got {report:?}");

    let child_parent: i64 = sqlx::query_scalar("SELECT parent FROM ord_child WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("child row survived with its FK intact");
    assert_eq!(child_parent, 1);
}

#[tokio::test]
async fn load_rejects_duplicate_table_entries() {
    boot().await;

    let dup = Dump {
        umbral_dump_version: "1".to_string(),
        exported_at: "2026-07-03T00:00:00Z".to_string(),
        models: vec![
            ModelDump {
                table: "ord_parent".to_string(),
                rows: Vec::new(),
            },
            ModelDump {
                table: "ord_parent".to_string(),
                rows: Vec::new(),
            },
        ],
    };

    let err = load(&dup).await.expect_err("duplicate table must error");
    assert!(
        matches!(err, BackupError::DuplicateTable { ref table } if table == "ord_parent"),
        "expected DuplicateTable for ord_parent; got {err:?}",
    );
}
