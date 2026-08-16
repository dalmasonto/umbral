//! Gap 109 follow-up: `#[umbral(slug_from = "...")]` auto-derivation must
//! fire on the TYPED write path (`objects().create()` / `bulk_create()`),
//! not only on the dynamic REST/admin path. Otherwise the same model
//! persists a derived slug via REST but an empty slug via `create()` — a
//! "depends who wrote the row" inconsistency (cf. features #83, which
//! closed the same split for `trim`/`lowercase`).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell};

static SERIALISE: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "slug_typed_article")]
pub struct Article {
    pub id: i64,
    pub title: String,
    #[umbral(slug_from = "title")]
    pub slug: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Article>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create schema");
    })
    .await;
}

async fn truncate() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM slug_typed_article")
        .execute(&pool)
        .await
        .expect("truncate");
}

/// `create()` with an empty slug derives it from the source column — the
/// same behavior a REST POST already gives.
#[tokio::test]
async fn create_derives_slug_from_source_when_empty() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let row = Article::objects()
        .create(Article {
            id: 0,
            title: "Hello World".into(),
            slug: String::new(),
        })
        .await
        .expect("create");

    assert_eq!(
        row.slug, "hello-world",
        "typed create should auto-derive the slug from `title`"
    );
}

/// An explicitly-supplied, non-empty slug is preserved — auto-derivation
/// never clobbers a caller-chosen slug.
#[tokio::test]
async fn create_keeps_explicit_slug() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let row = Article::objects()
        .create(Article {
            id: 0,
            title: "Hello World".into(),
            slug: "my-custom-slug".into(),
        })
        .await
        .expect("create");

    assert_eq!(row.slug, "my-custom-slug", "explicit slug must survive");
}

/// `bulk_create()` derives per-row too.
#[tokio::test]
async fn bulk_create_derives_slug_per_row() {
    let _g = SERIALISE.lock().await;
    boot().await;
    truncate().await;

    let n = Article::objects()
        .bulk_create(vec![
            Article {
                id: 0,
                title: "Foo Bar".into(),
                slug: String::new(), // → derived "foo-bar"
            },
            Article {
                id: 0,
                title: "Baz Qux".into(),
                slug: "kept".into(), // → explicit, preserved
            },
        ])
        .await
        .expect("bulk_create");
    assert_eq!(n, 2);

    // Ordered by title asc: "Baz Qux" then "Foo Bar".
    let rows = Article::objects()
        .order_by(article::TITLE.asc())
        .fetch()
        .await
        .expect("fetch");
    assert_eq!(rows[0].slug, "kept", "explicit slug preserved in bulk");
    assert_eq!(rows[1].slug, "foo-bar", "empty slug derived in bulk");
}
