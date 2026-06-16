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
use std::pin::Pin;
use std::sync::OnceLock;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{ConnectOptions, PgPool, SqlitePool};
use std::str::FromStr;
use std::time::Duration;

pub mod route_context;
pub mod router;

pub use route_context::{current as route_context, RouteContext, TenantKey};
pub use router::{router, Alias, DatabaseRouter, DefaultRouter, RouteOp, Schema};

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

/// Global default for whether ORM write terminals should wrap in a
/// transaction. Set by `AppBuilder::atomic_transactions(...)`; read by
/// every terminal that supports `.atomic()` / `.non_atomic()`. Unset
/// (the default) means "no wrapping" — preserves existing behaviour for
/// apps that don't opt in.
static ATOMIC_DEFAULT: OnceLock<bool> = OnceLock::new();

/// Publish the app-wide atomic-transactions default. Called by
/// `AppBuilder::build()` exactly when the user set the flag via
/// `atomic_transactions(...)`. Idempotent across re-init attempts —
/// the first set wins, matching the rest of the OnceLock-backed
/// ambient state.
pub(crate) fn init_atomic_default(enabled: bool) {
    let _ = ATOMIC_DEFAULT.set(enabled);
}

/// Read the app-wide atomic-transactions default. Returns `false` when
/// the builder didn't call `atomic_transactions(...)` (or when the
/// ambient state hasn't been published yet, as in unit tests that
/// drive the ORM with `.on(&pool)` and never call `App::build()`).
pub fn atomic_default() -> bool {
    *ATOMIC_DEFAULT.get().unwrap_or(&false)
}

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

/// Like [`pool_dispatched`] but returns `None` instead of panicking
/// when no pool is registered yet (`App::build()` hasn't run, or this
/// is a pure SQL-building call such as `QuerySet::to_sql` in a test with
/// no app booted). Used by runtime advisory paths that must not crash a
/// query-builder call — see the RIGHT-JOIN-on-old-SQLite warning.
pub fn try_pool_dispatched() -> Option<&'static DbPool> {
    POOLS.get().and_then(|pools| pools.get("default"))
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

/// List every registered pool alias, sorted alphabetically.
///
/// Used by the migration engine to walk each DB in deterministic
/// order so per-DB tracking tables get created and per-DB diffs run
/// against the right model subset. The `"default"` alias is always
/// present after `App::build()` succeeds and lands wherever
/// alphabetical sort puts it (typically first).
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub fn registered_aliases() -> Vec<String> {
    let mut aliases: Vec<String> = POOLS
        .get()
        .expect("umbra: db pool not initialised — did you call App::build()?")
        .keys()
        .cloned()
        .collect();
    aliases.sort();
    aliases
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
        "sqlite" => Ok(DbPool::Sqlite(connect_sqlite(url).await?)),
        "postgres" | "postgresql" => Ok(DbPool::Postgres(connect_postgres(url).await?)),
        other => Err(sqlx::Error::Configuration(
            format!(
                "umbra::db::connect: unsupported URL scheme `{other}://`. \
                 Phase 1 supports `sqlite://` and `postgres://`."
            )
            .into(),
        )),
    }
}

/// Open a Postgres pool from a URL with umbra's pool configuration.
///
/// PERF-5: bare `PgPool::connect` uses sqlx's defaults with **no acquire
/// timeout**, so a saturated pool blocks request tasks forever. We always
/// set a bounded `acquire_timeout` (fail fast) and a configurable
/// `max_connections`, read from [`crate::settings`] when available
/// (falling back to the documented defaults if the pool is opened before
/// settings are installed).
pub async fn connect_postgres(url: &str) -> Result<PgPool, sqlx::Error> {
    use std::time::Duration;
    let (max_conn, acquire_secs) = crate::settings::get_opt()
        .map(|s| (s.db_max_connections, s.db_acquire_timeout_secs))
        .unwrap_or((10, 30));
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_conn.max(1))
        .acquire_timeout(Duration::from_secs(acquire_secs))
        .connect(url)
        .await
}

