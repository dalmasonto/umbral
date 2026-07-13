//! Review #2 — `first()` must hydrate the SAME relation directives as
//! `fetch()`: select_related, prefetch_related, AND join_related. It used
//! to handle only select_related, so `.prefetch_related(...).first()`
//! returned an unprefetched row and `.join_related(...).first()` an
//! unresolved join — both silently.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, ReverseSet};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fhr_book")]
pub struct Book {
    pub id: i64,
    pub author: ForeignKey<Author>,
    pub title: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fhr_author")]
pub struct Author {
    pub id: i64,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(reverse_fk = "author")]
    pub books: ReverseSet<Book>,
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
            .model::<Book>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        sqlx::query("INSERT INTO fhr_author (name) VALUES ('Ada')")
            .execute(&pool)
            .await
            .expect("seed author");
        for title in ["B1", "B2"] {
            sqlx::query("INSERT INTO fhr_book (author, title) VALUES (1, ?)")
                .bind(title)
                .execute(&pool)
                .await
                .expect("seed book");
        }
    })
    .await;
}

#[tokio::test]
async fn first_hydrates_prefetch_related() {
    boot().await;
    let ada = Author::objects()
        .prefetch_related("books")
        .first()
        .await
        .expect("first")
        .expect("an author");
    let books = ada
        .books
        .resolved()
        .expect("prefetch_related hydrated by first() (was None)");
    assert_eq!(books.len(), 2, "Ada has 2 books");
}

#[tokio::test]
async fn first_hydrates_select_related() {
    boot().await;
    let book = Book::objects()
        .select_related("author")
        .first()
        .await
        .expect("first")
        .expect("a book");
    assert_eq!(book.author.resolved().expect("select_related").name, "Ada");
}

#[tokio::test]
async fn first_hydrates_join_related() {
    boot().await;
    let book = Book::objects()
        .left_join_related("author")
        .first()
        .await
        .expect("first")
        .expect("a book");
    assert_eq!(
        book.author
            .resolved()
            .expect("join_related hydrated by first() (was unresolved)")
            .name,
        "Ada"
    );
}
