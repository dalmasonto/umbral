//! gaps2 #91: connection-pool configuration smoke tests.
//!
//! sqlx doesn't expose the configured pool options for inspection, so
//! these assert *behaviour* — a pool built with the new `SqlitePoolOptions`
//! knobs still creates cleanly and serves queries, and the graceful-close
//! path closes a pool without panicking.

use sqlx::Row;
use umbral_core::db;

/// Building a SQLite pool through `connect_sqlite` (which now applies
/// `max_connections` / `min_connections` / `acquire_timeout` /
/// `idle_timeout` / `max_lifetime` / `test_before_acquire`) succeeds and a
/// trivial query runs — proving the added pool knobs don't break creation.
#[tokio::test]
async fn sqlite_pool_builds_and_serves_queries_with_new_knobs() {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("pool should build with the configured options");

    let row = sqlx::query("SELECT 1 AS one")
        .fetch_one(&pool)
        .await
        .expect("SELECT 1 should succeed on the freshly built pool");
    let one: i64 = row.get("one");
    assert_eq!(one, 1);

    // test_before_acquire (on by default) health-checks the connection,
    // so a second acquire after the first is returned still works.
    let row = sqlx::query("SELECT 2 AS two")
        .fetch_one(&pool)
        .await
        .expect("second query should reuse a health-checked connection");
    let two: i64 = row.get("two");
    assert_eq!(two, 2);

    pool.close().await;
}

/// A built pool can be closed without panicking, and acquiring from it
/// afterwards errors (closing is terminal). This exercises the same
/// `Pool::close().await` that `db::close()` calls per registered pool.
#[tokio::test]
async fn closing_a_pool_does_not_panic_and_is_terminal() {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("pool should build");

    // Closing is the operation `db::close()` performs on the ambient pool.
    pool.close().await;
    assert!(pool.is_closed(), "pool should report closed after close()");

    let result = sqlx::query("SELECT 1").fetch_one(&pool).await;
    assert!(result.is_err(), "querying a closed pool should error");
}

/// The ambient `db::close()` is a safe no-op when no pool has ever been
/// registered (an integration test can't drive `App::build`, which is the
/// only path that sets the ambient `POOLS` `OnceLock`).
#[tokio::test]
async fn ambient_close_is_a_noop_when_unset() {
    // Must not panic.
    db::close().await;
}
