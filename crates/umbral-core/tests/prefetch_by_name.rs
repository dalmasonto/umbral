//! Gap #75 — `QuerySet::prefetch_map::<C>()`: batch-load a reverse-FK
//! relation with **no** declared `ReverseSet<C>` field on the parent.
//!
//! `Developer` below carries no `ReverseSet<Achievement>` field at
//! all — the FK lives only on `Achievement` (`pub developer:
//! ForeignKey<Developer>`), exactly the metadata `.reverse::<C>()`
//! (gap #30) already discovers per-instance. `prefetch_map::<C>()` is
//! the batched counterpart: one query for the parents, one `IN (...)`
//! query for the children, and the result comes back explicitly as a
//! [`umbral::orm::Prefetched`] value instead of being written onto a
//! struct field (there is nowhere on `Developer` to write it).
//!
//! A dedicated (per-process) tracing counter proves the child load is
//! ONE statement regardless of parent/child row counts — the same
//! no-N+1 contract the declared-`ReverseSet` path holds.

#![allow(dead_code)]

use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use umbral::orm::ForeignKey;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pbn_developer")]
pub struct Developer {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    // Deliberately NO `ReverseSet<Achievement>` field — that's the gap.
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pbn_achievement")]
pub struct Achievement {
    #[umbral(primary_key)]
    pub id: i64,
    pub title: String,
    pub developer: ForeignKey<Developer>,
}

// =========================================================================
// A second child model with an AMBIGUOUS pair of FKs back to Developer,
// to exercise `PrefetchMapQuery::via`.
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pbn_pairing")]
pub struct Pairing {
    #[umbral(primary_key)]
    pub id: i64,
    pub label: String,
    pub mentor: ForeignKey<Developer>,
    pub mentee: ForeignKey<Developer>,
}

// A model with NO FK to Developer at all — for the "bad relation" error case.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pbn_unrelated")]
pub struct Unrelated {
    #[umbral(primary_key)]
    pub id: i64,
    pub note: String,
}

// ---------------------------------------------------------------------------
// A tiny, self-contained tracing query counter (mirrors query_counts.rs'
// approach but scoped to this binary — integration tests are separate
// processes, so there is no shared state to reuse across files).
// ---------------------------------------------------------------------------

static QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);
static INIT: Once = Once::new();
/// Serializes the measured sections so parallel `#[tokio::test]`s in
/// this binary never interleave their query events into the one
/// global counter — mirrors `query_counts.rs`'s `COUNT_LOCK`.
static COUNT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

impl<S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>> Layer<S>
    for CountLayer
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if !event.metadata().target().starts_with("sqlx::query") {
            return;
        }
        let mut v = StmtVisitor(None);
        event.record(&mut v);
        let stmt = v.0.unwrap_or_default();
        // Connection-setup PRAGMAs fire lazily on first use of a fresh
        // pool connection — not application DML/DQL. Same exclusion
        // query_counts.rs applies, needed for the same reason here.
        if stmt.trim_start().to_ascii_uppercase().starts_with("PRAGMA") {
            return;
        }
        QUERY_COUNT.fetch_add(1, Ordering::SeqCst);
    }
}

fn install_counter() {
    INIT.call_once(|| {
        tracing_subscriber::registry()
            .with(LevelFilter::TRACE)
            .with(CountLayer)
            .init();
    });
}

/// Acquire the counting lock (and install the subscriber on first
/// call). Hold the guard across boot + reset + the measured operation
/// so no other test's queries land in this count.
async fn query_lock() -> tokio::sync::MutexGuard<'static, ()> {
    install_counter();
    COUNT_LOCK.lock().await
}

fn reset_count() {
    QUERY_COUNT.store(0, Ordering::SeqCst);
}

