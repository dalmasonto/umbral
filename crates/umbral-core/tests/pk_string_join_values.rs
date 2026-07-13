//! Review #1 — `join_related` / `values()` must decode the related PK by
//! its `SqlType`, not `i64`. A `ForeignKey` to a String-PK target produces
//! a real joined row whose PK is a string; the old `Option<i64>` presence
//! check failed to decode it and treated the row as a left-join MISS,
//! dropping the resolved FK / nesting `null`.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::ForeignKey;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "jsv_author")]
pub struct Author {
    #[umbral(primary_key)]
    pub handle: String,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "jsv_post")]
pub struct Post {
    pub id: i64,
    pub author: ForeignKey<Author>, // FK to a String-PK target
    pub title: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Post>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        sqlx::query("INSERT INTO jsv_author (handle, name) VALUES ('ada', 'Ada')")
            .execute(&pool)
            .await
            .expect("seed author");
        sqlx::query("INSERT INTO jsv_post (author, title) VALUES ('ada', 'Hello')")
            .execute(&pool)
            .await
            .expect("seed post");
    })
    .await;
}

#[tokio::test]
async fn values_nests_string_pk_fk_instead_of_null() {
    boot().await;
    let rows = Post::objects()
        .values(&["title", "author__name"])
        .await
        .expect("values");
    assert_eq!(rows.len(), 1);
    let row = rows[0].as_object().expect("object row");
    assert_eq!(row.get("title").and_then(|v| v.as_str()), Some("Hello"));
    // Before the fix this nested object was `null` (the String PK failed
    // the Option<i64> presence check → treated as a left-join miss).
    let author = row
        .get("author")
        .and_then(|v| v.as_object())
        .expect("nested author object (not null) for a String-PK FK");
    assert_eq!(author.get("name").and_then(|v| v.as_str()), Some("Ada"));
}

#[tokio::test]
async fn join_related_resolves_string_pk_fk() {
    boot().await;
    let posts = Post::objects()
        .left_join_related("author")
        .fetch()
        .await
        .expect("join_related fetch");
    assert_eq!(posts.len(), 1);
    let author = posts[0]
        .author
        .resolved()
        .expect("join_related resolved the String-PK FK (not a left-join miss)");
    assert_eq!(author.name, "Ada");
    assert_eq!(author.handle, "ada");
}
