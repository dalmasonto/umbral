//! Query-count proof harness — prove the ORM's no-N+1 guarantees by
//! COUNTING the SQL statements actually executed, and showing the count
//! stays FLAT as row counts grow. This is the artifact a skeptic can't
//! argue with: not "the code looks like one query" but "exactly one
//! statement ran, whether the table holds 10 rows or 10,000."
//!
//! ## How it stays non-flaky (the "no harm" contract)
//!
//! sqlx 0.8 emits exactly one `tracing` event at target `sqlx::query`
//! per executed statement (confirmed empirically). We count those.
//! Two design choices keep the count isolated:
//!   1. **Dedicated binary.** This file is its own test process, so no
//!      unrelated test increments the shared counter.
//!   2. **Serialized counting + lock-scoped setup.** Every DB-touching
//!      test here first takes [`query_lock`]; it holds that lock across
//!      BOTH its setup and its measured section, so no two tests'
//!      queries ever interleave into one counter. Seed rows under the
//!      lock, then [`reset`] immediately before the operation you are
//!      measuring so the seed doesn't inflate the count.
//!
//! Pattern:
//! ```ignore
//! let _g = query_lock().await;      // serialize against other counters
//! seed_n_rows(&pool, 10_000).await; // counted, but we reset below
//! reset();
//! let rows = Thing::objects().join_related("a__b").fetch().await?;
//! assert_eq!(count(), 1, "one JOIN query regardless of row count");
//! ```

use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{Mutex, MutexGuard};
use tracing::Subscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

static QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);
static INIT: Once = Once::new();
/// Serializes the measured sections so concurrent tests in this binary
/// never interleave their query events into the single global counter.
static COUNT_LOCK: Mutex<()> = Mutex::const_new(());

static STMTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

struct CountLayer;

struct StmtVisitor(Option<String>);
impl tracing::field::Visit for StmtVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "summary" || field.name() == "db.statement" {
            self.0 = Some(format!("{value:?}"));
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "summary" || field.name() == "db.statement" {
            self.0 = Some(value.to_string());
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for CountLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if !event.metadata().target().starts_with("sqlx::query") {
            return;
        }
        let mut v = StmtVisitor(None);
        event.record(&mut v);
        let stmt = v.0.unwrap_or_default();
        // Connection-setup PRAGMAs (`PRAGMA foreign_keys = ON`, journal
        // mode, etc.) are sqlx/connection bootstrap that fires lazily on
        // first use of a fresh connection — NOT the application DML/DQL an
        // N+1 audit counts. Excluding them makes the count deterministic
        // regardless of whether the measured op warmed a new connection.
        if stmt.trim_start().to_ascii_uppercase().starts_with("PRAGMA") {
            return;
        }
        QUERY_COUNT.fetch_add(1, Ordering::SeqCst);
        STMTS.lock().unwrap().push(stmt);
    }
}

fn install() {
    INIT.call_once(|| {
        // LevelFilter::TRACE forces every sqlx event through to the layer
        // regardless of the default max-level hint.
        tracing_subscriber::registry()
            .with(LevelFilter::TRACE)
            .with(CountLayer)
            .init();
    });
}

/// Acquire the counting lock. Hold it across setup + measurement so no
/// other counting test's queries leak into your count. Installs the
/// subscriber on first call.
pub async fn query_lock() -> MutexGuard<'static, ()> {
    install();
    COUNT_LOCK.lock().await
}

/// Zero the counter (and the captured-statement log) — call immediately
/// before the operation you measure so lock-held setup queries don't
/// inflate the count.
pub fn reset() {
    QUERY_COUNT.store(0, Ordering::SeqCst);
    STMTS.lock().unwrap().clear();
}

/// Read how many sqlx statements (excluding connection-setup PRAGMAs)
/// have executed since the last [`reset`].
pub fn count() -> usize {
    QUERY_COUNT.load(Ordering::SeqCst)
}

/// The SQL of every counted statement since the last [`reset`]. Use in a
/// failing assertion message to see exactly what ran when a count is off.
pub fn statements() -> Vec<String> {
    STMTS.lock().unwrap().clone()
}

// ---------------------------------------------------------------------------
// Self-tests — prove the harness itself is accurate and that the most
// basic anti-N+1 property (reading N rows is ONE query) holds today, with
// no framework features required. The relation-specific scale proofs
// (nested join = 1, annotate = 1, select_related = 1+hops, prefetch = 2,
// M2M validation = 1 — each invariant to row count) are added by the
// query-count proof plan once those paths land.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn harness_counts_exactly_the_statements_executed() {
    let _g = query_lock().await;
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)")
        .execute(&pool)
        .await
        .unwrap();

    reset();
    sqlx::query("INSERT INTO t (n) VALUES (1)")
        .execute(&pool)
        .await
        .unwrap();
    let _ = sqlx::query("SELECT * FROM t")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        count(),
        2,
        "two statements ran; harness must report 2 (saw: {:?})",
        statements()
    );
}

#[tokio::test]
async fn reading_many_rows_is_one_query_not_n() {
    // The primitive anti-N+1 proof: a single SELECT is ONE statement
    // whether it returns 10 rows or 10,000. Count is invariant to row
    // count — the property a billion-row table depends on.
    let _g = query_lock().await;
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    sqlx::query("CREATE TABLE big (id INTEGER PRIMARY KEY, n INTEGER)")
        .execute(&pool)
        .await
        .unwrap();

    for &total in &[10_i64, 10_000] {
        // Seed up to `total` rows under the lock (counted, then reset).
        let current: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM big")
            .fetch_one(&pool)
            .await
            .unwrap();
        for n in current..total {
            sqlx::query("INSERT INTO big (n) VALUES (?)")
                .bind(n)
                .execute(&pool)
                .await
                .unwrap();
        }

        reset();
        let rows = sqlx::query("SELECT * FROM big")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows.len() as i64, total, "sanity: fetched all rows");
        assert_eq!(
            count(),
            1,
            "reading {total} rows must be ONE query, not {total}"
        );
    }
}
