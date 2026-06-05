//! Gap 21 — `Manager::update_or_create(predicate, defaults)`.
//!
//! Returns `(row, created)`. On match: update the matched row with the
//! defaults' non-PK fields, return the fresh row. On miss: insert the
//! defaults, return it. Mirrors Django's `update_or_create`.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use umbra::orm::Predicate;

static SERIALISE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "uoc_post")]
pub struct Post {
    pub id: i64,
    #[umbra(unique)]
    pub slug: String,
    pub title: String,
    pub views: i64,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
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
        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .build()
            .expect("App::build");
        let pool = umbra::db::pool();
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
    let pool = umbra::db::pool();
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
