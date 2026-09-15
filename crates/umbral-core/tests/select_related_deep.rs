//! Phase 1 / Plan C, Task 1 (heavy-relations epic) — the cache-aware to-one
//! accessor.
//!
//! `post.author().await?` must serve a `select_related`-hydrated relation
//! from the FK field's cache with ZERO further queries. Before this task the
//! derive-generated accessor was built from the PK only (`to_one_hop`), so it
//! always re-queried even when `select_related` had already fetched the row
//! in the same statement — the core latency gap this task closes.
//!
//! See `docs/specs/orm-heavy-relations-epic.md` ("Correction verified against
//! the implementation") and
//! `docs/superpowers/plans/2026-09-14-orm-heavy-relations-C-deep-hydration.md`
//! (Task 1) for the design.
//!
//! Query counting: this file is its own test binary/process (Rust compiles
//! each `tests/*.rs` file separately), so it carries its own copy of the
//! sqlx-tracing query counter — the same mechanism `tests/query_counts.rs`
//! and `tests/prefetch_by_name.rs` each already duplicate for the same
//! reason (no shared crate to import it from across binaries).

#![allow(dead_code)]

use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, MutexGuard, OnceCell};
use tracing::Subscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use umbral::orm::ForeignKey;

// =========================================================================
// Query counter — see the module doc: duplicated per test-binary by design.
// =========================================================================

static QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);
static INIT: Once = Once::new();
static COUNT_LOCK: Mutex<()> = Mutex::const_new(());

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
        // Connection-setup PRAGMAs fire lazily on first use of a fresh
        // connection — not application DML/DQL, so excluded from the count.
        if stmt.trim_start().to_ascii_uppercase().starts_with("PRAGMA") {
            return;
        }
        QUERY_COUNT.fetch_add(1, Ordering::SeqCst);
    }
}

fn install() {
    INIT.call_once(|| {
        tracing_subscriber::registry()
            .with(LevelFilter::TRACE)
            .with(CountLayer)
            .init();
    });
}

/// Acquire the counting lock — hold it across setup + measurement so no
/// other counting test's queries leak into this count.
async fn query_lock() -> MutexGuard<'static, ()> {
    install();
    COUNT_LOCK.lock().await
}

/// Zero the counter. Call immediately before the operation being measured.
fn reset() {
    QUERY_COUNT.store(0, Ordering::SeqCst);
}

/// Statements counted since the last [`reset`].
fn count() -> usize {
    QUERY_COUNT.load(Ordering::SeqCst)
}

// =========================================================================
// Models — one required forward FK, matching the brief's fixture shape.
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "srd_user")]
pub struct User {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "srd_post")]
pub struct Post {
    #[umbral(primary_key)]
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<User>,
}

// =========================================================================
// Harness — an ambient-pooled App, matching `relation_codegen.rs`'s pattern:
// the derive-generated accessor (`post.author()`) resolves the ambient pool,
// not an explicit `.on(&pool)`.
// =========================================================================

static BOOT: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn boot() -> sqlx::SqlitePool {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        // Build the pool DIRECTLY (not via `umbral::db::connect_sqlite`, which
        // sets `log_statements(Off)` for runtime performance and so suppresses
        // the very `sqlx::query` tracing events this harness counts — see
        // `tests/query_counts.rs`'s `boot_and_seed` for the same reasoning).
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("sqlite opts")
            .shared_cache(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .connect_with(opts)
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<User>()
            .model::<Post>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        sqlx::query("INSERT INTO srd_user (id, name) VALUES (1, 'Ada')")
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query("INSERT INTO srd_post (id, title, author) VALUES (1, 'Hello', 1)")
            .execute(&pool)
            .await
            .expect("seed post");

        pool
    })
    .await
    .clone()
}

// =========================================================================
// Tests
// =========================================================================

/// The core proof: once `select_related` has hydrated the FK's cache, the
/// awaited accessor serves it with ZERO further queries.
#[tokio::test]
async fn accessor_serves_select_related_cache_with_zero_queries() {
    let _g = query_lock().await;
    boot().await;

    let post = Post::objects()
        .filter(post::ID.eq(1))
        .select_related("author")
        .get()
        .await
        .expect("get with select_related");

    reset();
    let author = post
        .author()
        .await
        .expect("cached accessor resolves without a query");
    assert_eq!(author.name, "Ada");
    assert_eq!(
        count(),
        0,
        "post.author().await must serve the select_related cache with zero queries"
    );
}

/// Fallback path stays intact: with NO `select_related`, the same accessor
/// still resolves correctly — via a query.
#[tokio::test]
async fn accessor_without_select_related_still_resolves_via_query() {
    let _g = query_lock().await;
    boot().await;

    let post = Post::objects()
        .filter(post::ID.eq(1))
        .get()
        .await
        .expect("get without select_related");

    reset();
    let author = post
        .author()
        .await
        .expect("un-hydrated accessor still resolves");
    assert_eq!(author.name, "Ada");
    assert!(
        count() >= 1,
        "un-hydrated accessor must fall back to a real query"
    );
}
