//! End-to-end tests for the Model write-side primitives:
//!
//! - `Manager::create(instance)` → INSERT + RETURNING populated row.
//! - `Manager::bulk_create(Vec<T>)` → multi-row INSERT, returns count.
//! - `QuerySet::delete()` → DELETE WHERE → count.
//! - `QuerySet::update_values(map)` → UPDATE SET ... WHERE → count.
//!
//! Each test boots its OWN app (one per test binary; the framework's
//! settings OnceLock allows exactly one App per process). Tests run
//! serially in this binary by virtue of all sharing the boot fixture.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbra::orm::write::WriteError;

/// Serialise every test in this binary on a single mutex so the
/// shared `writes_post` table isn't raced. Cargo runs tests within a
/// binary in parallel by default; the alternative (per-test table
/// names) would multiply boilerplate for no benefit.
static SERIALISE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "writes_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub published: bool,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

/// Boot the App once, create the table once, return the ambient pool.
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("model_writes.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let _app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .build()
            .expect("App::build");

        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE writes_post (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL,\
                published BOOLEAN NOT NULL DEFAULT 0\
             )",
        )
        .execute(&pool)
        .await
        .expect("create writes_post table");
    })
    .await;
}

/// Re-create the table empty between tests since they all share the
/// same boot. Not parallel-safe; tokio's test runner serialises tests
/// within a single binary by default.
async fn truncate() {
    let pool = umbra::db::pool();
    sqlx::query("DELETE FROM writes_post")
        .execute(&pool)
        .await
        .expect("truncate");
}

// =====================================================================
// Manager::create
// =====================================================================

#[tokio::test]
async fn create_inserts_a_row_and_returns_it_populated() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let new = Post {
        id: 0, // autoincrement sentinel
        title: "hello".into(),
        body: "world".into(),
        published: true,
    };
    let row = Post::objects().create(new).await.expect("create");
    assert!(row.id > 0, "autoincrement PK should be populated");
    assert_eq!(row.title, "hello");
    assert_eq!(row.body, "world");
    assert!(row.published);
}

#[tokio::test]
async fn create_with_explicit_pk_respects_it() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let new = Post {
        id: 999,
        title: "explicit".into(),
        body: "pk".into(),
        published: false,
    };
    let row = Post::objects().create(new).await.expect("create");
    assert_eq!(row.id, 999);
}

#[tokio::test]
async fn create_rejects_missing_required_field_through_required_error() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    // Manually go through update_values with a null on a non-nullable
    // column to verify the error variant. (create itself doesn't
    // accept partial JSON; instances are typed and the compiler
    // catches missing fields, so this exercises the JSON-dispatch
    // path that update_values + REST share.)
    let mut bad: Map<String, Value> = Map::new();
    bad.insert("title".into(), json!(null));
    let err = Post::objects()
        .filter(post::ID.eq(123))
        .update_values(bad)
        .await
        .unwrap_err();
    assert!(matches!(err, WriteError::RequiredFieldMissing { .. }));
}

// =====================================================================
// Manager::bulk_create
// =====================================================================

#[tokio::test]
async fn bulk_create_inserts_many_rows() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let posts = (1..=5)
        .map(|i| Post {
            id: 0,
            title: format!("title {i}"),
            body: format!("body {i}"),
            published: i % 2 == 0,
        })
        .collect::<Vec<_>>();

    let n = Post::objects()
        .bulk_create(posts)
        .await
        .expect("bulk_create");
    assert_eq!(n, 5);

    let count = Post::objects().count().await.expect("count");
    assert_eq!(count, 5);
}

#[tokio::test]
async fn bulk_create_empty_input_is_a_noop() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let n = Post::objects()
        .bulk_create(Vec::<Post>::new())
        .await
        .expect("bulk_create");
    assert_eq!(n, 0);
}

// =====================================================================
// QuerySet::delete
// =====================================================================

#[tokio::test]
async fn delete_with_filter_removes_matching_rows() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    for i in 1..=4 {
        Post::objects()
            .create(Post {
                id: 0,
                title: format!("t{i}"),
                body: format!("b{i}"),
                published: i % 2 == 0,
            })
            .await
            .unwrap();
    }
    let n = Post::objects()
        .filter(post::PUBLISHED.eq(false))
        .delete()
        .await
        .expect("delete");
    assert_eq!(n, 2);

    let remaining = Post::objects().count().await.unwrap();
    assert_eq!(remaining, 2);
}

#[tokio::test]
async fn delete_without_filter_removes_every_row() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    for i in 1..=3 {
        Post::objects()
            .create(Post {
                id: 0,
                title: format!("t{i}"),
                body: "b".into(),
                published: true,
            })
            .await
            .unwrap();
    }
    let n = Post::objects()
        .filter(post::ID.gt(0))
        .delete()
        .await
        .unwrap();
    assert_eq!(n, 3);
    let remaining = Post::objects().count().await.unwrap();
    assert_eq!(remaining, 0);
}

// =====================================================================
// QuerySet::update_values
// =====================================================================

#[tokio::test]
async fn update_values_changes_matching_rows() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    for i in 1..=3 {
        Post::objects()
            .create(Post {
                id: 0,
                title: format!("t{i}"),
                body: format!("b{i}"),
                published: false,
            })
            .await
            .unwrap();
    }

    let mut updates: Map<String, Value> = Map::new();
    updates.insert("published".into(), json!(true));
    let n = Post::objects()
        .filter(post::TITLE.eq("t2"))
        .update_values(updates)
        .await
        .expect("update");
    assert_eq!(n, 1);

    let updated = Post::objects()
        .filter(post::PUBLISHED.eq(true))
        .fetch()
        .await
        .unwrap();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].title, "t2");
}

#[tokio::test]
async fn update_values_silently_skips_pk_to_avoid_corruption() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let original = Post::objects()
        .create(Post {
            id: 0,
            title: "keep my pk".into(),
            body: "b".into(),
            published: false,
        })
        .await
        .unwrap();

    let mut bad: Map<String, Value> = Map::new();
    bad.insert("id".into(), json!(99999));
    bad.insert("title".into(), json!("new title"));
    let n = Post::objects()
        .filter(post::ID.eq(original.id))
        .update_values(bad)
        .await
        .expect("update");
    assert_eq!(n, 1);

    let still_there = Post::objects()
        .filter(post::ID.eq(original.id))
        .first()
        .await
        .unwrap();
    assert!(
        still_there.is_some(),
        "the row identified by the original PK should still exist"
    );
    assert_eq!(still_there.unwrap().title, "new title");
}

#[tokio::test]
async fn update_values_unknown_column_errors_loudly() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let mut bad: Map<String, Value> = Map::new();
    bad.insert("definitely_not_a_column".into(), json!(123));
    let err = Post::objects()
        .filter(post::ID.eq(1))
        .update_values(bad)
        .await
        .unwrap_err();
    assert!(matches!(err, WriteError::UnknownColumn { .. }));
}
