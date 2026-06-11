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

// ---------------------------------------------------------------------------
// Scale proofs — the no-N+1 contract for every relation-loading path. Each
// proof runs the SAME ORM op against a SMALL (10) and a LARGE (10,000) row
// set and asserts the statement count is EQUAL across the two sizes (the
// data-size-invariance proof — this is the assertion that catches a real
// N+1; it must never be weakened) AND matches the per-feature absolute
// constant (the feature's contract). All share one in-memory pool booted
// once; the BOOT cell tops the seed up from 10 → 10,000 across calls so the
// large-N pass only inserts the delta.
// ---------------------------------------------------------------------------

use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell as TokioOnceCell;
use umbra::orm::{ForeignKey, M2M, ReverseSet};
use umbra::prelude::*;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Author {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Plugin {
    pub id: i64,
    pub name: String,
    pub author: ForeignKey<Author>, // NOT NULL → INNER under auto-inference
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Tag {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub plugin: ForeignKey<Plugin>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(reverse_fk = "comment")]
    pub reaction_set: ReverseSet<Reaction>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(m2m = "tag")]
    pub tags: M2M<Tag>,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Reaction {
    pub id: i64,
    pub kind: String,
    pub comment: ForeignKey<Comment>,
}

static BOOT: TokioOnceCell<()> = TokioOnceCell::const_new();

/// Boot the App once with the proof models against an in-memory SQLite
/// pool, create the tables, and seed `n` Comments each pointing at a
/// Plugin → Author chain, each with one Reaction and two Tags. Idempotent
/// per process; re-seeds up to `n` rows so a later larger-N call tops up.
async fn boot_and_seed(n: i64) {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        // Build the pool DIRECTLY rather than via `umbra::db::connect_sqlite`:
        // production `connect_sqlite` sets `log_statements(Off)` for runtime
        // performance, which suppresses the very `sqlx::query` tracing events
        // this harness counts. A query-count proof needs them ON.
        //
        // A bare `sqlite::memory:` database is per-CONNECTION — every fresh
        // pool connection sees an empty schema, so the tables created during
        // boot vanish the moment sqlx hands out a different connection (the
        // "no such table" race under parallel tests). `shared_cache(true)`
        // makes all connections in this process share ONE in-memory database;
        // `min_connections(1)` keeps it alive so the shared cache is never
        // dropped between queries.
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("sqlite opts")
            .shared_cache(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .connect_with(opts)
            .await
            .expect("in-memory sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Plugin>()
            .model::<Tag>()
            .model::<Comment>()
            .model::<Reaction>()
            .build()
            .expect("App::build");
        for ddl in [
            "CREATE TABLE author (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "CREATE TABLE plugin (id INTEGER PRIMARY KEY, name TEXT NOT NULL, author INTEGER NOT NULL)",
            "CREATE TABLE tag (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
            "CREATE TABLE comment (id INTEGER PRIMARY KEY, body TEXT NOT NULL, plugin INTEGER NOT NULL)",
            "CREATE TABLE reaction (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, comment INTEGER NOT NULL)",
            "CREATE TABLE comment_tags (parent_id INTEGER NOT NULL, child_id INTEGER NOT NULL)",
            "INSERT INTO author (id, name) VALUES (1, 'Ada')",
            "INSERT INTO plugin (id, name, author) VALUES (1, 'orm', 1)",
            "INSERT INTO tag (id, label) VALUES (1, 'perf'), (2, 'safety')",
        ] {
            sqlx::query(ddl).execute(&pool).await.unwrap();
        }
    })
    .await;

    // Set the comment population to EXACTLY `n` (+ one reaction + two tag
    // links each), all in a single transaction so the seed stays fast and
    // never bloats the measured count (the caller resets right after this
    // returns). "Set to exactly n" rather than "top up to n" because the
    // process-global ambient pool means EVERY proof shares this one
    // in-memory DB (only one `App::build` per process is permitted), and
    // tests run in parallel: a prior proof may have left 10,000 rows. To
    // keep each proof a genuine 10 → 10,000 span we trim back down to the
    // size THIS call wants. The lock the caller holds serialises this, so
    // no two proofs ever see each other's resize mid-flight. (Trimming a
    // throwaway in-memory fixture the test itself created is test hygiene,
    // not the "never wipe the DB" rule — that rule guards the user's real
    // data, never an `:memory:` table booted seconds ago.)
    let pool = umbra::db::pool_for("default");
    let have: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comment")
        .fetch_one(&pool)
        .await
        .unwrap();
    if have == n {
        return;
    }
    let mut tx = pool.begin().await.unwrap();
    if have > n {
        // Trim down: drop the comments above `n` and their dependents.
        for (table, col) in [
            ("comment_tags", "parent_id"),
            ("reaction", "comment"),
            ("comment", "id"),
        ] {
            sqlx::query(&format!("DELETE FROM {table} WHERE {col} > ?"))
                .bind(n)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
    } else {
        // Top up to `n`.
        for id in (have + 1)..=n {
            sqlx::query("INSERT INTO comment (id, body, plugin) VALUES (?, ?, 1)")
                .bind(id)
                .bind(format!("c{id}"))
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("INSERT INTO reaction (kind, comment) VALUES ('up', ?)")
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("INSERT INTO comment_tags (parent_id, child_id) VALUES (?, 1), (?, 2)")
                .bind(id)
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn select_related_nested_is_constant_queries_not_n_plus_1() {
    let _g = query_lock().await;
    let mut counts = Vec::new();
    for n in [10_i64, 10_000] {
        boot_and_seed(n).await;
        reset();
        let rows = Comment::objects()
            .select_related("plugin__author")
            .fetch()
            .await
            .expect("fetch");
        assert_eq!(rows.len() as i64, n, "sanity: all parents returned");
        // Deepest level hydrated from the batched chain, not per-row.
        let author = rows[0]
            .plugin
            .resolved()
            .and_then(|p| p.author.resolved())
            .expect("author hydrated");
        assert_eq!(author.name, "Ada");
        counts.push(count());
    }
    // 1 main + 2 hop batches = 3, for BOTH sizes. Equal counts is the
    // no-N+1 proof; the absolute value (3) is the select_related contract.
    assert_eq!(
        counts[0],
        counts[1],
        "query count must not grow with parent count (saw {counts:?}; statements: {:?})",
        statements()
    );
    assert_eq!(
        counts[0],
        3,
        "expected main + 2 hop batches; saw {:?}",
        statements()
    );
}

#[tokio::test]
async fn prefetch_related_is_constant_queries_not_n_plus_1() {
    let _g = query_lock().await;
    let mut counts = Vec::new();
    for n in [10_i64, 10_000] {
        boot_and_seed(n).await;
        reset();
        let rows = Comment::objects()
            .prefetch_related("reaction_set")
            .prefetch_related("tags")
            .fetch()
            .await
            .expect("fetch");
        assert_eq!(rows.len() as i64, n);
        assert_eq!(rows[0].reaction_set.resolved().map(|r| r.len()), Some(1));
        assert_eq!(rows[0].tags.resolved().map(|t| t.len()), Some(2));
        counts.push(count());
    }
    assert_eq!(
        counts[0],
        counts[1],
        "prefetch query count must not grow with parent count (saw {counts:?}; {:?})",
        statements()
    );
    // 1 main + 1 reverse-fk batch + 1 m2m batch = 3.
    assert_eq!(
        counts[0],
        3,
        "expected main + 2 prefetch batches; saw {:?}",
        statements()
    );
}

#[tokio::test]
async fn nested_join_related_is_one_query_not_n() {
    let _g = query_lock().await;
    let mut counts = Vec::new();
    for n in [10_i64, 10_000] {
        boot_and_seed(n).await;
        reset();
        let rows = Comment::objects()
            .inner_join_related("plugin__author")
            .fetch()
            .await
            .expect("fetch");
        assert_eq!(rows.len() as i64, n);
        let author = rows[0]
            .plugin
            .resolved()
            .and_then(|p| p.author.resolved())
            .expect("author hydrated from the single joined query");
        assert_eq!(author.name, "Ada");
        counts.push(count());
    }
    assert_eq!(
        counts[0],
        counts[1],
        "join query count must not grow with parent count (saw {counts:?}; {:?})",
        statements()
    );
    assert_eq!(
        counts[0],
        1,
        "a nested join is ONE statement; saw {:?}",
        statements()
    );
}

#[tokio::test]
async fn annotate_count_is_one_query_not_n() {
    let _g = query_lock().await;
    let mut counts = Vec::new();
    for n in [10_i64, 10_000] {
        boot_and_seed(n).await;
        reset();
        // Correlated subquery rides in the main SELECT — one statement for
        // all N parents. Both the reverse-FK count and the M2M count.
        let rows = Comment::objects()
            .annotate_count("reaction_set")
            .annotate_count("tags")
            .fetch_annotated()
            .await
            .expect("fetch_annotated");
        assert_eq!(rows.len() as i64, n);
        assert_eq!(rows[0].1["reaction_set_count"].as_i64(), Some(1));
        assert_eq!(rows[0].1["tags_count"].as_i64(), Some(2));
        counts.push(count());
    }
    assert_eq!(
        counts[0],
        counts[1],
        "annotate query count must not grow with parent count (saw {counts:?}; {:?})",
        statements()
    );
    assert_eq!(
        counts[0],
        1,
        "annotate is one correlated query; saw {:?}",
        statements()
    );
}

#[tokio::test]
async fn m2m_validation_is_one_query_not_per_id() {
    let _g = query_lock().await;
    boot_and_seed(1).await;
    let pool = umbra::db::pool_for("default");
    // Ensure at least 500 candidate tags exist.
    let have: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tag")
        .fetch_one(&pool)
        .await
        .unwrap();
    for id in (have + 1)..=500 {
        sqlx::query("INSERT INTO tag (id, label) VALUES (?, ?)")
            .bind(id)
            .bind(format!("t{id}"))
            .execute(&pool)
            .await
            .unwrap();
    }

    let mut counts = Vec::new();
    for k in [3_usize, 300] {
        let ids: Vec<String> = (1..=k as i64).map(|i| i.to_string()).collect();
        let mut errs = umbra::forms::ValidationErrors::new();
        reset();
        // The batched validator from Plan A: validates M submitted M2M ids
        // with ONE `WHERE pk IN (...)` query, never M per-id COUNTs.
        let _ok =
            umbra::orm::forms_runtime::validate_multi_fk_exists("tags", &ids, "tag", &mut errs)
                .await;
        assert!(errs.is_empty(), "all ids exist (errs: {:?})", errs.fields);
        counts.push(count());
    }
    assert_eq!(
        counts[0],
        counts[1],
        "M2M validation must be one query regardless of how many ids (saw {counts:?}; {:?})",
        statements()
    );
    assert_eq!(
        counts[0],
        1,
        "validating M ids is ONE IN-query; saw {:?}",
        statements()
    );
}
