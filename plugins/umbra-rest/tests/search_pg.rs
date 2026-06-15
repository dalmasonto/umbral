//! Postgres-only: real `ts_rank` ordering for `Search::across`. Gated exactly
//! like `rest_fts_pg.rs` — self-skips unless `UMBRA_TEST_POSTGRES_URL` points
//! at a Postgres server, and `#[ignore]`d so the default `cargo test` lane
//! doesn't try to reach a DB. Compiles regardless of whether PG is present.

#![allow(dead_code)]

use sqlx::PgPool;
use umbra::orm::{Search, Searchable};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "spg_plugin")]
pub struct Plugin {
    pub id: i64,
    pub name: String,
    pub blurb: String,
}
impl Searchable for Plugin {
    fn kind() -> &'static str {
        "plugin"
    }
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "spg_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
}
impl Searchable for Post {
    fn kind() -> &'static str {
        "post"
    }
}

#[tokio::test]
#[ignore = "needs UMBRA_TEST_POSTGRES_URL"]
async fn pg_ranks_title_match_above_body_match() {
    let Ok(url) = std::env::var("UMBRA_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRA_TEST_POSTGRES_URL not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect");

    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbra::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Plugin>()
        .model::<Post>()
        .build()
        .expect("build");

    for t in ["spg_plugin", "spg_post"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {t}"))
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query(
        "CREATE TABLE spg_plugin (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL, blurb TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE spg_post (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO spg_plugin (name, blurb) VALUES ('Redis cache','fast store')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO spg_plugin (name, blurb) VALUES ('Logger','sometimes uses redis')")
        .execute(&pool)
        .await
        .unwrap();

    let hits = Search::across::<(Plugin, Post)>("redis", 10)
        .await
        .expect("runs");
    assert!(!hits.is_empty(), "matches exist");
    assert_eq!(
        hits[0].title, "Redis cache",
        "title hit ranks first under real ts_rank: {hits:?}"
    );

    for t in ["spg_plugin", "spg_post"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {t}"))
            .execute(&pool)
            .await
            .unwrap();
    }
}
