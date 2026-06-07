//! Gaps #24 + #36 — DB function helpers (`Lower` / `Upper` / `Length`)
//! and date-extract helpers (`year` / `month` / `day`).
//!
//! Column extension methods return a `ColExpr<T>` whose comparison
//! operators (`.eq`, `.ne`, `.gt`, `.lt`, `.in_`) produce
//! `Predicate<T>` values ready to feed `QuerySet::filter`. Backend
//! dispatch is hidden behind `Predicate`'s per-backend rendering:
//! Postgres gets `EXTRACT(YEAR FROM col)`; SQLite gets
//! `CAST(strftime('%Y', col) AS INTEGER)`.

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use umbra::orm::column::{DateTimeColExt, StrColExt};
use umbra_core::db;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model,
)]
#[umbra(table = "fn_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub created_at: DateTime<Utc>,
}

async fn fresh_pool() -> SqlitePool {
    let pool = db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query(
        "CREATE TABLE fn_post (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
    for (title, ts) in &[
        ("Hello", "2024-06-04T12:00:00+00:00"),
        ("WORLD", "2025-01-15T08:30:00+00:00"),
        ("rust", "2026-06-04T10:00:00+00:00"),
        ("Mixed Case", "2026-06-20T15:00:00+00:00"),
    ] {
        sqlx::query("INSERT INTO fn_post (title, created_at) VALUES (?, ?)")
            .bind(*title)
            .bind(*ts)
            .execute(&pool)
            .await
            .expect("seed");
    }
    pool
}

// =====================================================================
// String functions
// =====================================================================

#[tokio::test]
async fn lower_eq_finds_case_insensitive_match() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .filter(post::TITLE.lower().eq("hello"))
        .on(&pool)
        .fetch()
        .await
        .expect("filter LOWER");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Hello");
}

#[tokio::test]
async fn upper_eq_finds_case_insensitive_match() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .filter(post::TITLE.upper().eq("WORLD"))
        .on(&pool)
        .fetch()
        .await
        .expect("filter UPPER");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "WORLD");
}

#[tokio::test]
async fn length_lt_filters_by_string_length() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .filter(post::TITLE.length().lt(6))
        .on(&pool)
        .fetch()
        .await
        .expect("filter LENGTH");
    // "Hello" (5), "WORLD" (5), "rust" (4) — three < 6.
    let mut titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
    titles.sort();
    assert_eq!(titles, vec!["Hello", "WORLD", "rust"]);
}

// =====================================================================
// Date extract functions
// =====================================================================

#[tokio::test]
async fn year_eq_filters_by_year() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .filter(post::CREATED_AT.year().eq(2026))
        .on(&pool)
        .fetch()
        .await
        .expect("filter YEAR");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.created_at.format("%Y").to_string() == "2026")
    );
}

#[tokio::test]
async fn month_eq_filters_by_month() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .filter(post::CREATED_AT.month().eq(6))
        .on(&pool)
        .fetch()
        .await
        .expect("filter MONTH");
    // Hello (2024-06), rust (2026-06), Mixed Case (2026-06) — three June rows.
    assert_eq!(rows.len(), 3);
}

#[tokio::test]
async fn day_eq_filters_by_day_of_month() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .filter(post::CREATED_AT.day().eq(4))
        .on(&pool)
        .fetch()
        .await
        .expect("filter DAY");
    // Two rows on the 4th: Hello + rust.
    assert_eq!(rows.len(), 2);
}

#[tokio::test]
async fn year_and_month_compose() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .filter(post::CREATED_AT.year().eq(2026))
        .filter(post::CREATED_AT.month().eq(6))
        .on(&pool)
        .fetch()
        .await
        .expect("filter YEAR+MONTH");
    // Both June-2026 rows.
    assert_eq!(rows.len(), 2);
}

// =====================================================================
// Feature #36 — hour / minute / second / week_day extracts
// =====================================================================

#[tokio::test]
async fn hour_eq_filters_by_hour_of_day() {
    let pool = fresh_pool().await;
    // 12:00, 08:30, 10:00, 15:00 — `.hour().eq(12)` matches only Hello.
    let rows = Post::objects()
        .filter(post::CREATED_AT.hour().eq(12))
        .on(&pool)
        .fetch()
        .await
        .expect("filter HOUR");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Hello");
}

#[tokio::test]
async fn minute_eq_filters_by_minute_of_hour() {
    let pool = fresh_pool().await;
    // Only WORLD has a non-zero minute (08:30).
    let rows = Post::objects()
        .filter(post::CREATED_AT.minute().eq(30))
        .on(&pool)
        .fetch()
        .await
        .expect("filter MINUTE");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "WORLD");
}

#[tokio::test]
async fn second_eq_filters_by_whole_seconds() {
    let pool = fresh_pool().await;
    // Every seed row uses "00" seconds, so the predicate matches all 4.
    let rows = Post::objects()
        .filter(post::CREATED_AT.second().eq(0))
        .on(&pool)
        .fetch()
        .await
        .expect("filter SECOND");
    assert_eq!(rows.len(), 4);
}

#[tokio::test]
async fn week_day_filters_match_calendar_days() {
    let pool = fresh_pool().await;
    // 2024-06-04 is a Tuesday (DOW=2). Both 2026-06-04 (Thursday=4)
    // and 2026-06-20 (Saturday=6) differ. So `.week_day().eq(2)` should
    // match only Hello.
    let rows = Post::objects()
        .filter(post::CREATED_AT.week_day().eq(2))
        .on(&pool)
        .fetch()
        .await
        .expect("filter DOW");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Hello");
}

#[tokio::test]
async fn hour_and_minute_compose() {
    let pool = fresh_pool().await;
    let rows = Post::objects()
        .filter(post::CREATED_AT.hour().eq(15))
        .filter(post::CREATED_AT.minute().eq(0))
        .on(&pool)
        .fetch()
        .await
        .expect("filter HOUR+MINUTE");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Mixed Case");
}
