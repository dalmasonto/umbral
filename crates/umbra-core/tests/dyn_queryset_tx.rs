//! Transaction-aware form writes on `DynQuerySet`:
//! `insert_form_in_tx` / `update_form_in_tx` / `delete_in_tx`.
//!
//! These let the admin save a parent row + its inline children atomically:
//! every statement runs on a caller-owned `db::Transaction`, so the whole
//! batch commits or rolls back as a unit. The load-bearing test is the
//! rollback one — it proves the writes are genuinely on the open tx (not
//! auto-committed on a fresh pool connection).

#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell};

use umbra::orm::{DynQuerySet, ForeignKey};
use umbra_core::db;

/// Serialise tests in this binary — they share the same file-backed DB.
static SERIALISE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "dqtx_order")]
pub struct Order {
    pub id: i64,
    #[umbra(string)]
    pub reference: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "dqtx_item")]
pub struct Item {
    pub id: i64,
    pub order: ForeignKey<Order>,
    #[umbra(string, unique)]
    pub sku: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbra_dyn_queryset_tx_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Order>()
            .model::<Item>()
            .build()
            .expect("App::build");
        sqlx::query(
            "CREATE TABLE dqtx_order (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                reference TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE order");
        sqlx::query(
            "CREATE TABLE dqtx_item (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                \"order\" INTEGER NOT NULL REFERENCES dqtx_order(id),
                sku TEXT NOT NULL UNIQUE
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE item");
    })
    .await;
}

fn order_meta() -> umbra::migrate::ModelMeta {
    umbra::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "dqtx_order")
        .expect("registered")
}

fn item_meta() -> umbra::migrate::ModelMeta {
    umbra::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "dqtx_item")
        .expect("registered")
}

fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

async fn count(table: &str) -> i64 {
    let meta = umbra::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .expect("registered");
    DynQuerySet::for_meta(&meta).count().await.expect("count") as i64
}

/// Read a single string column for the row matched by `reference` /
/// `sku`. Returns `None` when no row matches.
async fn first_value(table: &str, filter_col: &str, filter_val: &str, col: &str) -> Option<String> {
    let meta = umbra::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .expect("registered");
    let mut rows = DynQuerySet::for_meta(&meta)
        .select_cols(&[col.to_string()])
        .filter_eq_string(filter_col, filter_val)
        .fetch_as_strings()
        .await
        .expect("fetch");
    rows.pop().and_then(|mut r| r.remove(col))
}

/// Commit path: parent + child (FK to the just-inserted parent) both go
/// in on one tx; after commit both are readable.
#[tokio::test]
async fn insert_form_in_tx_commits_parent_and_child() {
    let _g = SERIALISE.lock().await;
    boot().await;

    let before_orders = count("dqtx_order").await;
    let before_items = count("dqtx_item").await;

    let mut tx = db::begin().await.expect("begin");

    let parent_pk = DynQuerySet::for_meta(&order_meta())
        .insert_form_in_tx(&mut tx, &form(&[("reference", "COMMIT-1")]), &[])
        .await
        .expect("parent insert");
    assert!(parent_pk > 0, "auto-increment PK should come back > 0");

    let child_pk = DynQuerySet::for_meta(&item_meta())
        .insert_form_in_tx(
            &mut tx,
            &form(&[("order", &parent_pk.to_string()), ("sku", "commit-sku")]),
            &[],
        )
        .await
        .expect("child insert");
    assert!(child_pk > 0);

    tx.commit().await.expect("commit");

    assert_eq!(count("dqtx_order").await, before_orders + 1);
    assert_eq!(count("dqtx_item").await, before_items + 1);
    assert_eq!(
        first_value("dqtx_item", "sku", "commit-sku", "order")
            .await
            .as_deref(),
        Some(parent_pk.to_string().as_str()),
        "child FK should point at the committed parent"
    );
}

/// Rollback atomicity — the load-bearing test. A row inserted on the tx
/// and then rolled back must NOT persist. If `insert_form_in_tx` had
/// auto-committed on a fresh pool connection, the row would survive the
/// rollback and this assertion would fail.
#[tokio::test]
async fn insert_form_in_tx_rolls_back_uncommitted_row() {
    let _g = SERIALISE.lock().await;
    boot().await;

    let before_orders = count("dqtx_order").await;

    let mut tx = db::begin().await.expect("begin");
    let pk = DynQuerySet::for_meta(&order_meta())
        .insert_form_in_tx(&mut tx, &form(&[("reference", "ROLLBACK-ME")]), &[])
        .await
        .expect("insert on tx");
    assert!(pk > 0);

    // Drop the tx by rolling it back instead of committing.
    tx.rollback().await.expect("rollback");

    assert_eq!(
        count("dqtx_order").await,
        before_orders,
        "the inserted row must NOT survive rollback — writes are on the tx"
    );
    assert!(
        first_value("dqtx_order", "reference", "ROLLBACK-ME", "id")
            .await
            .is_none(),
        "rolled-back row must be absent"
    );
}

