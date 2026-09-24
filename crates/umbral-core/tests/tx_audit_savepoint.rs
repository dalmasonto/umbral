//! gaps6 #18 — a failed in-tx audit insert must not break the caller's
//! business transaction.
//!
//! On Postgres a failed statement aborts the whole transaction (25P02), so an
//! audit insert that errors would take the business write down with it — the
//! opposite of the "don't fail the business write for the audit" posture. The
//! fix wraps the audit insert in a SAVEPOINT so a failure rolls back only the
//! audit statement.
//!
//! This test runs on SQLite (Postgres isn't wired here). SQLite is more
//! tolerant of a failed statement mid-transaction, so this may pass with or
//! without the savepoint — its job is to prove the failure is CONTAINED (the
//! business rows commit) and that the savepoint wrapping doesn't break the
//! path. It uses its own database file so it can break the audit sink (drop
//! `umbral_audit`) without disturbing other tests.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "sp_invoice", audited)]
pub struct SpInvoice {
    pub id: i64,
    pub label: String,
}

#[derive(Debug)]
struct Boom;
impl std::fmt::Display for Boom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "boom")
    }
}
impl std::error::Error for Boom {}
impl From<sqlx::Error> for Boom {
    fn from(_: sqlx::Error) -> Self {
        Boom
    }
}
impl From<umbral::orm::write::WriteError> for Boom {
    fn from(_: umbral::orm::write::WriteError) -> Self {
        Boom
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
        let path = tmp.path().join("tx_audit_savepoint.sqlite");
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
            .model::<SpInvoice>()
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

/// Break the audit sink (drop `umbral_audit`), then do two audited business
/// writes inside one transaction. Every audit insert fails — but the business
/// transaction must still commit and both rows must persist.
#[tokio::test]
async fn a_failed_in_tx_audit_insert_does_not_break_the_business_tx() {
    let _g = lock().lock().await;
    let pool = boot().await;

    sqlx::query("DROP TABLE IF EXISTS umbral_audit")
        .execute(&pool)
        .await
        .expect("drop the audit sink");

    db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            SpInvoice::objects()
                .on_tx(tx)
                .create(SpInvoice {
                    id: 0,
                    label: "a".into(),
                })
                .await?;
            SpInvoice::objects()
                .on_tx(tx)
                .create(SpInvoice {
                    id: 0,
                    label: "b".into(),
                })
                .await?;
            Ok::<_, Boom>(())
        })
    })
    .await
    .expect("the business tx must commit even though every audit insert failed");

    let n = SpInvoice::objects().count().await.expect("count");
    assert_eq!(
        n, 2,
        "both business rows must persist — a failed audit insert must not poison the tx",
    );
}
