//! Heavy-relations epic, Task 2b — cache-aware TO-MANY relation accessors.
//!
//! Symmetric with Task 2 (the to-one accessor's `select_related` cache-hit
//! path, `tests/select_related_deep.rs`). After
//! `.prefetch_related("categories")` / `.prefetch_related("comment_set")`,
//! the generated M2M forward accessor (`blog.categories()`) and the
//! reverse-FK accessor (`post.comment_set()`) must serve the prefetched
//! `Vec` with ZERO further queries via `.fetch()`/`.count()`/`.first()`/
//! `.exists()` — but ANY further `.filter()`/`.order_by()`/etc. on the
//! returned `QuerySet` invalidates the cache slot and re-queries, because
//! the cache is the UNFILTERED set.
//!
//! Query counting: this file is its own test binary/process (Rust compiles
//! each `tests/*.rs` file separately), so it carries its own copy of the
//! sqlx-tracing query counter — same duplication rationale as
//! `tests/query_counts.rs` / `tests/select_related_deep.rs`.

#![allow(dead_code)]

use std::str::FromStr;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, MutexGuard, OnceCell};
use tracing::Subscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use umbral::orm::{ForeignKey, M2M, ReverseSet};

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
///
/// Drains for a few milliseconds first: sqlx-sqlite executes each query on a
/// dedicated per-connection worker thread and the `sqlx::query` tracing event
/// this harness counts is emitted from THAT thread, which can straggle a hair
/// past the moment the awaiting future resolves and control returns here —
/// especially right after a PRECEDING test released [`COUNT_LOCK`] and this
/// test immediately re-acquired it (so the two tests' async work isn't
/// perfectly serialized down to the microsecond even though the LOCK is).
/// Without the drain, a straggling event from a PRIOR real query can land
/// just after `reset()` and register as a phantom "extra" query in a
/// following zero-query assertion — a rare but real flake this file's own
/// history surfaced (a preceding un-prefetched, real-query test followed
/// immediately by a must-be-zero one). Sleeping first, then clearing,
/// guarantees any such straggler is swept up and zeroed rather than
/// mis-attributed to the next measured window.
async fn reset() {
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    QUERY_COUNT.store(0, Ordering::SeqCst);
}

/// Statements counted since the last [`reset`].
fn count() -> usize {
    QUERY_COUNT.load(Ordering::SeqCst)
}

// =========================================================================
// Models — an M2M pair (Blog/Category) and a reverse-FK pair (Post/Comment).
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pac_category")]
pub struct Category {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub active: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pac_blog")]
pub struct Blog {
    #[umbral(primary_key)]
    pub id: i64,
    pub title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "pac_category")]
    pub categories: M2M<Category>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pac_comment")]
pub struct Comment {
    #[umbral(primary_key)]
    pub id: i64,
    pub body: String,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pac_post")]
pub struct Post {
    #[umbral(primary_key)]
    pub id: i64,
    pub title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "post")]
    pub comment_set: ReverseSet<Comment>,
}

// =========================================================================
// Harness
// =========================================================================

static BOOT: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn boot() -> sqlx::SqlitePool {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        // Build the pool DIRECTLY (not via `umbral::db::connect_sqlite`, which
        // suppresses `sqlx::query` tracing for runtime performance) — same
        // reasoning as `tests/select_related_deep.rs`'s harness.
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
            .model::<Category>()
            .model::<Blog>()
            .model::<Post>()
            .model::<Comment>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // Blog "b1" with 3 categories, 2 active + 1 inactive — the inactive
        // one lets the filter-after-prefetch test prove a real re-query.
        for (name, active) in &[("news", true), ("tech", true), ("archived", false)] {
            sqlx::query("INSERT INTO pac_category (name, active) VALUES (?, ?)")
                .bind(*name)
                .bind(*active)
                .execute(&pool)
                .await
                .expect("seed category");
        }
        sqlx::query("INSERT INTO pac_blog (title) VALUES ('b1')")
            .execute(&pool)
            .await
            .expect("seed blog");
        for child in [1_i64, 2, 3] {
            sqlx::query("INSERT INTO pac_blog_categories (parent_id, child_id) VALUES (1, ?)")
                .bind(child)
                .execute(&pool)
                .await
                .expect("seed junction");
        }

        // Post "p1" with 2 comments.
        sqlx::query("INSERT INTO pac_post (title) VALUES ('p1')")
            .execute(&pool)
            .await
            .expect("seed post");
        for body in &["first", "second"] {
            sqlx::query("INSERT INTO pac_comment (body, post) VALUES (?, 1)")
                .bind(*body)
                .execute(&pool)
                .await
                .expect("seed comment");
        }

        pool
    })
    .await
    .clone()
}

// =========================================================================
// Tests
// =========================================================================

