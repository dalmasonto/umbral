//! Review #5 — `QuerySetTx::count()` must render `COUNT(*)` (bare SQL
//! asterisk), not `COUNT("*")` (a quoted identifier). SQLite tolerates the
//! quoted form, so the bug only surfaces on Postgres — where `COUNT("*")`
//! reads `*` as a column name and errors. This pins the fix on a live PG.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use umbral_core::db::begin_pg;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "txc_item")]
pub struct TxItem {
    pub id: i64,
    pub name: String,
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn tx_count_renders_bare_asterisk_on_postgres() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect");

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<TxItem>()
        .build()
        .expect("App::build");

    sqlx::query("DROP TABLE IF EXISTS txc_item")
        .execute(&pool)
        .await
        .expect("setup");
    umbral_core::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema");
    sqlx::query("INSERT INTO txc_item (name) VALUES ('a'), ('b'), ('c')")
        .execute(&pool)
        .await
        .expect("setup");

    let mut tx = begin_pg(&pool).await.expect("begin");
    // Pre-fix this errored with `column "*" does not exist`.
    let n = TxItem::objects()
        .on_tx(&mut tx)
        .count()
        .await
        .expect("COUNT(*) inside a Postgres transaction");
    assert_eq!(n, 3);
    tx.commit().await.expect("commit");

    sqlx::query("DROP TABLE txc_item")
        .execute(&pool)
        .await
        .expect("cleanup");
}