/// Open a SQLite-backed pool from a URL.
///
/// Applies the standard production PRAGMAs to every connection in the
/// pool: WAL journal, NORMAL synchronous, a 5-second busy-timeout, and
/// foreign-key enforcement on. Without these, a fresh `SqlitePool` ends
/// up in `journal_mode = DELETE` + `synchronous = FULL` — the safe
/// SQLite defaults that cost ~1-4 seconds per concurrent INSERT once
/// any other connection touches the file (the rollback-journal lock
/// serialises writers).
///
/// | PRAGMA | Value | Why |
/// |---|---|---|
/// | `journal_mode` | `WAL` | Readers don't block writers; a single writer at a time but no full-file lock. Order-of-magnitude faster for any concurrent workload — typically the session/auth/audit tables fanning out. |
/// | `synchronous` | `NORMAL` | Skips the per-commit fsync of the rollback journal; safe with WAL since the WAL log is fsynced on checkpoint. The official SQLite docs call this the right pairing with WAL for "most applications". |
/// | `busy_timeout` | `5000ms` | Wait up to 5 s for a contended writer to release the lock before raising `SQLITE_BUSY`. Without this, two concurrent writers immediately race to error. |
/// | `foreign_keys` | `ON` | sqlite turns FK enforcement off by default. The ORM emits `REFERENCES` clauses assuming they're respected — turning it on per connection makes the FK contract real. |
///
/// **In-memory URLs are backed by a process-unique temp file.** A bare
/// `sqlite::memory:` gives every connection in the pool its OWN private,
/// empty database, so a table created on one connection is invisible to a
/// query that lands on another — and a shared in-memory database doesn't
/// survive the connection (or the tokio runtime) that created it being
/// dropped. Both surface as a flaky "no such table" whenever a pool is
/// reused across queries or test cases. Routing in-memory URLs through a
/// small temp file (which every connection sees and which persists for the
/// process) sidesteps both — the same approach `umbra-testing::TempPool`
/// already documents. File-backed (`sqlite://app.db`) and Postgres URLs are
/// untouched.
pub async fn connect_sqlite(url: &str) -> Result<SqlitePool, sqlx::Error> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static MEM_SEQ: AtomicU64 = AtomicU64::new(0);

    let lower = url.to_ascii_lowercase();
    let in_memory = lower.contains(":memory:") || lower.contains("mode=memory");

    let opts = if in_memory {
        let n = MEM_SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("umbra_mem_{}_{n}.sqlite", std::process::id()));
        // Best-effort: remove a stale file from a previous run with this
        // exact (pid, seq) — pids recycle. WAL/SHM siblings are recreated.
        let _ = std::fs::remove_file(&path);
        SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
    } else {
        SqliteConnectOptions::from_str(url)?
    };
    let opts = opts
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true)
        // Disable per-statement logging — sqlx's default INFO-level
        // logger reads every statement before execution, which adds a
        // measurable per-query overhead under load. The `slow statement`
        // WARN at the 1-second threshold stays on, since it goes via a
        // separate log target.
        .log_statements(tracing::log::LevelFilter::Off);
    SqlitePoolOptions::new().connect_with(opts).await
}

// =============================================================================
// Transaction support
// =============================================================================

/// An active database transaction, typed by backend.
///
/// `Transaction` wraps either a `sqlx::Transaction<'static, sqlx::Sqlite>` or
/// a `sqlx::Transaction<'static, sqlx::Postgres>` and provides the executor
/// surface needed by the ORM's query terminals.
///
/// ## How to obtain one
///
/// The typical path is through the top-level closure helpers:
///
/// ```rust,ignore
/// use umbra::db::transaction;
///
/// let order = transaction(|tx| async move {
///     let o = Order::objects().on_tx(tx).create(new_order).await?;
///     Inventory::objects().on_tx(tx).filter(...).update_values(...).await?;
///     Ok::<_, MyError>(o)
/// }).await?;
/// ```
///
/// For manual control (committing or rolling back yourself) call
/// [`begin`] / [`begin_sqlite`] / [`begin_pg`] directly.
///
/// ## Executor contract
///
/// The `as_sqlite_mut` / `as_pg_mut` accessors return a mutable reference to
/// the underlying sqlx transaction so ORM internals can call
/// `sqlx::query(...).execute(&mut *inner)`. Both the `QuerySet::on_tx` and
/// `Manager::create_in_tx` methods receive `&mut Transaction` and dispatch
/// through these accessors.
pub struct Transaction {
    inner: TransactionInner,
}

