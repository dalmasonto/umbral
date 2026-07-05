//! End-to-end tests for the atomic transaction API.
//!
//! Tests cover:
//!
//! 1. Commit on Ok — row visible after the closure returns Ok.
//! 2. Rollback on Err — row NOT visible after the closure returns Err.
//! 3. Multi-statement atomicity — two INSERTs commit together.
//! 4. Multi-statement rollback — two INSERTs roll back together on Err.
//! 5. Nested Manager/QuerySet calls — Manager::create_in_tx + QuerySet::on_tx
//!    see the uncommitted row inside the same open transaction.
//! 6. SQLite-specific: manual begin/commit via begin_sqlite.
//! 7. bulk_create_in_tx — many rows in one transactional batch.
//! 8. update_values + delete inside a transaction roll back together.
//!
//! Every test serialises through `SERIALISE` so the shared table isn't raced.
//!
//! The closure argument to `transaction_sqlite` must be `Box::pin(async move { ... })`
//! because Rust's async closures don't yet support capturing mutable references
//! across the closure boundary without boxing the returned future. This is the
//! standard workaround for stable Rust; see `umbral::db::TxFuture`.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbral::db::{begin_sqlite, transaction_sqlite};
use umbral::orm::write::WriteError;

/// Serialise all tests in this binary on a single mutex to avoid table races.
static SERIALISE: Mutex<()> = Mutex::const_new(());

// ============================================================================
// Test error type
//
// The transaction closures need an error type that can be produced by both
// sqlx operations and ORM write operations (WriteError). Define a small
// wrapper so `?` works for both inside the closure body.
// ============================================================================

#[derive(Debug)]
enum TxError {
    Sqlx(sqlx::Error),
    Write(WriteError),
    /// Sentinel used to deliberately trigger a rollback in tests.
    Deliberate,
}

impl From<sqlx::Error> for TxError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}
impl From<WriteError> for TxError {
    fn from(e: WriteError) -> Self {
        Self::Write(e)
    }
}

// ============================================================================
// Model fixture
// ============================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "tx_item")]
pub struct Item {
    pub id: i64,
    pub name: String,
    pub value: i64,
}

// ============================================================================
// Boot / helpers
// ============================================================================

static POOL: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn pool() -> sqlx::SqlitePool {
    POOL.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("transactions.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1) // single connection: avoids WAL-mode write-lock contention
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        sqlx::query(
            "CREATE TABLE tx_item (\
                id    INTEGER PRIMARY KEY AUTOINCREMENT,\
                name  TEXT    NOT NULL,\
                value INTEGER NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create tx_item");

        pool
    })
    .await
    .clone()
}

async fn truncate(pool: &sqlx::SqlitePool) {
    sqlx::query("DELETE FROM tx_item")
        .execute(pool)
        .await
        .expect("truncate tx_item");
}

async fn count_all(pool: &sqlx::SqlitePool) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tx_item")
        .fetch_one(pool)
        .await
        .expect("count");
    n
}

// ============================================================================
// Test 1 — commit on Ok
// ============================================================================

#[tokio::test]
async fn commit_on_ok_row_is_visible_after() {
    let _guard = SERIALISE.lock().await;
    let pool = pool().await;
    truncate(&pool).await;

    let result = transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            Item::objects()
                .on_tx(tx)
                .create(Item {
                    id: 0,
                    name: "alpha".into(),
                    value: 1,
                })
                .await?;
            Ok::<_, TxError>(())
        })
    })
    .await;

    assert!(result.is_ok(), "transaction should succeed: {result:?}");
    assert_eq!(count_all(&pool).await, 1, "committed row should be visible");
}

// ============================================================================
// Test 2 — rollback on Err
// ============================================================================

#[tokio::test]
async fn rollback_on_err_row_is_not_visible_after() {
    let _guard = SERIALISE.lock().await;
    let pool = pool().await;
    truncate(&pool).await;

    let result = transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            Item::objects()
                .on_tx(tx)
                .create(Item {
                    id: 0,
                    name: "beta".into(),
                    value: 2,
                })
                .await?;
            // Deliberately fail after the INSERT.
            Err::<(), TxError>(TxError::Deliberate)
        })
    })
    .await;

    assert!(result.is_err(), "transaction should fail");
    assert_eq!(
        count_all(&pool).await,
        0,
        "rolled-back row must not be visible"
    );
}

// ============================================================================
// Test 3 — multi-statement atomicity (all commit)
// ============================================================================

#[tokio::test]
async fn multi_statement_all_commit_together() {
    let _guard = SERIALISE.lock().await;
    let pool = pool().await;
    truncate(&pool).await;

    let result = transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            Item::objects()
                .on_tx(tx)
                .create(Item {
                    id: 0,
                    name: "gamma".into(),
                    value: 10,
                })
                .await?;
            Item::objects()
                .on_tx(tx)
                .create(Item {
                    id: 0,
                    name: "delta".into(),
                    value: 20,
                })
                .await?;
            Ok::<_, TxError>(())
        })
    })
    .await;

    assert!(result.is_ok());
    assert_eq!(count_all(&pool).await, 2, "both rows should be committed");
}

// ============================================================================
// Test 4 — multi-statement rollback (none commit)
// ============================================================================

