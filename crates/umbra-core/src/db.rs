use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Holds all registered database pools, keyed by alias.
/// The "default" pool is always present after `App::build()` succeeds.
static POOLS: OnceLock<HashMap<String, SqlitePool>> = OnceLock::new();

/// Initialize the pool registry. Called by `AppBuilder::build()` only.
pub(crate) fn init(pools: HashMap<String, SqlitePool>) {
    POOLS
        .set(pools)
        .expect("umbra::db::init called more than once");
}

/// Return the default connection pool.
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub fn pool() -> SqlitePool {
    POOLS
        .get()
        .expect("umbra: db pool not initialised — did you call App::build()?")
        .get("default")
        .expect("umbra: no default database registered")
        .clone()
}

/// Return a named connection pool.
///
/// # Panics
///
/// Panics if `App::build()` hasn't run or the alias isn't registered.
pub fn pool_for(alias: &str) -> SqlitePool {
    POOLS
        .get()
        .expect("umbra: db pool not initialised — did you call App::build()?")
        .get(alias)
        .unwrap_or_else(|| panic!("umbra: no database registered under alias '{alias}'"))
        .clone()
}

/// Open a new connection pool for the given database URL.
///
/// M0: SQLite only. M4+ will inspect the URL scheme and return a
/// backend-typed pool when Postgres support lands.
pub async fn connect(url: &str) -> Result<SqlitePool, sqlx::Error> {
    SqlitePool::connect(url).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // `pool` and `pool_for` read the process-wide `POOLS` `OnceLock`, which
    // can only be set once per process. Under cargo test's parallel runner
    // that makes them unreliable to cover directly without `serial_test` or
    // a refactor, so they're intentionally out of scope here. Same reason
    // the "pool() panics before init" path isn't exercised: another test in
    // the same process may have already populated the lock.
    //
    // Mirrors the settings module's stance on its own `init`/`get` pair.

    /// `connect` hands back a pool we can actually run queries through.
    #[tokio::test]
    async fn connect_returns_a_working_pool_against_in_memory_sqlite() {
        let pool = connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should always connect");

        let (one,): (i64,) = sqlx::query_as("SELECT 1")
            .fetch_one(&pool)
            .await
            .expect("SELECT 1 should succeed on a fresh pool");

        assert_eq!(one, 1);
    }

    /// A URL sqlx can't parse surfaces as a plain `sqlx::Error`. We don't
    /// pin the variant — the family is the contract.
    #[tokio::test]
    async fn connect_errors_on_malformed_url() {
        let result = connect("not-a-real-url").await;
        assert!(
            result.is_err(),
            "expected sqlx to reject a malformed url, got Ok"
        );
    }
}
