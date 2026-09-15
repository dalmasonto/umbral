//! Gap 28 + Gap 37 — select_related (eager FK loading) and template context.
//!
//! Coverage:
//!
//! - **Single FK:** `.select_related("author")` makes `post.author().await?`
//!   zero-query.
//! - **Serde JSON:** after select_related, `serde_json::to_value(&post)["author"]`
//!   is a full object, not a bare integer.
//! - **Without select_related:** `serde_json::to_value(&post)["author"]` is
//!   still a bare integer (backward compat).
//! - **Multi-FK:** `.select_related_many(&["author", "reviewer"])` loads both.
//! - **Template access (gap 37):** `ctx["author"]["name"]` works as a string.
//! - **Awaited accessor:** `post.author().await?` returns the hydrated
//!   `User` after select_related.
//! - **`.resolve(&pool)`:** still works and returns a clone of the cached row.

#![allow(dead_code)]

use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use std::str::FromStr;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, MutexGuard};
use tracing::Subscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use umbral::orm::ForeignKey;

// =========================================================================
// Query counter — this file is its own test binary/process (Rust compiles
// each `tests/*.rs` file separately), so it carries its own copy of the
// sqlx-tracing query counter — same duplication rationale as
// `tests/query_counts.rs` / `tests/prefetch_accessor_cache.rs`.
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
// Model declarations
// =========================================================================

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
)]
#[umbral(table = "sr_user")]
pub struct User {
    pub id: i64,
    pub name: String,
    pub username: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "sr_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<User>,
    pub reviewer: ForeignKey<User>,
}

// =========================================================================
// Pool helper
// =========================================================================