/// update_form_in_tx + commit → the change is visible afterward.
#[tokio::test]
async fn update_form_in_tx_commit_makes_change_visible() {
    let _g = SERIALISE.lock().await;
    boot().await;

    // Seed a row to mutate.
    let mut seed = db::begin().await.expect("begin");
    let pk = DynQuerySet::for_meta(&order_meta())
        .insert_form_in_tx(&mut seed, &form(&[("reference", "UPD-BEFORE")]), &[])
        .await
        .expect("seed insert");
    seed.commit().await.expect("seed commit");

    let mut tx = db::begin().await.expect("begin");
    let affected = DynQuerySet::for_meta(&order_meta())
        .filter_eq_string("id", &pk.to_string())
        .update_form_in_tx(&mut tx, &form(&[("reference", "UPD-AFTER")]), &[])
        .await
        .expect("update on tx");
    assert_eq!(affected, 1);
    tx.commit().await.expect("commit");

    assert_eq!(
        first_value("dqtx_order", "id", &pk.to_string(), "reference")
            .await
            .as_deref(),
        Some("UPD-AFTER"),
        "committed update must be visible"
    );
}

/// delete_in_tx + rollback → the row still exists (delete was on the tx).
#[tokio::test]
async fn delete_in_tx_rollback_keeps_row() {
    let _g = SERIALISE.lock().await;
    boot().await;

    let mut seed = db::begin().await.expect("begin");
    let pk = DynQuerySet::for_meta(&order_meta())
        .insert_form_in_tx(&mut seed, &form(&[("reference", "KEEP-ME")]), &[])
        .await
        .expect("seed insert");
    seed.commit().await.expect("seed commit");

    let mut tx = db::begin().await.expect("begin");
    let affected = DynQuerySet::for_meta(&order_meta())
        .filter_eq_string("id", &pk.to_string())
        .delete_in_tx(&mut tx)
        .await
        .expect("delete on tx");
    assert_eq!(affected, 1, "the DELETE itself matched the row");
    tx.rollback().await.expect("rollback");

    assert_eq!(
        first_value("dqtx_order", "id", &pk.to_string(), "reference")
            .await
            .as_deref(),
        Some("KEEP-ME"),
        "rolled-back delete must leave the row in place"
    );
}

/// Parity: the same `form` / `skip` through `insert_form` (pool) and
/// `insert_form_in_tx` + commit produce the same persisted row.
#[tokio::test]
async fn insert_form_and_in_tx_produce_same_row() {
    let _g = SERIALISE.lock().await;
    boot().await;

    // Pool path.
    let pool_pk = DynQuerySet::for_meta(&order_meta())
        .insert_form(&form(&[("reference", "PARITY")]), &[])
        .await
        .expect("pool insert");

    // Tx path with the identical form + skip.
    let mut tx = db::begin().await.expect("begin");
    let tx_pk = DynQuerySet::for_meta(&order_meta())
        .insert_form_in_tx(&mut tx, &form(&[("reference", "PARITY")]), &[])
        .await
        .expect("tx insert");
    tx.commit().await.expect("commit");

    let pool_ref = first_value("dqtx_order", "id", &pool_pk.to_string(), "reference").await;
    let tx_ref = first_value("dqtx_order", "id", &tx_pk.to_string(), "reference").await;
    assert_eq!(pool_ref.as_deref(), Some("PARITY"));
    assert_eq!(
        pool_ref, tx_ref,
        "pool and tx insert of the same form persist the same column value"
    );
}

/// Validation still surfaces as `DynError` from the tx path, and the tx
/// rolls back cleanly afterward. A child FK to a non-existent parent is
/// rejected; an earlier good insert on the same tx is not persisted once
/// we roll back.
#[tokio::test]
async fn validation_error_surfaces_from_in_tx_and_rolls_back() {
    let _g = SERIALISE.lock().await;
    boot().await;

    let before_orders = count("dqtx_order").await;
    let before_items = count("dqtx_item").await;

    let mut tx = db::begin().await.expect("begin");

    // A good parent insert first.
    let parent_pk = DynQuerySet::for_meta(&order_meta())
        .insert_form_in_tx(&mut tx, &form(&[("reference", "VAL-PARENT")]), &[])
        .await
        .expect("parent insert");
    assert!(parent_pk > 0);

    // Child FK points at a parent id that does not exist → error.
    let bad = DynQuerySet::for_meta(&item_meta())
        .insert_form_in_tx(
            &mut tx,
            &form(&[("order", "999999"), ("sku", "bad-fk")]),
            &[],
        )
        .await;
    assert!(bad.is_err(), "dangling FK must surface as an error");

    // Roll back the whole unit; nothing — including the good parent —
    // may persist.
    tx.rollback().await.expect("rollback");

    assert_eq!(
        count("dqtx_order").await,
        before_orders,
        "good parent must not persist after a rolled-back unit"
    );
    assert_eq!(count("dqtx_item").await, before_items);
}
