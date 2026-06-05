//! Gap #26 — `col.in_subquery(qs.into_subquery("col"))`.
//!
//! v1 supports the `col IN (SELECT col FROM ...)` shape. EXISTS with
//! correlated outer-refs is deferred — most "is there a row that
//! references me" queries can be expressed as `id IN (SELECT fk FROM
//! child WHERE ...)`, which the in_subquery path covers.

#![allow(dead_code)]

use sqlx::SqlitePool;
use umbra::orm::ForeignKey;
use umbra_core::db;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "sq_user")]
pub struct User {
    pub id: i64,
    pub username: String,
    pub is_staff: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "sq_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<User>,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE sq_user (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            is_staff BOOLEAN NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE sq_user");
    sqlx::query(
        "CREATE TABLE sq_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author INTEGER NOT NULL REFERENCES sq_user(id)
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE sq_post");
    // Users: 1=alice (staff), 2=bob (not), 3=carol (staff)
    sqlx::query(
        "INSERT INTO sq_user (id, username, is_staff) VALUES \
            (1, 'alice', 1), (2, 'bob', 0), (3, 'carol', 1)",
    )
    .execute(&pool)
    .await
    .expect("seed users");
    // Posts: 1+2 by alice (1), 3 by bob (2), 4 by carol (3)
    sqlx::query(
        "INSERT INTO sq_post (id, title, author) VALUES \
            (1, 'a1', 1), (2, 'a2', 1), (3, 'b1', 2), (4, 'c1', 3)",
    )
    .execute(&pool)
    .await
    .expect("seed posts");
    pool
}

#[tokio::test]
async fn fk_in_subquery_filters_by_related_table_predicate() {
    let pool = fresh_pool().await;
    // Posts whose author IS staff. Build the staff-user subquery,
    // then filter Posts where author IN that subquery.
    let staff_users = User::objects()
        .filter(user::IS_STAFF.eq(true))
        .into_subquery("id");
    let posts = Post::objects()
        .filter(post::AUTHOR.in_subquery(staff_users))
        .on(&pool)
        .fetch()
        .await
        .expect("fk_in_subquery");
    assert_eq!(posts.len(), 3, "alice (2) + carol (1) — three posts");
}

#[tokio::test]
async fn int_in_subquery_filters_by_id_set() {
    let pool = fresh_pool().await;
    // Users whose id is the author of at least one post (i.e. any
    // user with posts).
    let authored_ids = Post::objects().into_subquery("author");
    let users = User::objects()
        .filter(user::ID.in_subquery(authored_ids))
        .on(&pool)
        .fetch()
        .await
        .expect("int_in_subquery");
    assert_eq!(users.len(), 3, "all 3 users have at least one post");
}

#[tokio::test]
async fn fk_in_subquery_with_no_matches_returns_empty() {
    let pool = fresh_pool().await;
    // Authors of "no post titled banana" — there are none.
    let staff_with_banana = User::objects()
        .filter(user::USERNAME.eq("not-a-real-user"))
        .into_subquery("id");
    let posts = Post::objects()
        .filter(post::AUTHOR.in_subquery(staff_with_banana))
        .on(&pool)
        .fetch()
        .await
        .expect("subquery empty");
    assert!(posts.is_empty());
}
