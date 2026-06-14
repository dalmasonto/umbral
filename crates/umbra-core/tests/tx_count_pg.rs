//! Review #5 — `QuerySetTx::count()` must render `COUNT(*)` (bare SQL
//! asterisk), not `COUNT("*")` (a quoted identifier). SQLite tolerates the
//! quoted form, so the bug only surfaces on Postgres — where `COUNT("*")`
//! reads `*` as a column name and errors. This pins the fix on a live PG.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use umbra_core::db::begin_pg;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "txc_item")]
pub struct TxItem {
    pub id: i64,
    pub name: String,
}

#[tokio::test]
#[ignore = "needs UMBRA_TEST_POSTGRES_URL"]
async fn tx_count_renders_bare_asterisk_on_postgres() {
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
        .model::<TxItem>()
        .build()
        .expect("App::build");

    for ddl in [
        "DROP TABLE IF EXISTS txc_item",
        "CREATE TABLE txc_item (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL)",
        "INSERT INTO txc_item (name) VALUES ('a'), ('b'), ('c')",
    ] {
        sqlx::query(ddl).execute(&pool).await.expect("setup");
    }

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
