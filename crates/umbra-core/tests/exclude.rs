//! Gap 15 — `QuerySet::exclude(p)` / `Manager::exclude(p)`.
//!
//! The negated complement of `filter()`: every row where `p` does NOT hold.
//! Implementation-wise it's sugar for `filter(Q::not(p))` — the predicate
//! chain still ANDs, so `.filter(A).exclude(B).filter(C)` is
//! `WHERE A AND NOT B AND C`.

use sqlx::SqlitePool;
use umbra_core::db;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "ex_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub published: bool,
    pub author_id: i64,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite should always connect");

    sqlx::query(
        "CREATE TABLE ex_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            published INTEGER NOT NULL DEFAULT 0,
            author_id INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE ex_post");

    // id=1 published=true  author=1
    // id=2 published=false author=1
    // id=3 published=true  author=2
    // id=4 published=false author=2
    // id=5 published=true  author=1
    for (title, published, author_id) in &[
        ("pub-a1-1", true, 1i64),
        ("draft-a1", false, 1),
        ("pub-a2", true, 2),
        ("draft-a2", false, 2),
        ("pub-a1-2", true, 1),
    ] {
        sqlx::query("INSERT INTO ex_post (title, published, author_id) VALUES (?, ?, ?)")
            .bind(*title)
            .bind(*published)
            .bind(*author_id)
            .execute(&pool)
            .await
            .expect("insert seed");
    }
    pool
}

#[tokio::test]
async fn exclude_drops_matching_rows_on_queryset() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .exclude(post::PUBLISHED.eq(true))
        .on(&pool)
        .fetch()
        .await
        .expect("exclude fetch");
    assert_eq!(rows.len(), 2, "two unpublished rows remain: got {rows:?}");
    assert!(rows.iter().all(|r| !r.published));
}

#[tokio::test]
async fn exclude_drops_matching_rows_on_manager() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .exclude(post::AUTHOR_ID.eq(1))
        .on(&pool)
        .fetch()
        .await
        .expect("manager exclude fetch");
    assert_eq!(rows.len(), 2, "two rows with author_id != 1: got {rows:?}");
    assert!(rows.iter().all(|r| r.author_id != 1));
}

#[tokio::test]
async fn exclude_composes_with_filter() {
    let pool = fresh_pool().await;
    // published = true AND NOT (author_id = 2)
    // matches id=1 and id=5; excludes id=3 (published & author=2).
    let rows = Post::objects()
        .filter(post::PUBLISHED.eq(true))
        .exclude(post::AUTHOR_ID.eq(2))
        .on(&pool)
        .fetch()
        .await
        .expect("filter+exclude fetch");
    assert_eq!(rows.len(), 2, "got: {rows:?}");
    assert!(rows.iter().all(|r| r.published && r.author_id == 1));
}

#[tokio::test]
async fn multiple_excludes_and_together() {
    let pool = fresh_pool().await;
    // NOT(published=false) AND NOT(author_id=2) — all published rows by author 1.
    let rows = Post::objects()
        .exclude(post::PUBLISHED.eq(false))
        .exclude(post::AUTHOR_ID.eq(2))
        .on(&pool)
        .fetch()
        .await
        .expect("two excludes fetch");
    assert_eq!(rows.len(), 2, "got: {rows:?}");
    assert!(rows.iter().all(|r| r.published && r.author_id == 1));
}

#[test]
fn exclude_renders_not_in_sql() {
    let sql = Post::objects()
        .exclude(post::PUBLISHED.eq(true))
        .to_sql()
        .to_ascii_lowercase();
    assert!(
        sql.contains("not"),
        "expected NOT in rendered SQL; got: {sql}"
    );
    assert!(
        sql.contains("published"),
        "expected `published` in rendered SQL; got: {sql}"
    );
}
