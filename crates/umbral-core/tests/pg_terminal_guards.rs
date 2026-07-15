//! gaps4 #24 — the low-level `_pg` explicit-pool terminals must REFUSE a
//! chained hydration feature they can't apply, instead of silently handing back
//! un-hydrated rows.
//!
//! The guard fires BEFORE the pool is touched, so these tests use
//! `PgPool::connect_lazy` (which never actually connects) — no live Postgres
//! needed. A guard failure surfaces as a clear `Protocol` error naming the
//! feature; a connection error would mean the guard didn't fire.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pgguard_author")]
pub struct Author {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "pgguard_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub author: umbral::orm::ForeignKey<Author>,
}

/// A pool that never connects — the guard returns before any query runs.
fn lazy_pool() -> sqlx::PgPool {
    sqlx::PgPool::connect_lazy("postgres://localhost/does_not_matter")
        .expect("connect_lazy validates the URL but does not connect")
}

fn is_guard_error(e: &sqlx::Error, feature: &str) -> bool {
    matches!(e, sqlx::Error::Protocol(msg) if msg.contains(feature) && msg.contains("fetch_pg"))
}

#[tokio::test]
async fn fetch_pg_refuses_select_related() {
    let pool = lazy_pool();
    let err = Post::objects()
        .select_related("author")
        .fetch_pg(&pool)
        .await
        .expect_err("select_related on fetch_pg must be refused, not silently dropped");
    assert!(
        is_guard_error(&err, "select_related"),
        "expected a guard error naming select_related, got: {err:?}"
    );
}

#[tokio::test]
async fn fetch_pg_refuses_prefetch_related() {
    let pool = lazy_pool();
    // prefetch names an M2M field; the guard fires on presence, before validation.
    let err = Post::objects()
        .prefetch_related("anything")
        .fetch_pg(&pool)
        .await
        .expect_err("prefetch_related on fetch_pg must be refused");
    assert!(
        is_guard_error(&err, "prefetch_related"),
        "expected a guard error naming prefetch_related, got: {err:?}"
    );
}

#[tokio::test]
async fn fetch_pg_refuses_only() {
    let pool = lazy_pool();
    let err = Post::objects()
        .only(&["id"])
        .fetch_pg(&pool)
        .await
        .expect_err("`.only()` on a typed fetch_pg must be refused");
    // `.only()` reuses the shared only-terminal error message.
    assert!(
        matches!(&err, sqlx::Error::Protocol(m) if m.contains("only")),
        "expected the `.only(...)` error, got: {err:?}"
    );
}

#[tokio::test]
async fn plain_fetch_pg_passes_the_guard_and_only_then_hits_the_pool() {
    let pool = lazy_pool();
    // No chained feature → the guard passes, and the ONLY failure left is the
    // (lazy) connection actually being attempted. That proves the guard didn't
    // spuriously fire on a plain query.
    let err = Post::objects()
        .fetch_pg(&pool)
        .await
        .expect_err("the lazy pool can't really connect");
    assert!(
        !matches!(&err, sqlx::Error::Protocol(m) if m.contains("not applied")),
        "a plain fetch_pg must pass the guard and fail only on the connection: {err:?}"
    );
}