enum TransactionInner {
    Sqlite(sqlx::Transaction<'static, sqlx::Sqlite>),
    Postgres(sqlx::Transaction<'static, sqlx::Postgres>),
}

impl Transaction {
    /// Return a mutable reference to the inner SQLite transaction, or `None`
    /// when this is a Postgres transaction.
    pub fn as_sqlite_mut(&mut self) -> Option<&mut sqlx::Transaction<'static, sqlx::Sqlite>> {
        match &mut self.inner {
            TransactionInner::Sqlite(tx) => Some(tx),
            TransactionInner::Postgres(_) => None,
        }
    }

    /// Return a mutable reference to the inner Postgres transaction, or `None`
    /// when this is a SQLite transaction.
    pub fn as_pg_mut(&mut self) -> Option<&mut sqlx::Transaction<'static, sqlx::Postgres>> {
        match &mut self.inner {
            TransactionInner::Sqlite(_) => None,
            TransactionInner::Postgres(tx) => Some(tx),
        }
    }

    /// The backend name — `"sqlite"` or `"postgres"`. Mirrors
    /// [`DbPool::backend_name`] so shared dispatch helpers can use the same
    /// match arm.
    pub fn backend_name(&self) -> &'static str {
        match &self.inner {
            TransactionInner::Sqlite(_) => "sqlite",
            TransactionInner::Postgres(_) => "postgres",
        }
    }

    /// Commit the transaction explicitly.
    ///
    /// The closure-based helpers ([`transaction`] / [`transaction_sqlite`] /
    /// [`transaction_pg`]) call this automatically on `Ok`. Use this only
    /// when you obtained the transaction via [`begin`] / [`begin_sqlite`] /
    /// [`begin_pg`] and are driving the lifecycle yourself.
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        match self.inner {
            TransactionInner::Sqlite(tx) => tx.commit().await,
            TransactionInner::Postgres(tx) => tx.commit().await,
        }
    }

    /// Roll back the transaction explicitly.
    ///
    /// The closure-based helpers call this automatically on `Err`. Use this
    /// only in the manual-control pattern.
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        match self.inner {
            TransactionInner::Sqlite(tx) => tx.rollback().await,
            TransactionInner::Postgres(tx) => tx.rollback().await,
        }
    }
}

/// Begin a transaction against the ambient pool.
///
/// The `Transaction` is dropped-and-rolled-back if neither `commit` nor
/// `rollback` is called before it goes out of scope (sqlx's drop impl).
/// Most callers use the higher-level [`transaction`] / [`transaction_sqlite`]
/// / [`transaction_pg`] closures instead.
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub async fn begin() -> Result<Transaction, sqlx::Error> {
    match pool_dispatched() {
        DbPool::Sqlite(pool) => {
            let tx = pool.begin().await?;
            Ok(Transaction {
                inner: TransactionInner::Sqlite(tx),
            })
        }
        DbPool::Postgres(pool) => {
            let tx = pool.begin().await?;
            Ok(Transaction {
                inner: TransactionInner::Postgres(tx),
            })
        }
    }
}

/// Begin a transaction against an explicit SQLite pool.
pub async fn begin_sqlite(pool: &sqlx::SqlitePool) -> Result<Transaction, sqlx::Error> {
    let tx = pool.begin().await?;
    Ok(Transaction {
        inner: TransactionInner::Sqlite(tx),
    })
}

