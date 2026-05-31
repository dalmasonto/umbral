//! Database pool registry and connection helpers.
//!
//! ## DbPool: the multi-backend seam
//!
//! [`DbPool`] is a small enum that wraps either a [`sqlx::SqlitePool`]
//! or a [`sqlx::PgPool`]. It's the type [`connect`] returns and the
//! type [`AppBuilder::database`](crate::app::AppBuilder::database)
//! stores, so the framework remembers which backend each registered
//! alias is connected to.
//!
//! ### Why an enum, not `sqlx::AnyPool`
//!
//! `sqlx::AnyPool` is the more "correct" abstraction at the type
//! level: one pool type that dispatches to the right driver at
//! runtime. But it has a real-world cost — sea-query-binder (the
//! crate the QuerySet uses to bind parameters) doesn't have an
//! `Any` backend; values must be bound through the per-driver
//! query builder. Forcing every plugin and the queryset onto
//! `AnyPool` therefore turns the simple multi-backend goal into a
//! cascade through every binding site.
//!
//! The enum is the right shape for now. Every plugin still gets a
//! typed `SqlitePool` from [`pool`] / [`pool_for`], and the
//! ergonomics of `sqlx::query(...)` against that pool stay
//! identical. Phase 2 of the Postgres rollout (per `FEATURES.md`)
//! threads the variant choice through the migration engine and
//! queryset; Phase 1 only needs the type seam.
//!
//! ### Postgres at boot, today
//!
//! [`connect`] accepts both `sqlite://...` and `postgres://...`
//! URLs and returns a [`DbPool`] of the matching variant. The
//! detection mirrors [`crate::backend::detect`], so the boot path
//! has one URL parser and they can't drift.
//!
//! At Phase 1 the rest of the framework (queryset, migration
//! engine, every plugin) still reads through [`pool`] / [`pool_for`]
//! which hand back a `SqlitePool`. If the registered pool is
//! actually a `PgPool`, those functions panic with a clear
//! "Postgres support arrives in Phase 2" message. That's
//! deliberate: the type seam exists, but callers that aren't
//! ready for Postgres surface immediately at runtime rather than
//! limping along and producing wrong results.

use std::collections::HashMap;
use std::sync::OnceLock;

use sqlx::{PgPool, SqlitePool};

/// A pool of database connections, typed by backend.
///
/// Cloning is cheap — both variants wrap an `Arc`-backed inner
/// pool, so a `clone()` just bumps the refcount.
#[derive(Debug, Clone)]
pub enum DbPool {
    /// SQLite-backed connection pool. The default through Phase 1
    /// and the only variant the queryset / migration engine accepts
    /// today.
    Sqlite(SqlitePool),
    /// Postgres-backed connection pool. Connectable at Phase 1, but
    /// any code path that calls into the queryset or migration
    /// engine against this variant panics with a clear "arrives in
    /// Phase 2" message. The seam itself is the deliverable here.
    Postgres(PgPool),
}

impl DbPool {
    /// Borrow the inner `SqlitePool`. Returns `None` for a Postgres
    /// pool. Phase 1 callers that haven't migrated to the dispatch
    /// API yet typically reach for [`Self::sqlite_or_panic`]; the
    /// returned-Option variant is for the (rare today) code that
    /// wants to gracefully fall back.
    pub fn as_sqlite(&self) -> Option<&SqlitePool> {
        match self {
            DbPool::Sqlite(p) => Some(p),
            DbPool::Postgres(_) => None,
        }
    }

    /// Borrow the inner `PgPool`. Returns `None` for a SQLite pool.
    pub fn as_postgres(&self) -> Option<&PgPool> {
        match self {
            DbPool::Sqlite(_) => None,
            DbPool::Postgres(p) => Some(p),
        }
    }

    /// Borrow the inner `SqlitePool`, panicking with a clear "Postgres
    /// support arrives in Phase 2" message on a Postgres variant. Used
    /// by [`pool`] and [`pool_for`] so existing plugin code (that
    /// expects a `SqlitePool`) doesn't quietly limp along when the
    /// operator connects to Postgres.
    pub fn sqlite_or_panic(&self) -> &SqlitePool {
        self.as_sqlite().expect(
            "umbra: a Postgres pool is registered but this code path \
             still reads SqlitePool. Full Postgres support lands in \
             Phase 2 of the rollout — see FEATURES.md and the \
             `DbPool` rustdoc.",
        )
    }

    /// The string identifier of the underlying backend. Matches
    /// [`crate::backend::DatabaseBackend::name`] for the active
    /// pool variant.
    pub fn backend_name(&self) -> &'static str {
        match self {
            DbPool::Sqlite(_) => "sqlite",
            DbPool::Postgres(_) => "postgres",
        }
    }
}

impl From<SqlitePool> for DbPool {
    fn from(pool: SqlitePool) -> Self {
        DbPool::Sqlite(pool)
    }
}

impl From<PgPool> for DbPool {
    fn from(pool: PgPool) -> Self {
        DbPool::Postgres(pool)
    }
}

