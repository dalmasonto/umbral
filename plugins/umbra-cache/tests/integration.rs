//! Integration coverage for umbra-cache. Two suites: one against
//! the in-memory backend, one against the SQLite backend. The shape
//! is intentionally duplicated rather than abstracted into a
//! parameterised harness; the cost of two near-identical blocks is
//! lower than the cost of a macro or a trait gymnastic that hides
//! which backend a failure came from.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use tempfile::NamedTempFile;
use umbra_cache::{Cache, SqliteBackend};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Article {
    id: i64,
    title: String,
    body: String,
}

// ---- memory backend ----

#[tokio::test]
async fn memory_get_miss_returns_none() {
    let c = Cache::memory();
    assert!(c.get::<String>("missing").await.is_none());
}

#[tokio::test]
async fn memory_round_trip_a_string() {
    let c = Cache::memory();
    c.set("greeting", "hi there", None).await.unwrap();
    let v: Option<String> = c.get("greeting").await;
    assert_eq!(v.as_deref(), Some("hi there"));
}

#[tokio::test]
async fn memory_round_trip_a_struct() {
    let c = Cache::memory();
    let a = Article {
        id: 7,
        title: "umbra".into(),
        body: "shadow framework".into(),
    };
    c.set("art:7", &a, None).await.unwrap();
    let back: Option<Article> = c.get("art:7").await;
    assert_eq!(back, Some(a));
}

#[tokio::test]
async fn memory_delete_evicts_a_key() {
    let c = Cache::memory();
    c.set("k", "v", None).await.unwrap();
    c.delete("k").await;
    let v: Option<String> = c.get("k").await;
    assert!(v.is_none());
}

#[tokio::test]
async fn memory_clear_drops_every_key() {
    let c = Cache::memory();
    c.set("a", "1", None).await.unwrap();
    c.set("b", "2", None).await.unwrap();
    c.clear().await;
    assert!(c.get::<String>("a").await.is_none());
    assert!(c.get::<String>("b").await.is_none());
}

#[tokio::test]
async fn memory_ttl_expires_the_entry() {
    let c = Cache::memory();
    c.set("short", "lived", Some(Duration::from_millis(50)))
        .await
        .unwrap();
    let early: Option<String> = c.get("short").await;
    assert_eq!(early.as_deref(), Some("lived"));
    tokio::time::sleep(Duration::from_millis(80)).await;
    let late: Option<String> = c.get("short").await;
    assert!(late.is_none(), "TTL should have expired the entry");
}

// ---- sqlite backend ----

async fn sqlite_cache() -> (NamedTempFile, Cache, SqliteBackend) {
    let tmp = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}?mode=rwc", tmp.path().display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let backend = SqliteBackend::new(pool.clone()).await.unwrap();
    // build a parallel Cache against the same pool; both views see
    // the same rows.
    let cache = Cache::sqlite(pool).await.unwrap();
    (tmp, cache, backend)
}

#[tokio::test]
async fn sqlite_get_miss_returns_none() {
    let (_tmp, c, _b) = sqlite_cache().await;
    assert!(c.get::<String>("missing").await.is_none());
}

#[tokio::test]
async fn sqlite_round_trip_and_overwrite() {
    let (_tmp, c, _b) = sqlite_cache().await;
    c.set("k", "first", None).await.unwrap();
    assert_eq!(c.get::<String>("k").await.as_deref(), Some("first"));
    c.set("k", "second", None).await.unwrap();
    assert_eq!(c.get::<String>("k").await.as_deref(), Some("second"));
}

#[tokio::test]
async fn sqlite_ttl_expires_the_entry() {
    let (_tmp, c, _b) = sqlite_cache().await;
    c.set("short", "lived", Some(Duration::from_millis(50)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(c.get::<String>("short").await.is_none());
}

#[tokio::test]
async fn sqlite_sweep_removes_expired_rows() {
    let (_tmp, c, b) = sqlite_cache().await;
    c.set("keep", "alive", None).await.unwrap();
    c.set("drop", "soon", Some(Duration::from_millis(20)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    let removed = b.sweep().await.unwrap();
    assert_eq!(removed, 1);
    assert_eq!(c.get::<String>("keep").await.as_deref(), Some("alive"));
}

#[tokio::test]
async fn sqlite_clear_empties_the_table() {
    let (_tmp, c, _b) = sqlite_cache().await;
    c.set("a", "1", None).await.unwrap();
    c.set("b", "2", None).await.unwrap();
    c.clear().await;
    assert!(c.get::<String>("a").await.is_none());
    assert!(c.get::<String>("b").await.is_none());
}
