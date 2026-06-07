//! Feature #74 — fixture load/dump round-trip tests.

#![allow(dead_code, private_interfaces)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::fixtures::{FixtureError, dump_fixture, load_fixture};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "fx_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: Option<String>,
    pub created_at: DateTime<Utc>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        // Use a per-process tempfile rather than `:memory:` so every
        // pool connection sees the same schema. The default sqlx
        // SqlitePool keeps up to 10 connections; each one against
        // `:memory:` would be a fresh empty DB, so CREATE TABLE on
        // one connection isn't visible to load_fixture's later
        // INSERT on another.
        let dir = tempfile::tempdir().expect("tempdir");
        // Leak the tempdir so its path stays valid for the lifetime
        // of the test process — OnceCell only runs this block once.
        let db_path = dir.path().join("fixtures.db");
        Box::leak(Box::new(dir));
        let url = format!("sqlite://{}?mode=rwc", db_path.display());
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = umbra::db::connect_sqlite(&url).await.expect("sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Post>()
            .build()
            .expect("App::build");
        sqlx::query("CREATE TABLE fx_post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, body TEXT, created_at TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("create fx_post");
    })
    .await;
}

#[tokio::test]
async fn load_then_dump_round_trips_via_temp_file() {
    boot().await;
    // Hand-write a fixture file as if a test author put it in
    // tests/fixtures/posts.json.
    let dir = tempfile::tempdir().expect("tempdir");
    let in_path = dir.path().join("posts.json");
    // Unique titles per-test so the parallel test runner doesn't
    // make the row-counts shift under our feet — we'll grep the
    // dump for these specific titles rather than asserting on a
    // total count.
    let payload = serde_json::json!([
        { "id": 1001, "title": "rt-Hello",   "body": "world", "created_at": "2026-01-01T00:00:00Z" },
        { "id": 1002, "title": "rt-Fixtures", "body": null,   "created_at": "2026-02-01T00:00:00Z" }
    ]);
    std::fs::write(&in_path, serde_json::to_string_pretty(&payload).unwrap())
        .expect("write fixture");

    // Load the file — load_fixture returns the count it processed.
    let inserted = load_fixture::<Post, _>(&in_path).await.expect("load");
    assert_eq!(inserted, 2);

    // Round-trip: dump the table back out. The total can include
    // rows another parallel test inserted, so we only assert that
    // our two specific titles round-trip through the JSON.
    let out_path = dir.path().join("posts_dump.json");
    dump_fixture::<Post, _>(&out_path).await.expect("dump");
    let bytes = std::fs::read(&out_path).expect("read dump");
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("parse dump");
    let titles: Vec<&str> = arr
        .iter()
        .filter_map(|r| r.get("title").and_then(|v| v.as_str()))
        .collect();
    assert!(titles.contains(&"rt-Hello"));
    assert!(titles.contains(&"rt-Fixtures"));
}

#[tokio::test]
async fn load_rejects_non_array_payload() {
    boot().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("not_array.json");
    std::fs::write(&bad, r#"{"id": 1, "title": "oops"}"#).expect("write");
    let err = load_fixture::<Post, _>(&bad)
        .await
        .expect_err("must reject object-at-top-level");
    matches!(err, FixtureError::NotAnArray { .. });
}

#[tokio::test]
async fn manager_shim_works_as_method_call() {
    boot().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let in_path = dir.path().join("shim.json");
    std::fs::write(
        &in_path,
        r#"[{"id": 100, "title": "shim", "body": null, "created_at": "2026-03-01T00:00:00Z"}]"#,
    )
    .expect("write");
    let n = Post::objects()
        .load_fixture(&in_path)
        .await
        .expect("method shim");
    assert_eq!(n, 1);
}