/// Holds all registered database pools, keyed by alias.
/// The "default" pool is always present after `App::build()` succeeds.
static POOLS: OnceLock<HashMap<String, DbPool>> = OnceLock::new();

/// Initialize the pool registry. Called by `AppBuilder::build()` only.
pub(crate) fn init(pools: HashMap<String, DbPool>) {
    POOLS
        .set(pools)
        .expect("umbra::db::init called more than once");
}

/// Return the default connection pool, typed as a [`SqlitePool`].
///
/// This is the function every plugin and the queryset call. The
/// internal storage is a [`DbPool`]; this unwraps to the
/// `SqlitePool` variant or panics with a Phase-2 hint, matching
/// the documented Phase 1 contract.
///
/// # Panics
///
/// Panics if `App::build()` hasn't run or the registered default
/// pool is Postgres.
pub fn pool() -> SqlitePool {
    pool_dispatched().sqlite_or_panic().clone()
}

/// Return the default connection pool as a typed [`DbPool`].
///
/// Use this from code that's ready to dispatch on backend (the
/// migration engine and queryset will move to this surface in
/// Phase 2). Plugin code can stay on [`pool`] until then.
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub fn pool_dispatched() -> &'static DbPool {
    POOLS
        .get()
        .expect("umbra: db pool not initialised — did you call App::build()?")
        .get("default")
        .expect("umbra: no default database registered")
}

/// Return a named connection pool, typed as a [`SqlitePool`].
///
/// # Panics
///
/// Panics if `App::build()` hasn't run, the alias isn't registered,
/// or the registered pool is Postgres.
pub fn pool_for(alias: &str) -> SqlitePool {
    pool_for_dispatched(alias).sqlite_or_panic().clone()
}

/// Return a named connection pool as a typed [`DbPool`]. Phase 2
/// surface; see [`pool_dispatched`].
pub fn pool_for_dispatched(alias: &str) -> &'static DbPool {
    POOLS
        .get()
        .expect("umbra: db pool not initialised — did you call App::build()?")
        .get(alias)
        .unwrap_or_else(|| panic!("umbra: no database registered under alias '{alias}'"))
}

/// Open a new connection pool for the given database URL.
///
/// Dispatches on the URL scheme:
///
/// - `sqlite://...` or `sqlite::memory:` returns a
///   [`DbPool::Sqlite`].
/// - `postgres://...` / `postgresql://...` returns a
///   [`DbPool::Postgres`].
///
/// Any other scheme surfaces as an `sqlx::Error::Configuration`.
/// For callers that already have a typed pool, [`From`] impls on
/// [`DbPool`] convert directly: `let dp: DbPool = sqlite_pool.into();`.
pub async fn connect(url: &str) -> Result<DbPool, sqlx::Error> {
    let scheme = url
        .split("://")
        .next()
        .and_then(|s| s.split(':').next())
        .unwrap_or(url);
    match scheme {
        "sqlite" => Ok(DbPool::Sqlite(SqlitePool::connect(url).await?)),
        "postgres" | "postgresql" => Ok(DbPool::Postgres(PgPool::connect(url).await?)),
        other => Err(sqlx::Error::Configuration(
            format!(
                "umbra::db::connect: unsupported URL scheme `{other}://`. \
                 Phase 1 supports `sqlite://` and `postgres://`."
            )
            .into(),
        )),
    }
}

/// Open a SQLite-backed pool from a URL. Convenience shortcut for
/// callers that want a typed [`SqlitePool`] without the
/// [`DbPool`] enum (mainly tests and any code that needs to call
/// sqlite-specific APIs immediately after connecting).
pub async fn connect_sqlite(url: &str) -> Result<SqlitePool, sqlx::Error> {
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

    /// `connect` hands back a SQLite pool wrapped in `DbPool::Sqlite` we
    /// can actually run queries through.
    #[tokio::test]
    async fn connect_returns_a_working_pool_against_in_memory_sqlite() {
        let pool = connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should always connect");

        let sqlite = pool.as_sqlite().expect("should be Sqlite variant");
        let (one,): (i64,) = sqlx::query_as("SELECT 1")
            .fetch_one(sqlite)
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

    /// MySQL and similar schemes that umbra hasn't shipped yet
    /// surface as a clear configuration error rather than a
    /// driver-internal one.
    #[tokio::test]
    async fn connect_rejects_unsupported_scheme() {
        let result = connect("mysql://user:pass@host/db").await;
        match result {
            Err(sqlx::Error::Configuration(msg)) => {
                assert!(msg.to_string().contains("mysql"));
            }
            other => panic!("expected Configuration error, got {other:?}"),
        }
    }

    /// `From<SqlitePool>` and the variant accessors round-trip.
    #[tokio::test]
    async fn sqlite_pool_round_trips_through_dbpool() {
        let sp = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let dp: DbPool = sp.clone().into();
        assert_eq!(dp.backend_name(), "sqlite");
        assert!(dp.as_sqlite().is_some());
        assert!(dp.as_postgres().is_none());
    }
}
