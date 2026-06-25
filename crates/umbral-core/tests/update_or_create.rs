//! Gap 21 — `Manager::update_or_create(predicate, defaults)`.
//!
//! Returns `(row, created)`. On match: update the matched row with the
//! defaults' non-PK fields, return the fresh row. On miss: insert the
//! defaults, return it. The `update_or_create` terminal.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Barrier, Mutex, OnceCell};
use umbral::orm::Predicate;

static SERIALISE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "uoc_post")]
pub struct Post {
    pub id: i64,
    #[umbral(unique)]
    pub slug: String,
    pub title: String,
    pub views: i64,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("update_or_create.sqlite");
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
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .build()
            .expect("App::build");
        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE uoc_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slug TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                views INTEGER NOT NULL DEFAULT 0
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
    sqlx::query("DELETE FROM uoc_post")
        .execute(&pool)
        .await
        .expect("truncate");
}

#[tokio::test]
async fn update_or_create_inserts_when_missing() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let (row, created) = Post::objects()
        .update_or_create(
            Predicate::col_eq("slug", "first"),
            Post {
                id: 0,
                slug: "first".into(),
                title: "First Post".into(),
                views: 0,
            },
        )
        .await
        .expect("uoc insert");
    assert!(created);
    assert!(row.id > 0);
    assert_eq!(row.slug, "first");
    assert_eq!(row.title, "First Post");

    let count = Post::objects().count().await.expect("count");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn update_or_create_updates_when_found() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let seed = Post::objects()
        .create(Post {
            id: 0,
            slug: "second".into(),
            title: "Old Title".into(),
            views: 5,
        })
        .await
        .expect("seed");

    let (row, created) = Post::objects()
        .update_or_create(
            Predicate::col_eq("slug", "second"),
            Post {
                id: 0,
                slug: "second".into(),
                title: "New Title".into(),
                views: 99,
            },
        )
        .await
        .expect("uoc update");
    assert!(!created, "should NOT be created — row existed");
    assert_eq!(row.id, seed.id, "same PK as the seeded row");
    assert_eq!(row.title, "New Title");
    assert_eq!(row.views, 99);

    let count = Post::objects().count().await.expect("count");
    assert_eq!(count, 1, "still one row, just updated");
}

#[tokio::test]
async fn update_or_create_does_not_change_pk() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let seed = Post::objects()
        .create(Post {
            id: 0,
            slug: "third".into(),
            title: "Original".into(),
            views: 0,
        })
        .await
        .expect("seed");

    let (row, _created) = Post::objects()
        .update_or_create(
            Predicate::col_eq("slug", "third"),
            // Defaults set id=999 — should be IGNORED, the matched
            // row's PK stays put.
            Post {
                id: 999,
                slug: "third".into(),
                title: "Updated".into(),
                views: 1,
            },
        )
        .await
        .expect("uoc");
    assert_eq!(row.id, seed.id, "PK in defaults must be ignored");
    assert_eq!(row.title, "Updated");
}

/// Convergence test: two concurrent `update_or_create` callers race on the same
/// slug. Without the UniqueViolation-convergence fix the loser would surface a
/// `UniqueViolation` error. With the fix both callers converge: one creates,
/// the other re-fetches and updates; exactly one row exists and no error is
/// returned to either caller.
///
/// Approach: a `Barrier` synchronises both tasks so they complete their initial
/// SELECT before either proceeds to the INSERT, maximising the window for the
/// UniqueViolation→re-fetch→update path in `update_or_create`.
///
/// We still hold `SERIALISE` so other tests in this binary cannot truncate
/// the table while the two internal tasks are racing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_or_create_converges_under_concurrent_insert() {
    boot().await;
    let _g = SERIALISE.lock().await;
    truncate().await;

    let barrier = std::sync::Arc::new(Barrier::new(2));

    let b1 = barrier.clone();
    let t1 = tokio::spawn(async move {
        b1.wait().await;
        Post::objects()
            .update_or_create(
                Predicate::col_eq("slug", "race-uoc"),
                Post {
                    id: 0,
                    slug: "race-uoc".into(),
                    title: "Task 1 Title".into(),
                    views: 10,
                },
            )
            .await
    });

    let b2 = barrier.clone();
    let t2 = tokio::spawn(async move {
        b2.wait().await;
        Post::objects()
            .update_or_create(
                Predicate::col_eq("slug", "race-uoc"),
                Post {
                    id: 0,
                    slug: "race-uoc".into(),
                    title: "Task 2 Title".into(),
                    views: 20,
                },
            )
            .await
    });

    let r1 = t1.await.expect("task1 panicked").expect("task1 update_or_create");
    let r2 = t2.await.expect("task2 panicked").expect("task2 update_or_create");

    let (p1, c1) = r1;
    let (p2, c2) = r2;

    // Exactly one caller created the row; the other found/updated it.
    assert_eq!(
        c1 as u8 + c2 as u8,
        1,
        "exactly one task should have created=true"
    );
    // Both callers got back a row with the correct slug.
    assert_eq!(p1.slug, "race-uoc");
    assert_eq!(p2.slug, "race-uoc");
    // Both see the same PK.
    assert_eq!(p1.id, p2.id, "both callers must converge on the same row");

    // Only one row was inserted.
    let count = Post::objects().count().await.expect("count");
    assert_eq!(count, 1, "only one row must exist after concurrent update_or_create");
}