#[tokio::test]
async fn multi_statement_all_rollback_on_failure() {
    let _guard = SERIALISE.lock().await;
    let pool = pool().await;
    truncate(&pool).await;

    let result = transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            Item::objects()
                .on_tx(tx)
                .create(Item {
                    id: 0,
                    name: "epsilon".into(),
                    value: 30,
                })
                .await?;
            Item::objects()
                .on_tx(tx)
                .create(Item {
                    id: 0,
                    name: "zeta".into(),
                    value: 40,
                })
                .await?;
            // Fail after both INSERTs.
            Err::<(), TxError>(TxError::Deliberate)
        })
    })
    .await;

    assert!(result.is_err());
    assert_eq!(count_all(&pool).await, 0, "both rows must be rolled back");
}

// ============================================================================
// Test 5 — create_in_tx and on_tx read see the uncommitted row
// ============================================================================

#[tokio::test]
async fn in_tx_read_sees_uncommitted_row_in_same_transaction() {
    let _guard = SERIALISE.lock().await;
    let pool = pool().await;
    truncate(&pool).await;

    // Within the transaction, insert via create_in_tx and then read via on_tx;
    // the inserted row must be visible to the in-transaction SELECT.
    let result = transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            let row = Item::objects()
                .create_in_tx(
                    Item {
                        id: 0,
                        name: "eta".into(),
                        value: 99,
                    },
                    tx,
                )
                .await?;

            // Build a predicate for the id column using IntCol directly.
            use umbral::orm::column::IntCol;
            let id_col: IntCol<Item> = IntCol::new("id");

            // Read the row back inside the same open transaction.
            let fetched = Item::objects()
                .filter(id_col.eq(row.id))
                .on_tx(tx)
                .first()
                .await?;

            Ok::<_, TxError>(fetched)
        })
    })
    .await
    .expect("transaction should succeed");

    assert!(
        result.is_some(),
        "in-tx read should return the just-inserted row"
    );
    assert_eq!(result.unwrap().name, "eta");

    // After commit the row is still visible outside the transaction.
    assert_eq!(count_all(&pool).await, 1);
}

// ============================================================================
// Test 6 — manual begin/commit via begin_sqlite
// ============================================================================

#[tokio::test]
async fn manual_begin_commit_works() {
    let _guard = SERIALISE.lock().await;
    let pool = pool().await;
    truncate(&pool).await;

    let mut tx = begin_sqlite(&pool).await.expect("begin");
    Item::objects()
        .on_tx(&mut tx)
        .create(Item {
            id: 0,
            name: "theta".into(),
            value: 7,
        })
        .await
        .expect("create in tx");
    tx.commit().await.expect("commit");

    assert_eq!(count_all(&pool).await, 1);
}

// ============================================================================
// Test 7 — bulk_create_in_tx
// ============================================================================

#[tokio::test]
async fn bulk_create_in_tx_commits_all_rows() {
    let _guard = SERIALISE.lock().await;
    let pool = pool().await;
    truncate(&pool).await;

    let items = (1..=4i64)
        .map(|i| Item {
            id: 0,
            name: format!("item{i}"),
            value: i * 100,
        })
        .collect::<Vec<_>>();

    let n = transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            let n = Item::objects().bulk_create_in_tx(items, tx).await?;
            Ok::<_, TxError>(n)
        })
    })
    .await
    .expect("bulk_create_in_tx should succeed");

    assert_eq!(n, 4, "four rows should be inserted");
    assert_eq!(count_all(&pool).await, 4);
}

// ============================================================================
// Test 8 — update_values + delete inside a transaction roll back together
// ============================================================================

#[tokio::test]
async fn update_and_delete_in_tx_roll_back_on_error() {
    let _guard = SERIALISE.lock().await;
    let pool = pool().await;
    truncate(&pool).await;

    // Seed two rows outside any transaction.
    {
        let mut seed_tx = begin_sqlite(&pool).await.expect("begin seed");
        Item::objects()
            .on_tx(&mut seed_tx)
            .create(Item {
                id: 0,
                name: "iota".into(),
                value: 1,
            })
            .await
            .expect("seed iota");
        Item::objects()
            .on_tx(&mut seed_tx)
            .create(Item {
                id: 0,
                name: "kappa".into(),
                value: 2,
            })
            .await
            .expect("seed kappa");
        seed_tx.commit().await.expect("commit seed");
    }

    assert_eq!(count_all(&pool).await, 2);

    use umbral::orm::column::StrCol;

    // Now run a transaction that updates one row and deletes the other, then fails.
    let result = transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            let name_col: StrCol<Item> = StrCol::new("name");

            let mut updates = serde_json::Map::new();
            updates.insert("value".into(), serde_json::json!(999));
            Item::objects()
                .filter(name_col.eq("iota"))
                .on_tx(tx)
                .update_values(updates)
                .await?;

            Item::objects()
                .filter(name_col.eq("kappa"))
                .on_tx(tx)
                .delete()
                .await?;

            Err::<(), TxError>(TxError::Deliberate)
        })
    })
    .await;

    assert!(result.is_err());
    // Both the update and the delete must have rolled back.
    assert_eq!(
        count_all(&pool).await,
        2,
        "rows should be unchanged after rollback"
    );

    // Verify the update was rolled back (value is still 1, not 999).
    let iota_val: (i64,) = sqlx::query_as("SELECT value FROM tx_item WHERE name = 'iota'")
        .fetch_one(&pool)
        .await
        .expect("fetch iota");
    assert_eq!(iota_val.0, 1, "update must have rolled back");
}
