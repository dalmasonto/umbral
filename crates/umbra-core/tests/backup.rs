// The local `Comment` model is private but `#[derive(Model)]` emits a
// `pub const` column module that references it. The same file-level allow
// the migrate / type_catalogue tests use.
#![allow(dead_code, private_interfaces)]

//! End-to-end coverage for `umbra::backup`: dump every registered model
//! to a JSON envelope, then load it back into a fresh table and verify
//! the rows survive verbatim.
//!
//! Boot once via OnceCell (the ambient pool, settings, backend, and
//! model registry are all per-process). Seed two distinct tables via
//! raw SQL so the dump has something to walk and so the test doesn't
//! depend on the migrate engine's apply order.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::OnceCell;

use umbra::backup::{Dump, dump, dump_to_path, load, load_from_path};
use umbra_core::orm::Post;

// Two models so the dump walks more than one table. `Comment` lives
// here so the file owns the registry contract; `Post` is the framework
// fixture pulled in via the facade.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Comment {
    id: i64,
    body: String,
    posted_at: Option<DateTime<Utc>>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

/// Serialises tests that mutate the shared `post` / `comment` tables.
///
/// The ambient pool is process-wide (via `App::build`'s OnceLock), so
/// every test in this file points at the same SQLite database. Two of
/// them seed rows and assert counts; one of them runs `DELETE FROM
/// post` mid-test to exercise the load path. Without this mutex, the
/// DELETE races the row-count assertion and one test reports 0 rows.
///
/// Tests that don't touch tables (`load_rejects_unsupported_dump_version`,
/// `load_skips_unknown_tables`) skip the lock and run in parallel.
static TABLES_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");
        let pool = umbra::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");

        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .model::<Comment>()
            .build()
            .expect("App::build should succeed");

        // Create the model tables via raw SQL so the test doesn't
        // race against the migrate seed in other test binaries that
        // share these models. The shapes match what the M5 + M5.1
        // engine would emit (INTEGER PRIMARY KEY AUTOINCREMENT for
        // the `i64` PKs).
        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS post (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL,\
                published_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create post");
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS comment (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                body TEXT NOT NULL,\
                posted_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create comment");
    })
    .await;
}

/// dump walks every registered model in sorted-by-table order and
/// produces a Dump value with one ModelDump entry per model. With two
/// rows seeded across the two tables, the result has both tables and
/// the right row counts.
#[tokio::test]
async fn dump_walks_every_registered_model() {
    boot().await;
    // Lock the shared tables for the duration of this test - the
    // round-trip test wipes them, which races our row-count assertion.
    let _guard = TABLES_LOCK.lock().await;
    let pool = umbra::db::pool();

    // Start clean: another test in this binary may have left rows.
    sqlx::query("DELETE FROM post")
        .execute(&pool)
        .await
        .expect("clean post");
    sqlx::query("DELETE FROM comment")
        .execute(&pool)
        .await
        .expect("clean comment");

    // Seed two rows in each table.
    sqlx::query("INSERT INTO post (title, body, published_at) VALUES (?, ?, ?)")
        .bind("hello")
        .bind("first post body")
        .bind("2026-05-31T12:00:00Z")
        .execute(&pool)
        .await
        .expect("seed post 1");
    sqlx::query("INSERT INTO post (title, body, published_at) VALUES (?, ?, ?)")
        .bind("draft")
        .bind("second post body, unpublished")
        .bind(None::<String>)
        .execute(&pool)
        .await
        .expect("seed post 2");
    sqlx::query("INSERT INTO comment (body, posted_at) VALUES (?, ?)")
        .bind("nice post")
        .bind("2026-05-31T12:30:00Z")
        .execute(&pool)
        .await
        .expect("seed comment");

    let d: Dump = dump().await.expect("dump should succeed");

    assert_eq!(d.umbra_dump_version, "1");
    assert!(!d.exported_at.is_empty());

    let tables: Vec<&str> = d.models.iter().map(|m| m.table.as_str()).collect();
    assert!(
        tables.contains(&"post"),
        "dump should include `post`; got {tables:?}"
    );
    assert!(
        tables.contains(&"comment"),
        "dump should include `comment`; got {tables:?}"
    );

    let post = d.models.iter().find(|m| m.table == "post").unwrap();
    assert!(
        post.rows.len() >= 2,
        "expected at least 2 post rows; got {}",
        post.rows.len()
    );
    // The nullable column round-trips both shapes: a value and a null.
    let has_published: usize = post
        .rows
        .iter()
        .filter(|r| r.get("published_at").is_some_and(|v| !v.is_null()))
        .count();
    let has_null: usize = post
        .rows
        .iter()
        .filter(|r| r.get("published_at").is_some_and(|v| v.is_null()))
        .count();
    assert!(has_published >= 1 && has_null >= 1);
}

