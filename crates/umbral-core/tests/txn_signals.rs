//! gaps4 #92 — transactional after-create hooks (`subscribe_txn` / `emit_txn`).
//!
//! A hook registered with `subscribe_txn` runs INSIDE the creating transaction:
//! it can write a dependent row on the same tx, and if it returns `Err` the
//! whole create is rolled back. This is the atomic "create A ⇒ B exists"
//! invariant that async fire-and-forget `post_save` subscribers cannot give.

#![allow(dead_code)]

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex as TokioMutex, OnceCell};

use umbral::migrate::registered_models;
use umbral::orm::DynQuerySet;
use umbral_core::db;

/// Serialise the tests: they share the registry, the DB, and the signal names.
fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "tsx_parent")]
pub struct Parent {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "tsx_child")]
pub struct Child {
    pub id: i64,
    pub parent_id: i64,
    #[umbral(string)]
    pub label: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbral_txn_signals_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
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

/// A `subscribe_txn` hook creates a dependent row on the SAME transaction as the
/// parent create — both land atomically.
#[tokio::test]
async fn txn_hook_creates_dependent_row_atomically() {
    let _g = test_lock().lock().await;
    boot().await;
    umbral::signals::clear_for_tests();

    // On every Parent create, auto-create a Child on the same tx (the #92
    // "AuthUser ⇒ Profile" shape). The hook uses the parent id from the payload.
    umbral::signals::subscribe_txn("post_save:tsx_parent", |payload, tx| {
        Box::pin(async move {
            let pid = payload["instance"]["id"].clone();
            let child = registered_models()
                .into_iter()
                .find(|m| m.table == "tsx_child")
                .expect("child model registered");
            let mut body = serde_json::Map::new();
            body.insert("parent_id".to_string(), pid);
            body.insert("label".to_string(), json!("auto-created"));
            DynQuerySet::for_meta(&child)
                .insert_json_in_tx(&body, tx)
                .await?;
            Ok(())
        })
    });

    let p = Parent::objects()
        .create(Parent {
            id: 0,
            name: "Ada".into(),
        })
        .await
        .expect("parent create with txn hook");
    assert!(p.id > 0);

    let children = Child::objects()
        .filter(child::PARENT_ID.eq(p.id))
        .count()
        .await
        .unwrap();
    assert_eq!(
        children, 1,
        "the txn hook created the dependent row atomically"
    );

    umbral::signals::clear_for_tests();
}

/// A `subscribe_txn` hook that returns `Err` aborts the whole create — the
/// parent row is rolled back, never persisted.
#[tokio::test]
async fn failing_txn_hook_rolls_back_the_parent() {
    let _g = test_lock().lock().await;
    boot().await;
    umbral::signals::clear_for_tests();

    let before = Parent::objects().count().await.unwrap();

    umbral::signals::subscribe_txn("post_save:tsx_parent", |_payload, _tx| {
        Box::pin(async move {
            Err(umbral::orm::WriteError::Sqlx(sqlx::Error::Protocol(
                "hook rejected the create".to_string(),
            )))
        })
    });

    let res = Parent::objects()
        .create(Parent {
            id: 0,
            name: "Grace".into(),
        })
        .await;
    assert!(res.is_err(), "a failing txn hook must abort the create");

    assert_eq!(
        Parent::objects().count().await.unwrap(),
        before,
        "the parent was rolled back with the failed hook — nothing persisted"
    );

    umbral::signals::clear_for_tests();
}
