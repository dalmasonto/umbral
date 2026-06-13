//! PK refactor — the win. A parent model with a `String` (slug) primary
//! key carries a `ReverseSet<Article>` and hydrates it via
//! `prefetch_related`, end to end. Before the lift this was impossible:
//! the reverse-FK hydrator collected parents via the i64-only `pk_i64()`
//! and the `ReverseSet` slot stored `Option<i64>`, so a String-PK parent
//! either failed to compile or silently hydrated nothing.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::{ForeignKey, ReverseSet};
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "pkrfk_article")]
pub struct Article {
    pub id: i64,
    pub title: String,
    /// FK pointing at a `String`-PK parent — the column holds the slug.
    pub author: ForeignKey<Author>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "pkrfk_author")]
pub struct Author {
    /// Slug primary key — a `String`, not an `i64`.
    #[umbra(primary_key)]
    pub slug: String,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(reverse_fk = "author")]
    pub articles: ReverseSet<Article>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Article>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE pkrfk_author (
                slug TEXT PRIMARY KEY,
                name TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE pkrfk_author");
        sqlx::query(
            "CREATE TABLE pkrfk_article (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                author TEXT NOT NULL REFERENCES pkrfk_author(slug)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE pkrfk_article");

        for (slug, name) in &[("rust", "Rust"), ("go", "Go"), ("zig", "Zig")] {
            sqlx::query("INSERT INTO pkrfk_author (slug, name) VALUES (?, ?)")
                .bind(*slug)
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed author");
        }
        // rust: 2 articles, go: 1, zig: 0.
        for (title, author) in &[
            ("ownership", "rust"),
            ("borrowing", "rust"),
            ("goroutines", "go"),
        ] {
            sqlx::query("INSERT INTO pkrfk_article (title, author) VALUES (?, ?)")
                .bind(*title)
                .bind(*author)
                .execute(&pool)
                .await
                .expect("seed article");
        }
    })
    .await;
}

#[tokio::test]
async fn prefetch_hydrates_reverse_set_on_a_string_pk_parent() {
    boot().await;
    let authors = Author::objects()
        .prefetch_related("articles")
        .fetch()
        .await
        .expect("fetch");

    let by_slug: std::collections::HashMap<&str, &Author> =
        authors.iter().map(|a| (a.slug.as_str(), a)).collect();

    let rust = by_slug.get("rust").expect("rust present");
    let rust_articles = rust
        .articles
        .resolved()
        .expect("ReverseSet hydrated for a String-PK parent");
    assert_eq!(rust_articles.len(), 2, "rust has 2 articles");
    let titles: Vec<&str> = rust_articles.iter().map(|a| a.title.as_str()).collect();
    assert!(titles.contains(&"ownership"));
    assert!(titles.contains(&"borrowing"));

    let go = by_slug.get("go").expect("go present");
    assert_eq!(go.articles.resolved().expect("hydrated").len(), 1);

    let zig = by_slug.get("zig").expect("zig present");
    assert!(
        zig.articles
            .resolved()
            .expect("hydrated (empty)")
            .is_empty(),
        "zig has no children → resolved is Some(&[])"
    );
}