fn query_count() -> usize {
    QUERY_COUNT.load(Ordering::SeqCst)
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        // Build the pool DIRECTLY rather than via `umbral::db::connect_sqlite`:
        // production `connect_sqlite` sets `log_statements(Off)`, which
        // suppresses the `sqlx::query` tracing events this test's counter
        // relies on to prove the one-query batch. A REAL temp-file-backed
        // SQLite db (not a shared-cache `:memory:`) is used — same trick
        // `connect_sqlite` itself plays for "in-memory" URLs — so every
        // pool connection naturally sees the same schema/rows without the
        // shared-cache-drops-when-idle flakiness a `:memory:` + shared_cache
        // pool is prone to under parallel test threads.
        let path = std::env::temp_dir().join(format!(
            "umbral_prefetch_by_name_{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(opts)
            .await
            .expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Developer>()
            .model::<Achievement>()
            .model::<Pairing>()
            .model::<Unrelated>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // ada (1): 3 achievements. grace (2): 2 achievements. ken (3): 0.
        for name in &["ada", "grace", "ken"] {
            sqlx::query("INSERT INTO pbn_developer (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed developer");
        }
        for (title, dev) in &[
            ("shipped M1", 1_i64),
            ("shipped M2", 1),
            ("shipped M3", 1),
            ("fixed the flaky test", 2),
            ("wrote the migration engine", 2),
        ] {
            sqlx::query("INSERT INTO pbn_achievement (title, developer) VALUES (?, ?)")
                .bind(*title)
                .bind(*dev)
                .execute(&pool)
                .await
                .expect("seed achievement");
        }
        sqlx::query("INSERT INTO pbn_pairing (label, mentor, mentee) VALUES ('p1', 1, 2)")
            .execute(&pool)
            .await
            .expect("seed pairing");
    })
    .await;
}

// =========================================================================
// Tests
// =========================================================================

/// The headline behavior: `Developer::objects().prefetch_map::<Achievement>()`
/// batches every developer's achievements in ONE child query, with NO
/// `ReverseSet<Achievement>` field declared on `Developer` anywhere.
#[tokio::test]
async fn prefetch_map_batches_reverse_fk_with_no_declared_field() {
    let _g = query_lock().await;
    boot().await;
    reset_count();

    let prefetched = Developer::objects()
        .prefetch_map::<Achievement>()
        .fetch()
        .await
        .expect("prefetch_map fetch");

    // 1 query for developers + 1 batched `IN (...)` for achievements = 2,
    // regardless of how many developers or achievements exist.
    assert_eq!(
        query_count(),
        2,
        "one parent query + one batched child query, no N+1"
    );

    let by_name: std::collections::HashMap<&str, &Developer> = prefetched
        .parents
        .iter()
        .map(|d| (d.name.as_str(), d))
        .collect();

    let ada = *by_name.get("ada").expect("ada present");
    let ada_achievements = prefetched.children_of(ada);
    assert_eq!(ada_achievements.len(), 3, "ada has 3 achievements");
    let mut titles: Vec<&str> = ada_achievements.iter().map(|a| a.title.as_str()).collect();
    titles.sort();
    assert_eq!(
        titles,
        vec!["shipped M1", "shipped M2", "shipped M3"],
        "ada's exact achievements round-trip"
    );

    let grace = *by_name.get("grace").expect("grace present");
    let grace_achievements = prefetched.children_of(grace);
    assert_eq!(grace_achievements.len(), 2, "grace has 2 achievements");

    let ken = *by_name.get("ken").expect("ken present");
    let ken_achievements = prefetched.children_of(ken);
    assert!(
        ken_achievements.is_empty(),
        "ken has no achievements — children_of reads an empty slice, not an error"
    );
}

/// Composes with the normal QuerySet chain: filtering the parent side
/// before `.prefetch_map` still only takes ONE child query.
#[tokio::test]
async fn prefetch_map_composes_with_filter_and_stays_one_child_query() {
    let _g = query_lock().await;
    boot().await;
    reset_count();

    let prefetched = Developer::objects()
        .filter(developer::NAME.ne("ken"))
        .order_by(developer::NAME.asc())
        .prefetch_map::<Achievement>()
        .fetch()
        .await
        .expect("prefetch_map fetch with filter");

    assert_eq!(query_count(), 2, "filtered parent query + one child batch");
    assert_eq!(prefetched.parents.len(), 2, "ken excluded by the filter");
    assert_eq!(prefetched.parents[0].name, "ada");
    assert_eq!(prefetched.parents[1].name, "grace");
}