/// Begin a transaction against an explicit Postgres pool.
pub async fn begin_pg(pool: &sqlx::PgPool) -> Result<Transaction, sqlx::Error> {
    let tx = pool.begin().await?;
    Ok(Transaction {
        inner: TransactionInner::Postgres(tx),
    })
}

/// Pinned, boxed `Future` with a lifetime parameter.
///
/// This is the required shape for the closure argument to
/// [`transaction`] / [`transaction_sqlite`] / [`transaction_pg`].
/// The lifetime `'a` ties the future to the `&'a mut Transaction`
/// reference so the borrow checker can verify that the transaction
/// outlives the async work being done inside it.
///
/// Call sites construct this by calling `.boxed()` or wrapping the
/// `async move` block:
///
/// ```rust,ignore
/// use futures::FutureExt;
/// use umbra::db::{transaction, TxFuture};
///
/// transaction(|tx| {
///     Box::pin(async move {
///         Post::objects().on_tx(tx).create(new_post).await?;
///         Ok::<_, MyError>(())
///     })
/// }).await?;
/// ```
///
/// The `async move { ... }` block captures the `&mut Transaction` by
/// move and the `Box::pin(...)` wrapper satisfies the HRTB bound.
pub type TxFuture<'a, T, E> = Pin<Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'a>>;

/// Run an async closure inside a database transaction against the ambient pool.
///
/// The closure receives `&mut Transaction`. On `Ok` the transaction is
/// committed; on `Err` it is rolled back. Returns the closure's `Ok` value
/// on success.
///
/// The closure must return a `TxFuture` (a `Pin<Box<dyn Future>>`).
/// Use `Box::pin(async move { ... })`:
///
/// ```rust,ignore
/// use umbra::db::transaction;
///
/// let order = transaction(|tx| Box::pin(async move {
///     let o = Order::objects().on_tx(tx).create(new_order).await?;
///     Inventory::objects()
///         .on_tx(tx)
///         .filter(inv::PRODUCT_ID.eq(sku))
///         .update_values(delta)
///         .await?;
///     Ok::<_, MyError>(o)
/// })).await?;
/// ```
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub async fn transaction<F, T, E>(f: F) -> Result<T, E>
where
    for<'a> F: FnOnce(&'a mut Transaction) -> TxFuture<'a, T, E>,
    E: From<sqlx::Error>,
{
    let mut tx = begin().await.map_err(E::from)?;
    match f(&mut tx).await {
        Ok(val) => {
            tx.commit().await.map_err(E::from)?;
            Ok(val)
        }
        Err(e) => {
            // Best-effort rollback — if it fails we surface the original error.
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// Run an async closure inside a SQLite transaction against an explicit pool.
///
/// The SQLite-specific variant of [`transaction`] for callers that want to
/// pin to SQLite regardless of what the ambient pool is, or that are running
/// outside of `App::build()` (e.g. tests).
///
/// See [`transaction`] for the closure shape.
pub async fn transaction_sqlite<F, T, E>(pool: &sqlx::SqlitePool, f: F) -> Result<T, E>
where
    for<'a> F: FnOnce(&'a mut Transaction) -> TxFuture<'a, T, E>,
    E: From<sqlx::Error>,
{
    let mut tx = begin_sqlite(pool).await.map_err(E::from)?;
    match f(&mut tx).await {
        Ok(val) => {
            tx.commit().await.map_err(E::from)?;
            Ok(val)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// Run an async closure inside a Postgres transaction against an explicit pool.
///
/// The Postgres-specific variant of [`transaction`] for callers that want to
/// pin to Postgres or run outside `App::build()`.
///
/// See [`transaction`] for the closure shape.
pub async fn transaction_pg<F, T, E>(pool: &sqlx::PgPool, f: F) -> Result<T, E>
where
    for<'a> F: FnOnce(&'a mut Transaction) -> TxFuture<'a, T, E>,
    E: From<sqlx::Error>,
{
    let mut tx = begin_pg(pool).await.map_err(E::from)?;
    match f(&mut tx).await {
        Ok(val) => {
            tx.commit().await.map_err(E::from)?;
            Ok(val)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
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
