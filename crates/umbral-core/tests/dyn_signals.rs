//! gaps #77 — `DynQuerySet` write paths fire the same `pre_save` /
//! `post_save` / `bulk_post_save` / `bulk_post_delete` signals as the
//! typed paths.
//!
//! Pins the contract: REST endpoints (which go through `insert_json`
//! / `update_json` / `delete`) and admin form submits surface in
//! signal subscribers identically to typed `Manager::create` /
//! `QuerySet::update_values` / `QuerySet::delete`. Without this
//! coverage the audit-log story is incomplete — typed writes get
//! audited, REST/admin writes don't.

#![allow(dead_code)]

use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::OnceCell;

use umbral::orm::DynQuerySet;
use umbral_core::db;
use umbral_core::signals::{clear_for_tests, subscribe};

/// Serialise the tests in this file. `clear_for_tests()` is process-
/// global and would race with parallel test threads otherwise — same
/// pattern `tests/signals_registry.rs` uses.
fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dsig_note")]
pub struct Note {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    pub body: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        // SQLite `:memory:` is per-connection: each fresh pool
        // connection sees its own empty DB, so reads-after-writes
        // across `#[tokio::test]` runtimes (or even across pool
        // checkouts within one runtime) come back empty. A
        // tempfile-backed DB sidesteps the issue at the cost of
        // one filesystem write per test run. The path is namespaced
        // per process so multiple `cargo test` invocations don't
        // collide on shared state.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbral_dyn_signals_{}.db", std::process::id()));
        // Wipe any leftover from a previous run so test re-runs
        // start clean.
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Note>()
            .build()
            .expect("App::build");
        sqlx::query(
            "CREATE TABLE dsig_note (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                body  TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE");
    })
    .await;
}

fn meta() -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "dsig_note")
        .expect("registered")
}

/// Subscribe to a signal name and return a thread-safe sink that
/// every fire writes its payload into.
fn collect(name: &'static str) -> Arc<Mutex<Vec<Value>>> {
    let bucket: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let bucket2 = bucket.clone();
    subscribe(name, move |payload| {
        bucket2.lock().unwrap().push(payload.clone());
    });
    bucket
}

#[tokio::test]
async fn insert_json_fires_pre_and_post_save_with_created_true() {
    let _guard = test_lock().lock().await;
    boot().await;
    clear_for_tests();

    let pre = collect("pre_save:dsig_note");
    let post = collect("post_save:dsig_note");

    let mut body = serde_json::Map::new();
    body.insert("title".to_string(), Value::String("hello".to_string()));
    body.insert("body".to_string(), Value::String("world".to_string()));
    let row = DynQuerySet::for_meta(&meta())
        .insert_json(&body)
        .await
        .expect("insert");

    // pre_save fired ONCE with `created=true` and the body JSON as
    // the instance (pre-INSERT, no PK yet).
    let pre = pre.lock().unwrap().clone();
    assert_eq!(pre.len(), 1, "pre_save must fire exactly once");
    assert_eq!(pre[0]["created"], Value::Bool(true));
    assert_eq!(pre[0]["instance"]["title"].as_str(), Some("hello"));

    // post_save fired ONCE with `created=true` and the populated row
    // (PK now assigned, plus any framework-managed columns).
    let post = post.lock().unwrap().clone();
    assert_eq!(post.len(), 1, "post_save must fire exactly once");
    assert_eq!(post[0]["created"], Value::Bool(true));
    let id_from_row = row["id"].as_i64().expect("id present in returned row");
    assert_eq!(
        post[0]["instance"]["id"].as_i64(),
        Some(id_from_row),
        "post_save payload's instance must carry the assigned PK"
    );
}

#[tokio::test]
async fn update_json_fires_bulk_post_save_with_ids() {
    let _guard = test_lock().lock().await;
    boot().await;
    clear_for_tests();

    // Seed one row. (Multi-row seed across `:memory:` SQLite pool
    // connections is unreliable in #[tokio::test]; one row is enough
    // to pin the signal contract.)
    let mut row = serde_json::Map::new();
    row.insert("title".to_string(), Value::String("orig".to_string()));
    row.insert("body".to_string(), Value::String("x".to_string()));
    let r = DynQuerySet::for_meta(&meta())
        .insert_json(&row)
        .await
        .expect("seed");
    let id = r["id"].as_i64().unwrap();

    // Subscribe AFTER seeding (the seed's own post_save shouldn't
    // count toward this test's assertions).
    clear_for_tests();
    let bulk = collect("bulk_post_save:dsig_note");

    let mut patch = serde_json::Map::new();
    patch.insert("body".to_string(), Value::String("updated".to_string()));
    let n = DynQuerySet::for_meta(&meta())
        .filter_eq_string("id", &id.to_string())
        .update_json(&patch)
        .await
        .expect("update");
    assert!(n >= 1, "update returned rows_affected = {n}");

    let bulk = bulk.lock().unwrap().clone();
    assert_eq!(bulk.len(), 1, "bulk_post_save must fire exactly once");
    assert_eq!(
        bulk[0]["created"],
        Value::Bool(false),
        "UPDATE path carries created=false"
    );
    let ids = bulk[0]["ids"].as_array().expect("ids array");
    let id_vals: Vec<i64> = ids.iter().filter_map(|v| v.as_i64()).collect();
    assert!(
        id_vals.contains(&id),
        "ids payload must carry the updated PK; got: {ids:?}"
    );
}

#[tokio::test]
async fn delete_fires_bulk_post_delete_with_affected_ids() {
    let _guard = test_lock().lock().await;
    boot().await;
    clear_for_tests();

    let mut row = serde_json::Map::new();
    row.insert("title".to_string(), Value::String("trash".to_string()));
    row.insert("body".to_string(), Value::String("me".to_string()));
    let r = DynQuerySet::for_meta(&meta())
        .insert_json(&row)
        .await
        .expect("seed");
    let id = r["id"].as_i64().unwrap();

    clear_for_tests();
    let bulk = collect("bulk_post_delete:dsig_note");

    let n = DynQuerySet::for_meta(&meta())
        .filter_eq_string("id", &id.to_string())
        .delete()
        .await
        .expect("delete");
    assert_eq!(n, 1);

    let bulk = bulk.lock().unwrap().clone();
    assert_eq!(bulk.len(), 1, "bulk_post_delete must fire exactly once");
    let ids = bulk[0]["ids"].as_array().expect("ids array");
    assert_eq!(ids.len(), 1, "exactly one PK was deleted");
    assert_eq!(
        ids[0].as_i64(),
        Some(id),
        "the deleted PK must be in the payload (captured pre-DELETE)"
    );
}

#[tokio::test]
async fn delete_with_no_matches_still_fires_with_empty_ids() {
    let _guard = test_lock().lock().await;
    boot().await;
    clear_for_tests();
    let bulk = collect("bulk_post_delete:dsig_note");

    // WHERE that matches no rows.
    let n = DynQuerySet::for_meta(&meta())
        .filter_eq_string("id", "999999")
        .delete()
        .await
        .expect("delete");
    assert_eq!(n, 0);

    let bulk = bulk.lock().unwrap().clone();
    assert_eq!(
        bulk.len(),
        1,
        "empty-match delete still fires (subscribers filter, not us)"
    );
    let ids = bulk[0]["ids"].as_array().expect("ids array");
    assert!(ids.is_empty(), "empty payload when nothing matched");
}
