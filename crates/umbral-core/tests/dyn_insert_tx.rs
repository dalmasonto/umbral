//! orm_fixes #2 — `DynQuerySet::insert_json_in_tx` gives the dynamic
//! write path a true transaction variant, so a nested create (parent +
//! children) is atomic at the DB level rather than compensated after the
//! fact.
//!
//! The contract these tests pin:
//!
//! 1. **Happy path** — parent + every child commit together on one tx.
//! 2. **True rollback** — when a child insert fails mid-batch, rolling
//!    the tx back leaves ZERO rows (the parent included). This is the
//!    distinction from the old compensating handler: the parent is never
//!    committed in the first place, so a process crash between the parent
//!    insert and a failing child can't orphan it.
//! 3. **Cross-row visibility** — a child whose FK points at the
//!    just-inserted (uncommitted) parent validates and inserts on the same
//!    tx, proving the FK-existence check reads the open transaction.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, OnceCell};

use umbral::orm::{DynQuerySet, ForeignKey};
use umbral_core::db;

/// Serialise tests in this binary — they share the same file-backed DB.
static SERIALISE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ditx_order")]
pub struct Order {
    pub id: i64,
    #[umbral(string)]
    pub reference: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ditx_item")]
pub struct Item {
    pub id: i64,
    pub order: ForeignKey<Order>,
    #[umbral(string, unique)]
    pub sku: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbral_dyn_insert_tx_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Order>()
            .model::<Item>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

fn order_meta() -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "ditx_order")
        .expect("registered")
}

fn item_meta() -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "ditx_item")
        .expect("registered")
}

fn obj(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    m
}

async fn count(table: &str) -> i64 {
    let meta = umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .expect("registered");
    DynQuerySet::for_meta(&meta).count().await.expect("count") as i64
}

/// Happy path: parent + two children commit atomically on one tx.
#[tokio::test]
async fn nested_insert_in_tx_commits_parent_and_all_children() {
    let _g = SERIALISE.lock().await;
    boot().await;

    let before_orders = count("ditx_order").await;
    let before_items = count("ditx_item").await;

    let mut tx = db::begin().await.expect("begin");

    let parent = DynQuerySet::for_meta(&order_meta())
        .insert_json_in_tx(
            &obj(&[("reference", Value::String("OK-1".into()))]),
            &mut tx,
        )
        .await
        .expect("parent insert");
    let pk = parent.get("id").cloned().expect("parent pk");

    for sku in ["aaa", "bbb"] {
        DynQuerySet::for_meta(&item_meta())
            .insert_json_in_tx(
                &obj(&[("order", pk.clone()), ("sku", Value::String(sku.into()))]),
                &mut tx,
            )
            .await
            .unwrap_or_else(|e| panic!("child insert {sku}: {e:?}"));
    }

    tx.commit().await.expect("commit");

    assert_eq!(count("ditx_order").await, before_orders + 1);
    assert_eq!(count("ditx_item").await, before_items + 2);
}

/// True rollback: a child insert fails (UNIQUE collision on `sku`); the
/// whole tx rolls back and NO rows land — the parent is never committed.
/// This is the regression the compensating handler could not give: the
/// parent row never exists at all, so there's nothing to orphan.
#[tokio::test]
async fn nested_insert_in_tx_rolls_back_parent_on_child_failure() {
    let _g = SERIALISE.lock().await;
    boot().await;

    // Pre-existing item with sku "dup" so the second child collides.
    let mut seed = db::begin().await.expect("begin");
    let seed_parent = DynQuerySet::for_meta(&order_meta())
        .insert_json_in_tx(
            &obj(&[("reference", Value::String("SEED".into()))]),
            &mut seed,
        )
        .await
        .expect("seed parent");
    let seed_pk = seed_parent.get("id").cloned().unwrap();
    DynQuerySet::for_meta(&item_meta())
        .insert_json_in_tx(
            &obj(&[("order", seed_pk), ("sku", Value::String("dup".into()))]),
            &mut seed,
        )
        .await
        .expect("seed item");
    seed.commit().await.expect("seed commit");

    let before_orders = count("ditx_order").await;
    let before_items = count("ditx_item").await;

    // Now attempt a nested create whose second child reuses "dup".
    let mut tx = db::begin().await.expect("begin");
    let parent = DynQuerySet::for_meta(&order_meta())
        .insert_json_in_tx(
            &obj(&[("reference", Value::String("ROLLBACK".into()))]),
            &mut tx,
        )
        .await
        .expect("parent insert");
    let pk = parent.get("id").cloned().expect("parent pk");

    // First child OK.
    DynQuerySet::for_meta(&item_meta())
        .insert_json_in_tx(
            &obj(&[
                ("order", pk.clone()),
                ("sku", Value::String("fresh".into())),
            ]),
            &mut tx,
        )
        .await
        .expect("first child");

    // Second child collides on UNIQUE sku -> Err.
    let err = DynQuerySet::for_meta(&item_meta())
        .insert_json_in_tx(
            &obj(&[("order", pk.clone()), ("sku", Value::String("dup".into()))]),
            &mut tx,
        )
        .await;
    assert!(err.is_err(), "duplicate sku must fail the child insert");

    // Roll the whole thing back.
    tx.rollback().await.expect("rollback");

    // Nothing landed — parent included.
    assert_eq!(
        count("ditx_order").await,
        before_orders,
        "parent must NOT be committed after rollback"
    );
    assert_eq!(
        count("ditx_item").await,
        before_items,
        "no child may be committed after rollback"
    );
}

/// FK-existence validation reads the open transaction: a child whose FK
/// targets the just-inserted (uncommitted) parent passes validation and
/// inserts. If validation ran on the ambient pool it would not see the
/// uncommitted parent and would reject the child as a dangling FK.
#[tokio::test]
async fn child_fk_validates_against_uncommitted_parent_in_same_tx() {
    let _g = SERIALISE.lock().await;
    boot().await;

    let mut tx = db::begin().await.expect("begin");
    let parent = DynQuerySet::for_meta(&order_meta())
        .insert_json_in_tx(
            &obj(&[("reference", Value::String("FKVIS".into()))]),
            &mut tx,
        )
        .await
        .expect("parent insert");
    let pk = parent.get("id").cloned().expect("parent pk");

    let child = DynQuerySet::for_meta(&item_meta())
        .insert_json_in_tx(
            &obj(&[
                ("order", pk.clone()),
                ("sku", Value::String("fkvis".into())),
            ]),
            &mut tx,
        )
        .await;
    assert!(
        child.is_ok(),
        "child FK should validate against the uncommitted parent: {child:?}"
    );
    tx.commit().await.expect("commit");
}
