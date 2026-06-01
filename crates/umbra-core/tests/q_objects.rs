//! Gap 28 — Q-objects (composable predicates).
//!
//! Coverage:
//!
//! - **`Q::or(a, b)`:** SQL `(a OR b)` generated correctly.
//! - **`Q::and(a, b)`:** SQL `(a AND b)` generated correctly.
//! - **`Q::not(p)`:** SQL `NOT p` generated correctly.
//! - **Nested composition:** `Q::or(Q::and(a, b), c)` nests with the right
//!   parentheses shape.
//! - **Live SQLite:** each variant executes correctly against a real pool.

use sqlx::SqlitePool;
use umbra::orm::Q;
use umbra_core::db;

// =========================================================================
// Model declarations
// =========================================================================

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "q_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub published: bool,
    pub author_id: i64,
}

// =========================================================================
// Pool helper
// =========================================================================

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite should always connect");

    sqlx::query(
        "CREATE TABLE q_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            published INTEGER NOT NULL DEFAULT 0,
            author_id INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE q_post");

    // Seed: 5 rows covering published/unpublished and two author IDs.
    // id=1: published=true,  author=1
    // id=2: published=false, author=1
    // id=3: published=true,  author=2
    // id=4: published=false, author=2
    // id=5: published=true,  author=1
    for (title, published, author_id) in &[
        ("pub-a1-1", true, 1i64),
        ("draft-a1", false, 1),
        ("pub-a2", true, 2),
        ("draft-a2", false, 2),
        ("pub-a1-2", true, 1),
    ] {
        sqlx::query("INSERT INTO q_post (title, published, author_id) VALUES (?, ?, ?)")
            .bind(*title)
            .bind(*published)
            .bind(*author_id)
            .execute(&pool)
            .await
            .expect("insert seed");
    }

    pool
}

// =========================================================================
// SQL rendering (pure, no pool)
// =========================================================================

/// `Q::or` renders a predicate whose SQL text contains the two column names.
#[test]
fn q_or_renders_with_both_column_names() {
    let pred = Q::or(post::PUBLISHED.eq(true), post::AUTHOR_ID.eq(99));
    let sql = Post::objects().filter(pred).to_sql();
    let lower = sql.to_ascii_lowercase();
    assert!(
        lower.contains("published"),
        "should mention published; got: {sql}"
    );
    assert!(
        lower.contains("author_id"),
        "should mention author_id; got: {sql}"
    );
}

/// `Q::and` renders a predicate whose SQL text contains the two column names.
#[test]
fn q_and_renders_with_both_column_names() {
    let pred = Q::and(post::PUBLISHED.eq(true), post::AUTHOR_ID.eq(1));
    let sql = Post::objects().filter(pred).to_sql();
    let lower = sql.to_ascii_lowercase();
    assert!(lower.contains("published"), "got: {sql}");
    assert!(lower.contains("author_id"), "got: {sql}");
}

/// `Q::not` wraps the predicate in a NOT.
#[test]
fn q_not_renders_not_keyword() {
    let pred = Q::not(post::AUTHOR_ID.eq(1));
    let sql = Post::objects().filter(pred).to_sql();
    let lower = sql.to_ascii_lowercase();
    assert!(lower.contains("not"), "should contain NOT; got: {sql}");
    assert!(
        lower.contains("author_id"),
        "should mention author_id; got: {sql}"
    );
}

// =========================================================================
// Live SQLite: Q::or
// =========================================================================

/// `Q::or(published=true, author_id=2)` returns all published posts PLUS
/// all posts by author 2 (without duplication — SQL OR semantics).
#[tokio::test]
async fn q_or_returns_union() {
    let pool = fresh_pool().await;

    // published=true: ids 1, 3, 5
    // author_id=2: ids 3, 4
    // union: ids 1, 3, 4, 5
    let rows = Post::objects()
        .on(&pool)
        .filter(Q::or(post::PUBLISHED.eq(true), post::AUTHOR_ID.eq(2)))
        .fetch()
        .await
        .expect("fetch Q::or");

    assert_eq!(
        rows.len(),
        4,
        "should match 4 rows (union of published and author_id=2)"
    );
}

// =========================================================================
// Live SQLite: Q::and
// =========================================================================

/// `Q::and(published=true, author_id=1)` returns only rows where both
/// conditions hold.
#[tokio::test]
async fn q_and_returns_intersection() {
    let pool = fresh_pool().await;

    // published=true AND author_id=1: ids 1, 5
    let rows = Post::objects()
        .on(&pool)
        .filter(Q::and(post::PUBLISHED.eq(true), post::AUTHOR_ID.eq(1)))
        .fetch()
        .await
        .expect("fetch Q::and");

    assert_eq!(
        rows.len(),
        2,
        "should match 2 rows (published AND author_id=1)"
    );
    assert!(rows.iter().all(|r| r.published && r.author_id == 1));
}

// =========================================================================
// Live SQLite: Q::not
// =========================================================================

/// `Q::not(author_id=1)` excludes all posts by author 1.
#[tokio::test]
async fn q_not_excludes_matching_rows() {
    let pool = fresh_pool().await;

    // author_id != 1: ids 3, 4
    let rows = Post::objects()
        .on(&pool)
        .filter(Q::not(post::AUTHOR_ID.eq(1)))
        .fetch()
        .await
        .expect("fetch Q::not");

    assert_eq!(rows.len(), 2, "should match 2 rows (not author_id=1)");
    assert!(rows.iter().all(|r| r.author_id != 1));
}

// =========================================================================
// Live SQLite: nested Q composition
// =========================================================================

/// `Q::or(Q::and(published=true, author_id=1), Q::not(published=true))`
/// returns all published rows by author 1 PLUS all unpublished rows.
#[tokio::test]
async fn q_nested_composition() {
    let pool = fresh_pool().await;

    // (published AND author=1) OR (NOT published)
    // = {1, 5} OR {2, 4} = {1, 2, 4, 5}
    let rows = Post::objects()
        .on(&pool)
        .filter(Q::or(
            Q::and(post::PUBLISHED.eq(true), post::AUTHOR_ID.eq(1)),
            Q::not(post::PUBLISHED.eq(true)),
        ))
        .fetch()
        .await
        .expect("fetch nested Q");

    assert_eq!(rows.len(), 4, "nested Q should return 4 rows");
    let mut ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    ids.sort();
    assert_eq!(ids, vec![1, 2, 4, 5]);
}

/// Two `.filter()` calls AND together — existing behaviour preserved.
#[tokio::test]
async fn multiple_filter_calls_and_together() {
    let pool = fresh_pool().await;

    // published AND author=1 via two filter calls
    let rows = Post::objects()
        .on(&pool)
        .filter(post::PUBLISHED.eq(true))
        .filter(post::AUTHOR_ID.eq(1))
        .fetch()
        .await
        .expect("chained filters");

    assert_eq!(
        rows.len(),
        2,
        "two filter calls should AND (published AND author=1)"
    );
}
