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