/// dump_to_path + load_from_path round-trip: write the JSON envelope
/// to disk, drop the rows from the live tables, load the envelope, and
/// verify the rows came back.
#[tokio::test]
async fn round_trip_through_disk_preserves_rows() {
    boot().await;
    // The round-trip wipes both tables mid-test; serialise against
    // `dump_walks_every_registered_model` which seeds rows and asserts
    // counts.
    let _guard = TABLES_LOCK.lock().await;
    let pool = umbra::db::pool();

    // Start clean.
    sqlx::query("DELETE FROM post")
        .execute(&pool)
        .await
        .expect("clean post");
    sqlx::query("DELETE FROM comment")
        .execute(&pool)
        .await
        .expect("clean comment");

    // Seed a deterministic comment row we'll look for after the
    // round-trip.
    sqlx::query("INSERT INTO comment (body, posted_at) VALUES (?, ?)")
        .bind("survives the round trip")
        .bind("2026-05-31T13:00:00Z")
        .execute(&pool)
        .await
        .expect("seed");

    let tmp: TempDir = tempfile::tempdir().expect("tempdir");
    let path: PathBuf = tmp.path().join("dump.json");

    dump_to_path(&path).await.expect("dump_to_path");
    assert!(path.exists(), "dump file should exist at {path:?}");

    // Wipe both tables before load so the round-trip is the only
    // path that puts rows back. The dump captured both tables; the
    // load will re-insert their original rows, including the same
    // primary-key values, which is why the wipe has to be total.
    sqlx::query("DELETE FROM comment")
        .execute(&pool)
        .await
        .expect("wipe comment");
    sqlx::query("DELETE FROM post")
        .execute(&pool)
        .await
        .expect("wipe post");
    let count_after_wipe: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM comment")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count_after_wipe.0, 0);

    let report = load_from_path(&path).await.expect("load_from_path");
    assert!(
        report.rows_loaded >= 1,
        "load should report rows; got {}",
        report.rows_loaded
    );
    assert!(
        report.tables_loaded.contains(&"comment".to_string()),
        "report should list `comment`; got {:?}",
        report.tables_loaded
    );

    let count_after_load: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM comment")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert!(
        count_after_load.0 >= 1,
        "load should have written rows back; got {}",
        count_after_load.0
    );

    let survived: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT body, posted_at FROM comment WHERE body = 'survives the round trip'",
    )
    .fetch_optional(&pool)
    .await
    .expect("select survivor");
    let (body, posted_at) = survived.expect("the seeded comment should round-trip");
    assert_eq!(body, "survives the round trip");
    assert_eq!(
        posted_at.as_deref(),
        Some("2026-05-31T13:00:00+00:00"),
        "RFC-3339 timestamp survives the round trip (timezone normalised)"
    );
}

/// A dump with an `umbra_dump_version` other than "1" surfaces as an
/// `UnsupportedVersion` error instead of being silently accepted.
#[tokio::test]
async fn load_rejects_unsupported_dump_version() {
    boot().await;

    let bad = Dump {
        umbra_dump_version: "99".to_string(),
        exported_at: "2026-05-31T00:00:00Z".to_string(),
        models: Vec::new(),
    };
    let err = load(&bad).await.expect_err("load should reject version 99");
    let msg = err.to_string();
    assert!(
        msg.contains("99") && msg.contains("not supported"),
        "diagnostic should mention the offending version and the unsupported case; got {msg}"
    );
}

/// A dump that includes a table not in the current registry skips it
/// with a warning (the `skipped_tables` field), rather than erroring.
/// Lets a dump from a richer schema still feed the tables this build
/// knows about.
#[tokio::test]
async fn load_skips_unknown_tables() {
    boot().await;

    let dump = Dump {
        umbra_dump_version: "1".to_string(),
        exported_at: "2026-05-31T00:00:00Z".to_string(),
        models: vec![umbra::backup::ModelDump {
            table: "table_that_does_not_exist".to_string(),
            rows: vec![],
        }],
    };
    let report = load(&dump)
        .await
        .expect("load should not fail on unknown table");
    assert_eq!(report.tables_loaded.len(), 0);
    assert!(
        report
            .skipped_tables
            .contains(&"table_that_does_not_exist".to_string()),
        "the unknown table should appear in skipped_tables; got {:?}",
        report.skipped_tables
    );
}

/// Helper: keep an unused SqlitePool import path stable; the boot()
/// helper uses it indirectly through `umbra::db::pool()`.
#[allow(dead_code)]
fn _unused_pool_marker(_: SqlitePool) {}