/// `.via(...)` disambiguates when the child has more than one FK back
/// to the parent (mirrors `reverse_via`'s escape hatch).
#[tokio::test]
async fn prefetch_map_via_disambiguates_multiple_fks() {
    let _g = query_lock().await;
    boot().await;

    // Without `.via`, Pairing has two FKs to Developer (mentor, mentee) —
    // a clear, actionable error naming both candidates.
    let ambiguous_err = Developer::objects()
        .prefetch_map::<Pairing>()
        .fetch()
        .await
        .expect_err("ambiguous FK must error");
    let msg = ambiguous_err.to_string();
    assert!(msg.contains("mentor"), "names candidate `mentor`: {msg}");
    assert!(msg.contains("mentee"), "names candidate `mentee`: {msg}");

    let via_mentor = Developer::objects()
        .prefetch_map::<Pairing>()
        .via("mentor")
        .fetch()
        .await
        .expect("via(\"mentor\") resolves the ambiguity");
    let ada = via_mentor
        .parents
        .iter()
        .find(|d| d.name == "ada")
        .expect("ada present");
    let mentor_pairings = via_mentor.children_of(ada);
    assert_eq!(mentor_pairings.len(), 1, "ada mentors one pairing");
    assert_eq!(mentor_pairings[0].label, "p1");

    let via_mentee = Developer::objects()
        .prefetch_map::<Pairing>()
        .via("mentee")
        .fetch()
        .await
        .expect("via(\"mentee\") resolves the ambiguity");
    let grace = via_mentee
        .parents
        .iter()
        .find(|d| d.name == "grace")
        .expect("grace present");
    let mentee_pairings = via_mentee.children_of(grace);
    assert_eq!(mentee_pairings.len(), 1, "grace is mentee of one pairing");
}

/// A bad relation — the child has NO foreign key to the parent at all —
/// errors clearly instead of silently returning empty results.
#[tokio::test]
async fn prefetch_map_errors_clearly_on_unrelated_child() {
    let _g = query_lock().await;
    boot().await;
    let err = Developer::objects()
        .prefetch_map::<Unrelated>()
        .fetch()
        .await
        .expect_err("no FK from Unrelated to Developer must error");
    let msg = err.to_string();
    assert!(
        msg.contains("Unrelated") && msg.contains("pbn_developer"),
        "error names the child and the parent table it can't reach: {msg}"
    );
}

/// `.via(...)` with a column that isn't a FK to the parent also errors
/// clearly rather than silently misbehaving.
#[tokio::test]
async fn prefetch_map_via_bad_column_errors_clearly() {
    let _g = query_lock().await;
    boot().await;
    let err = Developer::objects()
        .prefetch_map::<Achievement>()
        .via("title")
        .fetch()
        .await
        .expect_err("`title` is not a FK to Developer");
    let msg = err.to_string();
    assert!(msg.contains("title"), "error names the bad column: {msg}");
}

/// `prefetch_map` is purely additive: it doesn't replace or disturb a
/// normal `.fetch()` on the same model. (The full regression suite for
/// the pre-existing declared-`ReverseSet` `.prefetch_related("x_set")`
/// path — untouched by this change — lives in `reverse_fk_prefetch.rs`
/// and stays green; a second `App::build` in this process to construct
/// a parallel declared-field fixture isn't possible, since `App::build`
/// is the one-shot ambient singleton per CLAUDE.md's "ambient ORM
/// access" note.)
#[tokio::test]
async fn prefetch_map_is_additive_to_plain_fetch() {
    let _g = query_lock().await;
    boot().await;
    let plain = Developer::objects().fetch().await.expect("plain fetch");
    assert_eq!(
        plain.len(),
        3,
        "prefetch_map adds a path, doesn't replace fetch()"
    );
}
