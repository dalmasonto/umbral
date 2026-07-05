//! The ORM must open SQLite write transactions with `BEGIN IMMEDIATE`.
//!
//! sqlx's `pool.begin()` issues `BEGIN DEFERRED`: no lock is taken until the
//! first write, so a read→write transaction upgrades its lock mid-flight. Under
//! concurrent writes on a file DB that upgrade returns SQLITE_BUSY *immediately*
//! — SQLite's deadlock-avoidance path, which the `busy_timeout` handler is never
//! consulted for. The fix is `BEGIN IMMEDIATE`, which takes the write lock at
//! BEGIN so `busy_timeout` applies and a contending writer WAITS.
//!
//! This test pins the behaviour deterministically: with `busy_timeout = 0`, a
//! concurrent writer on a second connection is locked out iff the ORM's
//! transaction already holds the write lock — which only happens under
//! `BEGIN IMMEDIATE`. Under `BEGIN DEFERRED` the empty transaction holds nothing
//! and the concurrent write slips through.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::time::Duration;

#[tokio::test]
async fn begin_sqlite_takes_the_write_lock_immediately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("begin_immediate.sqlite");
    // busy_timeout = 0 → a blocked writer errors instantly instead of waiting,
    // so the lock state is observable without a race.
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .expect("pool");
    sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .expect("create table");

    // Open a write transaction through the ORM. Under BEGIN IMMEDIATE this holds
    // the RESERVED write lock *now*, before any statement runs.
    let tx = umbral_core::db::begin_sqlite(&pool)
        .await
        .expect("begin transaction");

    // A concurrent writer on a different pooled connection must be locked out
    // (busy_timeout = 0 → instant error), proving the transaction already owns
    // the write lock. Under BEGIN DEFERRED the transaction holds no lock and
    // this INSERT would succeed.
    let mut other = pool.acquire().await.expect("second connection");
    let res = sqlx::query("INSERT INTO t (id) VALUES (1)")
        .execute(&mut *other)
        .await;
    assert!(
        res.is_err(),
        "a concurrent write must be blocked by the IMMEDIATE transaction; got \
         {res:?} — BEGIN DEFERRED (the bug) would let it through"
    );

    tx.rollback().await.expect("rollback");
}
