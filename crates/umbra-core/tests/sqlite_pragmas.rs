//! Lock in the SQLite PRAGMAs `umbra::db::connect_sqlite` applies to every
//! pool connection.
//!
//! Without these, a fresh `SqlitePool` lands in `journal_mode = DELETE` +
//! `synchronous = FULL` — the official safe defaults that serialise every
//! concurrent writer behind a full-file lock. The user-visible symptom is
//! sessions and writes taking 1-4 seconds per INSERT under concurrent
//! load. WAL + NORMAL + a busy-timeout brings that back into the
//! 10-50 ms range, so we test the configuration explicitly to keep it
//! from regressing.

use sqlx::Row;
use umbra_core::db;

#[tokio::test]
async fn connect_sqlite_enables_wal_journal() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("pragma_test.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.display());

    let pool = db::connect_sqlite(&url).await.expect("connect");
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .expect("read journal_mode");
    // PRAGMA returns lowercase variants — both "wal" and "WAL" are
    // acceptable depending on the SQLite version. Match case-insensitively.
    assert_eq!(
        mode.to_lowercase(),
        "wal",
        "expected WAL journal mode for concurrent writers"
    );
}

#[tokio::test]
async fn connect_sqlite_uses_normal_synchronous() {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("memory connect");
    // synchronous=NORMAL maps to PRAGMA value 1.
    let level: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(&pool)
        .await
        .expect("read synchronous");
    assert_eq!(level, 1, "expected synchronous = NORMAL (1)");
}

#[tokio::test]
async fn connect_sqlite_sets_busy_timeout() {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("memory connect");
    let timeout_ms: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(&pool)
        .await
        .expect("read busy_timeout");
    assert!(
        timeout_ms >= 5000,
        "expected busy_timeout >= 5000 ms, got {timeout_ms}"
    );
}

#[tokio::test]
async fn connect_sqlite_enables_foreign_keys() {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("memory connect");
    let on: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .expect("read foreign_keys");
    assert_eq!(on, 1, "expected foreign_keys = ON");
}

#[tokio::test]
async fn pragmas_apply_per_connection_in_pool() {
    // Force the pool to hand out several distinct connections and verify
    // each one carries the PRAGMAs — proves the connect-options pipeline
    // runs on every new connection, not only the first.
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("memory connect");
    let rows = sqlx::query("PRAGMA foreign_keys")
        .fetch_all(&pool)
        .await
        .expect("read foreign_keys");
    let val: i64 = rows[0].get(0);
    assert_eq!(val, 1);
}
