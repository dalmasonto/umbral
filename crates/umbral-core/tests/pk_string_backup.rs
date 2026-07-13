//! PK refactor — dumpdata/loaddata round-trip for a ForeignKey pointing
//! at a String-PK target. The FK column is TEXT in the DB; before the lift
//! the backup reader/binder forced every `SqlType::ForeignKey` through
//! i64, so dumping such a column failed (or mangled the slug). Now the
//! backup resolves the FK to its target's PK type and round-trips it.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::backup::{dump, load};
use umbral::orm::ForeignKey;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "bk_author")]
pub struct Author {
    #[umbral(primary_key)]
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "bk_book")]
pub struct Book {
    pub id: i64,
    pub author: ForeignKey<Author>, // FK to a String-PK target
    pub title: String,
}

static BOOT: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn pool() -> sqlx::SqlitePool {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Book>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        sqlx::query("INSERT INTO bk_author (slug, name) VALUES ('ada', 'Ada')")
            .execute(&pool)
            .await
            .expect("seed author");
        sqlx::query("INSERT INTO bk_book (author, title) VALUES ('ada', 'Rust')")
            .execute(&pool)
            .await
            .expect("seed book");
        pool
    })
    .await
    .clone()
}

#[tokio::test]
async fn string_fk_round_trips_through_dump_and_load() {
    let pool = pool().await;

    // Dump everything (reads the TEXT `author` FK column as a String now).
    let snapshot = dump().await.expect("dump");

    // Wipe both tables, then restore from the snapshot.
    sqlx::query("DELETE FROM bk_book")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM bk_author")
        .execute(&pool)
        .await
        .unwrap();
    load(&snapshot).await.expect("load");

    // The FK survived as the slug, not coerced through i64.
    let book = Book::objects()
        .on(&pool)
        .first()
        .await
        .expect("query")
        .expect("book restored");
    assert_eq!(book.title, "Rust");
    let author_slug: String = book.author.id();
    assert_eq!(author_slug, "ada");

    let author = Author::objects()
        .on(&pool)
        .first()
        .await
        .expect("query")
        .expect("author restored");
    assert_eq!(author.slug, "ada");
    assert_eq!(author.name, "Ada");
}