/// The core M2M proof: `.prefetch_related("categories")` populates the
/// `M2M` cache, and the generated `blog.categories()` accessor serves it
/// with ZERO further queries across `fetch()`/`count()`/`exists()`.
#[tokio::test]
async fn m2m_accessor_serves_prefetch_cache_zero_queries() {
    let _g = query_lock().await;
    boot().await;

    let blog = Blog::objects()
        .filter(blog::ID.eq(1))
        .prefetch_related("categories")
        .get()
        .await
        .expect("get with prefetch_related");
    assert_eq!(
        blog.categories.resolved().map(|r| r.len()),
        Some(3),
        "sanity: prefetch populated the M2M cache with all 3 categories"
    );

    reset().await;
    let fetched = blog.categories().fetch().await.expect("cache-served fetch");
    assert_eq!(fetched.len(), 3, "fetch() must return all 3 categories");
    assert_eq!(
        count(),
        0,
        "blog.categories().fetch() must serve the prefetch cache with zero queries"
    );

    reset().await;
    let n = blog.categories().count().await.expect("cache-served count");
    assert_eq!(n, 3);
    assert_eq!(
        count(),
        0,
        "blog.categories().count() must serve the prefetch cache with zero queries"
    );

    reset().await;
    let exists = blog
        .categories()
        .exists()
        .await
        .expect("cache-served exists");
    assert!(exists);
    assert_eq!(
        count(),
        0,
        "blog.categories().exists() must serve the prefetch cache with zero queries"
    );

    reset().await;
    let first = blog.categories().first().await.expect("cache-served first");
    assert!(first.is_some());
    assert_eq!(
        count(),
        0,
        "blog.categories().first() must serve the prefetch cache with zero queries"
    );
}

/// Adding a `.filter()` after the cache-serving accessor invalidates the
/// slot: the cache is the UNFILTERED set, so a narrower query MUST re-run
/// against the database and return the correctly-filtered result.
#[tokio::test]
async fn m2m_accessor_filter_after_prefetch_requeries() {
    let _g = query_lock().await;
    boot().await;

    let blog = Blog::objects()
        .filter(blog::ID.eq(1))
        .prefetch_related("categories")
        .get()
        .await
        .expect("get with prefetch_related");

    reset().await;
    let active_count = blog
        .categories()
        .filter(category::ACTIVE.eq(true))
        .count()
        .await
        .expect("filtered count re-queries");
    assert_eq!(
        active_count, 2,
        "filter(active=true) must return only the 2 active categories, not all 3 cached ones"
    );
    assert!(
        count() >= 1,
        "adding .filter() after the accessor must invalidate the cache and issue a real query"
    );
}

/// Reverse-FK sibling of the M2M proof: `post.comment_set()` serves the
/// `.prefetch_related("comment_set")`-loaded `ReverseSet` cache with zero
/// further queries.
#[tokio::test]
async fn reverse_fk_set_serves_prefetch_cache() {
    let _g = query_lock().await;
    boot().await;

    let post = Post::objects()
        .filter(post::ID.eq(1))
        .prefetch_related("comment_set")
        .get()
        .await
        .expect("get with prefetch_related");
    assert_eq!(
        post.comment_set.resolved().map(|r| r.len()),
        Some(2),
        "sanity: prefetch populated the ReverseSet cache with both comments"
    );

    reset().await;
    let fetched = post
        .comment_set()
        .fetch()
        .await
        .expect("cache-served fetch");
    assert_eq!(fetched.len(), 2);
    assert_eq!(
        count(),
        0,
        "post.comment_set().fetch() must serve the prefetch cache with zero queries"
    );

    reset().await;
    let n = post
        .comment_set()
        .count()
        .await
        .expect("cache-served count");
    assert_eq!(n, 2);
    assert_eq!(count(), 0);
}

/// Fallback path stays intact for BOTH relation kinds: with no
/// `prefetch_related`, the accessors still resolve correctly — via a real
/// query.
#[tokio::test]
async fn accessor_without_prefetch_still_queries() {
    let _g = query_lock().await;
    boot().await;

    let blog = Blog::objects()
        .filter(blog::ID.eq(1))
        .get()
        .await
        .expect("get without prefetch_related");
    assert!(
        blog.categories.resolved().is_none(),
        "sanity: cache is empty without prefetch_related"
    );

    reset().await;
    let fetched = blog.categories().fetch().await.expect("un-cached fetch");
    assert_eq!(fetched.len(), 3);
    assert!(
        count() >= 1,
        "un-prefetched M2M accessor must fall back to a real query"
    );

    let post = Post::objects()
        .filter(post::ID.eq(1))
        .get()
        .await
        .expect("get without prefetch_related");
    assert!(post.comment_set.resolved().is_none());

    reset().await;
    let fetched = post.comment_set().fetch().await.expect("un-cached fetch");
    assert_eq!(fetched.len(), 2);
    assert!(
        count() >= 1,
        "un-prefetched reverse-FK accessor must fall back to a real query"
    );
}
