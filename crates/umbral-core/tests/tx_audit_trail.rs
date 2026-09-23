//! gaps6 #16 — `#[umbral(audited)]` writes made inside `on_tx()` must be
//! recorded in `umbral_audit`, exactly like the non-transactional path.
//!
//! Two properties that make this NOT a trivial "call the same helper":
//!
//! 1. **The audit row must be written through the SAME transaction.** On
//!    SQLite only one writer may hold the database at a time, so an
//!    ambient-pool INSERT into `umbral_audit` while the caller's write tx is
//!    still open would deadlock (block on the open writer, then time out).
//!    Writing through the tx also means the audit row **rolls back with the
//!    business write** — a rolled-back tx leaves no audit trail for a change
//!    that never happened.
//!
//! 2. **The after-image must be read through the tx connection.** A separate
//!    connection cannot see the tx's own uncommitted UPDATE, so an
//!    ambient-pool after-read would record a stale (pre-update) after-image
//!    and the audit row would claim the update changed nothing.
//!
//! These tests drive create / update / delete / soft-delete inside
//! `db::transaction_sqlite(...)` and assert the audit trail matches the
//! non-tx contract, plus the rollback case that proves the audit row is
//! transactional.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "tx_audit_invoice", audited)]
pub struct TxAuditInvoice {
    pub id: i64,
    pub label: String,
    pub amount: i64,
}

/// Audited AND soft-delete: a soft delete inside a tx must still log as DELETE.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "tx_audit_note", audited, soft_delete)]
pub struct TxAuditNote {
    pub id: i64,
    pub body: String,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug)]
struct BoomError;
impl std::fmt::Display for BoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "boom")
    }
}
impl std::error::Error for BoomError {}
impl From<sqlx::Error> for BoomError {
    fn from(_: sqlx::Error) -> Self {
        BoomError
    }
}
impl From<umbral::orm::write::WriteError> for BoomError {
    fn from(_: umbral::orm::write::WriteError) -> Self {
        BoomError
    }
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() -> sqlx::SqlitePool {
    static ONCE: tokio::sync::OnceCell<sqlx::SqlitePool> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tx_audit_trail.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<TxAuditInvoice>()
            .model::<TxAuditNote>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        pool
    })
    .await
    .clone()
}

/// Audit rows for one table, oldest first.
async fn entries(table: &str) -> Vec<(String, Value)> {
    let pool = umbral::db::pool();
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT action, changes FROM umbral_audit WHERE table_name = ? ORDER BY id",
    )
    .bind(table)
    .fetch_all(&pool)
    .await
    .expect("select audit");
    rows.into_iter()
        .map(|(a, c)| (a, serde_json::from_str(&c).unwrap_or(Value::Null)))
        .collect()
}

async fn clear_audit() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM umbral_audit")
        .execute(&pool)
        .await
        .expect("clear");
}

/// The crux: create → update → delete an audited model, all inside one
/// `on_tx()` transaction that commits, and every write lands in the audit
/// trail — with the update's after-image read correctly through the tx.
#[tokio::test]
async fn tx_create_update_delete_are_all_audited_after_commit() {
    let _g = lock().lock().await;
    let pool = boot().await;
    clear_audit().await;

    db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            let created = TxAuditInvoice::objects()
                .on_tx(tx)
                .create(TxAuditInvoice {
                    id: 0,
                    label: "acme".into(),
                    amount: 100,
                })
                .await?;

            TxAuditInvoice::objects()
                .filter(tx_audit_invoice::ID.eq(created.id))
                .on_tx(tx)
                .update_values(json!({"amount": 250}).as_object().unwrap().clone())
                .await?;

            TxAuditInvoice::objects()
                .filter(tx_audit_invoice::ID.eq(created.id))
                .on_tx(tx)
                .delete()
                .await?;

            Ok::<_, BoomError>(())
        })
    })
    .await
    .expect("transaction commits");

    let log = entries("tx_audit_invoice").await;
    let actions: Vec<&str> = log.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(
        actions,
        vec!["create", "update", "delete"],
        "every write inside on_tx() must be audited, in order; got: {log:?}",
    );

    // The update's after-image must be read through the tx — an ambient-pool
    // read would not see the uncommitted UPDATE and record `to = 100`.
    let (_, changes) = &log[1];
    assert_eq!(changes["amount"]["from"], json!(100), "got: {changes}");
    assert_eq!(
        changes["amount"]["to"],
        json!(250),
        "the after-image must reflect the in-tx UPDATE; got: {changes}",
    );
}

/// A rolled-back transaction must leave NO audit rows — the audit row is
/// written through the same tx, so it rolls back with the business write.
#[tokio::test]
async fn a_rolled_back_tx_writes_no_audit_rows() {
    let _g = lock().lock().await;
    let pool = boot().await;
    clear_audit().await;

    let result: Result<(), BoomError> = db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            TxAuditInvoice::objects()
                .on_tx(tx)
                .create(TxAuditInvoice {
                    id: 0,
                    label: "ghost".into(),
                    amount: 5,
                })
                .await?;
            Err(BoomError)
        })
    })
    .await;

    assert!(result.is_err(), "the closure's Err must propagate");
    assert!(
        entries("tx_audit_invoice").await.is_empty(),
        "a rolled-back tx must record no audit rows — the audit write is \
         transactional and dies with the rollback",
    );
}

/// A soft delete inside a tx is an UPDATE on the wire but must be logged as a
/// DELETE, matching the non-tx contract.
#[tokio::test]
async fn a_tx_soft_delete_is_audited_as_delete() {
    let _g = lock().lock().await;
    let pool = boot().await;
    clear_audit().await;

    let note = TxAuditNote::objects()
        .create(TxAuditNote {
            id: 0,
            body: "hi".into(),
            deleted_at: None,
        })
        .await
        .expect("seed row");

    let note_id = note.id;
    db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            TxAuditNote::objects()
                .filter(tx_audit_note::ID.eq(note_id))
                .on_tx(tx)
                .delete() // soft
                .await?;
            Ok::<_, BoomError>(())
        })
    })
    .await
    .expect("transaction commits");

    let log = entries("tx_audit_note").await;
    let actions: Vec<&str> = log.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(
        actions,
        vec!["create", "delete"],
        "a soft delete inside on_tx() must read as `delete`; got: {log:?}",
    );
}
