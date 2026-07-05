//! Gap 38 — bulk-write signals (`bulk_post_save:<table>` /
//! `bulk_post_delete:<table>`).
//!
//! Each bulk terminal — `Manager::bulk_create`,
//! `QuerySet::update_values`, `QuerySet::update_expr`,
//! `QuerySet::delete` — fires exactly one signal per call with the list
//! of affected primary keys in the payload. Auditing subscribers learn
//! "INSERT touched ids [1, 2, 3]" without paying for per-row signal
//! fanout.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex as TokioMutex, OnceCell};

use umbral_core::signals::{clear_for_tests, subscribe};

/// Process-wide serialiser so the shared `bulkpost` table isn't raced.
static SERIALISE: TokioMutex<()> = TokioMutex::const_new(());

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "bulkpost")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub published: bool,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("bulk_signals.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        let _ = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .build()
            .expect("App::build");
        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE bulkpost (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                published BOOLEAN NOT NULL DEFAULT 0\
             )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE bulkpost");
    })
    .await;
}

async fn truncate() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM bulkpost")
        .execute(&pool)
        .await
        .expect("truncate");
}

/// Subscribe to a signal and capture its latest payload.
fn capture(signal: &str) -> Arc<Mutex<Vec<Value>>> {
    let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let c = captured.clone();
    subscribe(signal, move |p| c.lock().unwrap().push(p.clone()));
    captured
}

#[tokio::test]
async fn bulk_create_emits_bulk_post_save_with_inserted_ids() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    let captured = capture("bulk_post_save:bulkpost");

    let posts: Vec<Post> = (1..=3)
        .map(|i| Post {
            id: 0,
            title: format!("p{i}"),
            published: false,
        })
        .collect();
    let n = Post::objects()
        .bulk_create(posts)
        .await
        .expect("bulk_create");
    assert_eq!(n, 3);

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1, "exactly one bulk_post_save event");
    let payload = &events[0];
    assert_eq!(payload["created"], json!(true));
    let ids = payload["ids"].as_array().expect("ids array");
    assert_eq!(ids.len(), 3, "all inserted ids captured");
    for v in ids {
        assert!(v.is_number(), "PK should be a number; got {v}");
    }
    assert!(payload.get("actor").is_some(), "actor key present");
}

#[tokio::test]
async fn bulk_create_empty_input_does_not_emit() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    let captured = capture("bulk_post_save:bulkpost");
    let n = Post::objects()
        .bulk_create(Vec::<Post>::new())
        .await
        .expect("bulk_create");
    assert_eq!(n, 0);
    assert!(
        captured.lock().unwrap().is_empty(),
        "no event when 0 rows touched"
    );
}

#[tokio::test]
async fn update_values_emits_bulk_post_save_with_matched_ids() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    // Seed three rows.
    let mut want_ids: Vec<i64> = Vec::new();
    for i in 1..=3 {
        let row = Post::objects()
            .create(Post {
                id: 0,
                title: format!("u{i}"),
                published: false,
            })
            .await
            .expect("seed create");
        want_ids.push(row.id);
    }

    let captured = capture("bulk_post_save:bulkpost");
    let mut update: Map<String, Value> = Map::new();
    update.insert("published".into(), json!(true));
    let n = Post::objects()
        .filter(post::ID.gt(0))
        .update_values(update)
        .await
        .expect("update_values");
    assert_eq!(n, 3);

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1);
    let payload = &events[0];
    assert_eq!(payload["created"], json!(false));
    let mut got_ids: Vec<i64> = payload["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    got_ids.sort();
    let mut want = want_ids.clone();
    want.sort();
    assert_eq!(got_ids, want);
}

#[tokio::test]
async fn update_values_matching_zero_rows_does_not_emit() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    let captured = capture("bulk_post_save:bulkpost");
    let mut update: Map<String, Value> = Map::new();
    update.insert("published".into(), json!(true));
    let n = Post::objects()
        .filter(post::ID.eq(9999))
        .update_values(update)
        .await
        .expect("update_values");
    assert_eq!(n, 0);
    assert!(
        captured.lock().unwrap().is_empty(),
        "no event when zero rows matched"
    );
}

#[tokio::test]
async fn delete_emits_bulk_post_delete_with_removed_ids() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    let mut want_ids: Vec<i64> = Vec::new();
    for i in 1..=2 {
        let row = Post::objects()
            .create(Post {
                id: 0,
                title: format!("d{i}"),
                published: false,
            })
            .await
            .expect("seed");
        want_ids.push(row.id);
    }

    let captured = capture("bulk_post_delete:bulkpost");
    let n = Post::objects()
        .filter(post::ID.gt(0))
        .delete()
        .await
        .expect("delete");
    assert_eq!(n, 2);

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1);
    let payload = &events[0];
    let mut got_ids: Vec<i64> = payload["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    got_ids.sort();
    let mut want = want_ids.clone();
    want.sort();
    assert_eq!(got_ids, want);
}

#[tokio::test]
async fn delete_matching_zero_rows_does_not_emit() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    let captured = capture("bulk_post_delete:bulkpost");
    let n = Post::objects()
        .filter(post::ID.eq(9999))
        .delete()
        .await
        .expect("delete");
    assert_eq!(n, 0);
    assert!(captured.lock().unwrap().is_empty());
}
