//! Gap 28 + Gap 37 — select_related (eager FK loading) and template context.
//!
//! Coverage:
//!
//! - **Single FK:** `.select_related("author")` populates `post.author.resolved()`.
//! - **Serde JSON:** after select_related, `serde_json::to_value(&post)["author"]`
//!   is a full object, not a bare integer.
//! - **Without select_related:** `serde_json::to_value(&post)["author"]` is
//!   still a bare integer (backward compat).
//! - **Multi-FK:** `.select_related_many(&["author", "reviewer"])` loads both.
//! - **Template access (gap 37):** `ctx["author"]["name"]` works as a string.
//! - **`resolved()` accessor:** returns `Some(&User)` after select_related.
//! - **`.resolve(&pool)`:** still works and returns a clone of the cached row.

#![allow(dead_code)]

use sqlx::SqlitePool;
use umbral::orm::ForeignKey;
use umbral_core::db;

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
    let pool = db::connect_sqlite("sqlite::memory:")
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

/// Without `.select_related`, `post.author.resolved()` is `None`.
#[tokio::test]
async fn without_select_related_resolved_is_none() {
    let pool = fresh_pool().await;
    let user = insert_user(&pool, "Alice", "alice").await;
    let _ = insert_post(&pool, "Hello", user.id, user.id).await;

    let post = Post::objects().on(&pool).get().await.expect("get post");

    assert!(
        post.author.resolved().is_none(),
        "without select_related, resolved() must be None"
    );
    assert_eq!(
        post.author.id(),
        user.id,
        "raw FK id should still be correct"
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

/// `.select_related("author")` populates `post.author.resolved()` with the
/// full User row.
#[tokio::test]
async fn select_related_single_fk_populates_resolved() {
    let pool = fresh_pool().await;
    let user = insert_user(&pool, "Alice", "alice").await;
    let _ = insert_post(&pool, "Hello", user.id, user.id).await;

    let post = Post::objects()
        .on(&pool)
        .select_related("author")
        .get()
        .await
        .expect("get with select_related");

    let resolved = post
        .author
        .resolved()
        .expect("select_related should have populated resolved()");

    assert_eq!(resolved.id, user.id);
    assert_eq!(resolved.name, "Alice");
    assert_eq!(resolved.username, "alice");
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

    let author = post.author.resolved().expect("author should be resolved");
    let reviewer = post
        .reviewer
        .resolved()
        .expect("reviewer should be resolved");

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

/// `.select_related("author").fetch()` populates resolved on every row.
#[tokio::test]
async fn select_related_fetch_populates_all_rows() {
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
    for post in &posts {
        assert!(
            post.author.resolved().is_some(),
            "every post should have resolved author; post id={}",
            post.id
        );
    }

    // The batch fetch should have correctly matched author to each post.
    let alice_posts: Vec<&Post> = posts.iter().filter(|p| p.author.id() == alice.id).collect();
    let bob_posts: Vec<&Post> = posts.iter().filter(|p| p.author.id() == bob.id).collect();

    assert_eq!(alice_posts.len(), 2);
    assert_eq!(bob_posts.len(), 1);
    assert_eq!(alice_posts[0].author.resolved().unwrap().username, "alice");
    assert_eq!(bob_posts[0].author.resolved().unwrap().username, "bob");
}

// =========================================================================
// resolve() still works (uses cached resolved when available)
// =========================================================================

/// After select_related, `.resolve(&pool)` returns the cached row without
/// an extra round-trip. The result is the same as `resolved().unwrap()`.
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
    let via_resolved = post.author.resolved().expect("resolved slot");

    assert_eq!(via_resolve.id, via_resolved.id);
    assert_eq!(via_resolve.username, "charlie");
}
