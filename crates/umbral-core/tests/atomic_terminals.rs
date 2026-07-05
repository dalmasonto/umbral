//! Gap 38.1 — `.atomic()` / `.non_atomic()` and `App::builder().atomic_transactions(true)`.
//!
//! The opt-in surface that wraps ORM write terminals in a transaction
//! without forcing the caller to thread a `sqlx::Transaction` through
//! the QuerySet API. Two layers:
//!
//! 1. **Per-call**: `Post::objects().atomic().create(post)` runs the
//!    write inside a BEGIN / COMMIT pair (rolled back on Err).
//! 2. **Builder default**: `App::builder().atomic_transactions(true)`
//!    flips the global default. Every write inherits it unless
//!    overridden with `.non_atomic()`.
//!
//! These tests pin behaviour rather than transaction-isolation
//! semantics — a single-statement INSERT is already DB-atomic, so the
//! observable side-effect is "still produces correct results when the
//! atomic flag is on" and "rolls back on Err".

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbral::orm::write::WriteError;

static SERIALISE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "atomic_post")]
pub struct Post {
    pub id: i64,
    #[umbral(unique)]
    pub slug: String,
    pub title: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("atomic_terminals.sqlite");
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
        // Flip the global default ON for this test binary so the
        // `non_atomic()` opt-out path is exercisable. Per-test
        // `.atomic()` / `.non_atomic()` overrides still win — the
        // builder default is the fall-through when neither is set.
        let _ = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .atomic_transactions(true)
            .model::<Post>()
            .build()
            .expect("App::build");
        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE atomic_post (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                slug TEXT NOT NULL UNIQUE,\
                title TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE");
    })
    .await;
}

async fn truncate() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM atomic_post")
        .execute(&pool)
        .await
        .expect("truncate");
}

#[tokio::test]
async fn atomic_create_commits_on_ok() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let row = Post::objects()
        .atomic()
        .create(Post {
            id: 0,
            slug: "atomic-1".into(),
            title: "t1".into(),
        })
        .await
        .expect("atomic create");
    assert!(row.id > 0);

    let count = Post::objects().count().await.expect("count");
    assert_eq!(count, 1, "committed row visible to follow-up SELECT");
}

#[tokio::test]
async fn atomic_create_rolls_back_on_unique_violation() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    Post::objects()
        .create(Post {
            id: 0,
            slug: "dup".into(),
            title: "first".into(),
        })
        .await
        .expect("seed");
    let err = Post::objects()
        .atomic()
        .create(Post {
            id: 0,
            slug: "dup".into(),
            title: "second".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            WriteError::UniqueViolation { .. } | WriteError::Sqlx(_)
        ),
        "unique conflict surfaced; got: {err:?}"
    );
    let count = Post::objects().count().await.expect("count");
    assert_eq!(count, 1, "second insert rolled back");
}

#[tokio::test]
async fn atomic_bulk_create_produces_correct_ids() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let n = Post::objects()
        .atomic()
        .bulk_create(vec![
            Post {
                id: 0,
                slug: "b1".into(),
                title: "t".into(),
            },
            Post {
                id: 0,
                slug: "b2".into(),
                title: "t".into(),
            },
            Post {
                id: 0,
                slug: "b3".into(),
                title: "t".into(),
            },
        ])
        .await
        .expect("atomic bulk_create");
    assert_eq!(n, 3);
    let count = Post::objects().count().await.expect("count");
    assert_eq!(count, 3);
}

#[tokio::test]
async fn non_atomic_overrides_global_default() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    // The test binary's App was built with atomic_transactions(true).
    // `.non_atomic()` explicitly opts THIS call out — the operation
    // still has to succeed and produce the same result; we're pinning
    // that the override is honoured, not the transaction wire-format.
    let row = Post::objects()
        .non_atomic()
        .create(Post {
            id: 0,
            slug: "non-atomic-1".into(),
            title: "t".into(),
        })
        .await
        .expect("non-atomic create");
    assert!(row.id > 0);
}

#[tokio::test]
async fn implicit_atomic_via_global_default() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    // No `.atomic()` on the chain — the App's
    // `.atomic_transactions(true)` should kick in. Result: the row
    // exists and the operation succeeded; the wire-level transaction
    // is invisible to the caller.
    let row = Post::objects()
        .create(Post {
            id: 0,
            slug: "default-atomic".into(),
            title: "t".into(),
        })
        .await
        .expect("create under global atomic default");
    assert!(row.id > 0);
}

#[tokio::test]
async fn atomic_update_values_succeeds() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let row = Post::objects()
        .create(Post {
            id: 0,
            slug: "u1".into(),
            title: "old".into(),
        })
        .await
        .expect("seed");
    let mut update = serde_json::Map::new();
    update.insert("title".into(), serde_json::json!("new"));
    let n = Post::objects()
        .filter(post::ID.eq(row.id))
        .atomic()
        .update_values(update)
        .await
        .expect("atomic update_values");
    assert_eq!(n, 1);
    let fresh = Post::objects()
        .get(post::ID.eq(row.id))
        .await
        .expect("re-fetch");
    assert_eq!(fresh.title, "new");
}

#[tokio::test]
async fn atomic_delete_succeeds() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let row = Post::objects()
        .create(Post {
            id: 0,
            slug: "d1".into(),
            title: "doomed".into(),
        })
        .await
        .expect("seed");
    let n = Post::objects()
        .filter(post::ID.eq(row.id))
        .atomic()
        .delete()
        .await
        .expect("atomic delete");
    assert_eq!(n, 1);
}
