//! Feature #29 Phase 1 — `QuerySet::try_for_each` regression tests.
//!
//! Streams rows through a callback in `chunk_size` pages so a
//! million-row table doesn't OOM the way `fetch()` would. Pure unit
//! coverage: chunked walk visits every row exactly once and in PK
//! order, an empty result set is a no-op, callback errors halt the
//! walk on the first offending row, and a chunk_size larger than the
//! table degrades gracefully to a single fetch_all.

#![allow(dead_code, private_interfaces)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::TryForEachError;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Item {
    pub id: i64,
    pub label: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults always load in tests");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Item>()
            .build()
            .expect("App::build should succeed");
        // Schema + seed. Run inside the boot so every test sees the
        // same 25 rows in PK order without each test reseeding.
        sqlx::query(
            "CREATE TABLE item (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("create table");
        for i in 1..=25 {
            sqlx::query("INSERT INTO item (label) VALUES (?)")
                .bind(format!("row-{i}"))
                .execute(&pool)
                .await
                .expect("insert seed row");
        }
    })
    .await;
}

#[tokio::test]
async fn try_for_each_visits_every_row_across_chunks() {
    boot().await;
    let seen: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen.clone();
    Item::objects()
        .order_by(item::ID.asc())
        .try_for_each::<_, ()>(7, move |row| {
            seen_clone.lock().unwrap().push(row.id);
            Ok(())
        })
        .await
        .expect("happy-path walk should complete");
    let collected = seen.lock().unwrap().clone();
    assert_eq!(
        collected,
        (1..=25).collect::<Vec<i64>>(),
        "every row visited exactly once, in PK order, across multiple chunks",
    );
}

#[tokio::test]
async fn try_for_each_handles_chunk_size_larger_than_table() {
    boot().await;
    let count = AtomicUsize::new(0);
    Item::objects()
        .order_by(item::ID.asc())
        .try_for_each::<_, ()>(1000, |_row| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("oversized chunk should still visit every row in one fetch");
    assert_eq!(count.load(Ordering::Relaxed), 25);
}

#[tokio::test]
async fn try_for_each_short_circuits_on_callback_error() {
    boot().await;
    let count = AtomicUsize::new(0);
    let result: Result<(), TryForEachError<String>> = Item::objects()
        .order_by(item::ID.asc())
        .try_for_each(5, |row| {
            count.fetch_add(1, Ordering::Relaxed);
            if row.id == 3 {
                Err("stop at row 3".to_string())
            } else {
                Ok(())
            }
        })
        .await;
    match result {
        Err(TryForEachError::Callback(msg)) => assert_eq!(msg, "stop at row 3"),
        other => panic!("expected callback error, got {other:?}"),
    }
    assert_eq!(
        count.load(Ordering::Relaxed),
        3,
        "walk stops on first error — rows 4+ never invoke the callback",
    );
}

#[tokio::test]
async fn try_for_each_with_empty_filter_is_a_noop() {
    boot().await;
    let count = AtomicUsize::new(0);
    Item::objects()
        .filter(item::ID.lt(0))
        .try_for_each::<_, ()>(10, |_row| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .await
        .expect("empty filter is a no-op");
    assert_eq!(count.load(Ordering::Relaxed), 0);
}