async fn fresh_pool() -> SqlitePool {
    // Built directly (not via `umbral_core::db::connect_sqlite`, which sets
    // `log_statements(Off)` for runtime performance and so suppresses the
    // `sqlx::query` tracing events the query counter above relies on — same
    // reasoning as `tests/query_counts.rs`'s `boot_and_seed`). `shared_cache`
    // keeps every connection in this pool seeing the same in-memory DB.
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("sqlite opts")
        .shared_cache(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .min_connections(1)
        .connect_with(opts)
        .await
        .expect("in-memory SQLite");

    sqlx::query(
        "CREATE TABLE sr_user (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            username TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE sr_user");

    sqlx::query(
        "CREATE TABLE sr_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author INTEGER NOT NULL REFERENCES sr_user(id),
            reviewer INTEGER NOT NULL REFERENCES sr_user(id)
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE sr_post");

    pool
}

/// Insert a user and return it.
async fn insert_user(pool: &SqlitePool, name: &str, username: &str) -> User {
    sqlx::query_as::<sqlx::Sqlite, User>(
        "INSERT INTO sr_user (name, username) VALUES (?, ?) RETURNING id, name, username",
    )
    .bind(name)
    .bind(username)
    .fetch_one(pool)
    .await
    .expect("insert user")
}

/// Insert a post and return it (without select_related — raw integer FKs).
async fn insert_post(pool: &SqlitePool, title: &str, author_id: i64, reviewer_id: i64) -> Post {
    sqlx::query_as::<sqlx::Sqlite, Post>(
        "INSERT INTO sr_post (title, author, reviewer)
         VALUES (?, ?, ?)
         RETURNING id, title, author, reviewer",
    )
    .bind(title)
    .bind(author_id)
    .bind(reviewer_id)
    .fetch_one(pool)
    .await
    .expect("insert post")
}

// =========================================================================
// Without select_related — backward compat
// =========================================================================

/// Without `.select_related`, the cache is empty, so `post.author().await?`
/// must fall back to a real query (proven via the query counter) and still
/// return the correct row.
#[tokio::test]
async fn without_select_related_accessor_still_queries() {
    let _g = query_lock().await;
    let pool = fresh_pool().await;
    let user = insert_user(&pool, "Alice", "alice").await;
    let _ = insert_post(&pool, "Hello", user.id, user.id).await;

    let post = Post::objects().on(&pool).get().await.expect("get post");
    assert_eq!(
        post.author.id(),
        user.id,
        "raw FK id should still be correct"
    );

    reset();
    let author = post
        .author()
        .on(&pool)
        .await
        .expect("un-cached accessor still fetches");
    assert_eq!(author.id, user.id);
    assert!(
        count() >= 1,
        "without select_related, the accessor must issue a real query"
    );
}

/// Without `.select_related`, `serde_json::to_value` emits author as a bare i64.
#[tokio::test]
async fn without_select_related_serialises_as_integer() {
    let pool = fresh_pool().await;
    let user = insert_user(&pool, "Alice", "alice").await;
    let _ = insert_post(&pool, "Hello", user.id, user.id).await;

    let post = Post::objects().on(&pool).get().await.expect("get post");

    let json = serde_json::to_value(&post).expect("serialize");
    assert_eq!(
        json["author"],
        serde_json::Value::Number(user.id.into()),
        "without select_related, author JSON should be a bare integer"
    );
}

// =========================================================================
// With select_related — single FK
// =========================================================================

/// `.select_related("author")` makes `post.author().await?` return the full
/// User row with ZERO further queries (served from the cache, proven via
/// the query counter).
#[tokio::test]
async fn select_related_single_fk_makes_accessor_zero_query() {
    let _g = query_lock().await;
    let pool = fresh_pool().await;
    let user = insert_user(&pool, "Alice", "alice").await;
    let _ = insert_post(&pool, "Hello", user.id, user.id).await;

    let post = Post::objects()
        .on(&pool)
        .select_related("author")
        .get()
        .await
        .expect("get with select_related");

    reset();
    let author = post
        .author()
        .on(&pool)
        .await
        .expect("select_related should have populated the cache");
    assert_eq!(
        count(),
        0,
        "post.author().await must serve the select_related cache with zero queries"
    );

    assert_eq!(author.id, user.id);
    assert_eq!(author.name, "Alice");
    assert_eq!(author.username, "alice");
}

/// After select_related, `serde_json::to_value` emits author as a full object.
///
/// This is the gap 37 case: template `{{ post.author.username }}` resolves to
/// "alice" when `post` is passed as the context after `select_related("author")`.
#[tokio::test]
async fn select_related_serialises_as_full_object() {
    let pool = fresh_pool().await;
    let user = insert_user(&pool, "Alice", "alice").await;
    let _ = insert_post(&pool, "Hello", user.id, user.id).await;

    let post = Post::objects()
        .on(&pool)
        .select_related("author")
        .get()
        .await
        .expect("get with select_related");

    let ctx = serde_json::to_value(&post).expect("serialize");

    // The author key should be a JSON object, not an integer.
    assert!(
        ctx["author"].is_object(),
        "after select_related, author should serialize as an object; got {:?}",
        ctx["author"]
    );
    assert_eq!(
        ctx["author"]["username"], "alice",
        "ctx[author][username] should be 'alice'"
    );
    assert_eq!(ctx["author"]["name"], "Alice");
    assert_eq!(
        ctx["author"]["id"],
        serde_json::Value::Number(user.id.into())
    );
}

/// `post.author.id()` still returns the raw integer after select_related
/// (the ID is never lost — it's preserved in `raw`).
#[tokio::test]
async fn select_related_raw_id_preserved() {
    let pool = fresh_pool().await;
    let user = insert_user(&pool, "Bob", "bob").await;
    let _ = insert_post(&pool, "test", user.id, user.id).await;

    let post = Post::objects()
        .on(&pool)
        .select_related("author")
        .get()
        .await
        .expect("get");

    assert_eq!(
        post.author.id(),
        user.id,
        "raw id() must equal the stored FK integer"
    );
}

// =========================================================================
// With select_related — multiple FKs
// =========================================================================

/// `.select_related_many(&["author", "reviewer"])` populates both FKs.
#[tokio::test]
async fn select_related_many_populates_both_fks() {
    let pool = fresh_pool().await;
    let alice = insert_user(&pool, "Alice", "alice").await;
    let bob = insert_user(&pool, "Bob", "bob").await;
    let _ = insert_post(&pool, "collab", alice.id, bob.id).await;

    let post = Post::objects()
        .on(&pool)
        .select_related_many(&["author", "reviewer"])
        .get()
        .await
        .expect("get with select_related_many");

    let author = post.author().on(&pool).await.expect("author resolved");
    let reviewer = post.reviewer().on(&pool).await.expect("reviewer resolved");

    assert_eq!(author.username, "alice");
    assert_eq!(reviewer.username, "bob");
}

/// With two FKs resolved, serde emits both as objects.
#[tokio::test]
async fn select_related_many_both_serialise_as_objects() {
    let pool = fresh_pool().await;
    let alice = insert_user(&pool, "Alice", "alice").await;
    let bob = insert_user(&pool, "Bob", "bob").await;
    let _ = insert_post(&pool, "collab", alice.id, bob.id).await;

    let post = Post::objects()
        .on(&pool)
        .select_related_many(&["author", "reviewer"])
        .get()
        .await
        .expect("get");

    let ctx = serde_json::to_value(&post).expect("serialize");
    assert!(
        ctx["author"].is_object(),
        "author should be object; got {:?}",
        ctx["author"]
    );
    assert!(
        ctx["reviewer"].is_object(),
        "reviewer should be object; got {:?}",
        ctx["reviewer"]
    );
    assert_eq!(ctx["author"]["username"], "alice");
    assert_eq!(ctx["reviewer"]["username"], "bob");
}

// =========================================================================
// select_related + multiple rows (fetch)
// =========================================================================

/// `.select_related("author").fetch()` makes `post.author().await?`
/// zero-query on every row.
#[tokio::test]
async fn select_related_fetch_populates_all_rows() {
    let _g = query_lock().await;
    let pool = fresh_pool().await;
    let alice = insert_user(&pool, "Alice", "alice").await;
    let bob = insert_user(&pool, "Bob", "bob").await;

    // Alice authors 2 posts, Bob authors 1.
    let _ = insert_post(&pool, "post-1", alice.id, alice.id).await;
    let _ = insert_post(&pool, "post-2", alice.id, alice.id).await;
    let _ = insert_post(&pool, "post-3", bob.id, bob.id).await;

    let posts = Post::objects()
        .on(&pool)
        .select_related("author")
        .fetch()
        .await
        .expect("fetch with select_related");

    assert_eq!(posts.len(), 3);

    reset();
    let mut alice_posts: Vec<&Post> = Vec::new();
    let mut bob_posts: Vec<&Post> = Vec::new();
    for post in &posts {
        let author = post
            .author()
            .on(&pool)
            .await
            .unwrap_or_else(|_| panic!("post id={} should have a resolved author", post.id));
        if author.id == alice.id {
            alice_posts.push(post);
        } else if author.id == bob.id {
            bob_posts.push(post);
        }
    }
    assert_eq!(
        count(),
        0,
        "every row's post.author().await must serve select_related's batched cache with zero \
         further queries"
    );

    assert_eq!(alice_posts.len(), 2);
    assert_eq!(bob_posts.len(), 1);
    assert_eq!(
        alice_posts[0].author().on(&pool).await.unwrap().username,
        "alice"
    );
    assert_eq!(
        bob_posts[0].author().on(&pool).await.unwrap().username,
        "bob"
    );
}

// =========================================================================
// resolve() still works (uses cached resolved when available)
// =========================================================================

/// After select_related, `.resolve(&pool)` returns the cached row without
/// an extra round-trip. The result is the same as the awaited accessor's.
#[tokio::test]
async fn resolve_returns_cached_row_after_select_related() {
    let pool = fresh_pool().await;
    let user = insert_user(&pool, "Charlie", "charlie").await;
    let _ = insert_post(&pool, "cached", user.id, user.id).await;

    let post = Post::objects()
        .on(&pool)
        .select_related("author")
        .get()
        .await
        .expect("get");

    let via_resolve = post.author.resolve(&pool).await.expect("resolve");
    let via_accessor = post.author().on(&pool).await.expect("accessor");

    assert_eq!(via_resolve.id, via_accessor.id);
    assert_eq!(via_resolve.username, "charlie");
}
