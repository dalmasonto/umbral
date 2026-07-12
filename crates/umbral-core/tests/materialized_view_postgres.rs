//! features #73 — materialized views, against a REAL Postgres.
//!
//! Gated + `#[ignore]`d: set `UMBRAL_TEST_POSTGRES_URL` and run with `-- --ignored`.
//! SQLite has no materialized views, so nothing below can be verified without a live
//! Postgres — and "the SQLite suite is green" would prove exactly nothing about the
//! only backend this feature runs on.
//!
//! The test hinges on the property that DEFINES a materialized view: it is stale. Its
//! rows are computed once, at CREATE time, and do not move when the underlying table
//! does. So we insert orders, assert the view still shows the OLD answer, then refresh
//! and assert it catches up. A plain view would pass the "reads rows" test just as
//! well; only staleness proves the MATERIALIZED keyword actually took effect.

use serde::{Deserialize, Serialize};
use umbral::migrate::{ModelMeta, Snapshot, diff, render_operation_for};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mvp_order")]
pub struct MvpOrder {
    pub id: i64,
    pub customer: String,
    pub amount: i64,
}

/// Note the `CAST(... AS BIGINT)`. Postgres's `SUM(bigint)` returns NUMERIC, which
/// does not decode into an `i64` field — the framework cannot catch that, because the
/// SELECT list is an opaque string it never parses. `CAST` is standard SQL and both
/// backends accept it, so it is also the portable spelling. This test exists partly to
/// keep that fact discovered rather than rediscovered.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(
    table = "mvp_total",
    materialized_view = "SELECT MIN(id) AS id, customer, CAST(SUM(amount) AS BIGINT) AS total FROM mvp_order GROUP BY customer"
)]
pub struct MvpTotal {
    pub id: i64,
    pub customer: String,
    pub total: i64,
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn materialized_view_is_stale_until_refreshed() {
    let url = std::env::var("UMBRAL_TEST_POSTGRES_URL")
        .expect("set UMBRAL_TEST_POSTGRES_URL to run this");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connect postgres");

    // Clean slate for a re-run against the same cluster.
    for stmt in [
        "DROP MATERIALIZED VIEW IF EXISTS \"mvp_total\"",
        "DROP TABLE IF EXISTS \"mvp_order\"",
    ] {
        sqlx::query(stmt).execute(&pool).await.expect("reset");
    }

    // Settings default to a SQLite URL; the App refuses a pool whose backend does not
    // match the configured URL, which is a check worth having and worth satisfying
    // honestly rather than bypassing.
    //
    // SAFETY: single-threaded test, set before any other thread reads the environment.
    unsafe { std::env::set_var("UMBRAL_DATABASE_URL", &url) };
    let settings = umbral::Settings::from_env().expect("figment defaults");
    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<MvpOrder>()
        .model::<MvpTotal>()
        .build()
        .expect("App::build must succeed — a materialized view IS supported on postgres");

    let models = vec![ModelMeta::for_::<MvpOrder>(), ModelMeta::for_::<MvpTotal>()];
    let ops = diff(&Snapshot::default(), &Snapshot { models }).expect("diff");
    for op in &ops {
        for stmt in render_operation_for(op, "postgres") {
            sqlx::query(&stmt)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("applying `{stmt}` failed: {e}"));
        }
    }

    // It really is MATERIALIZED as far as Postgres is concerned — ask the catalog, not
    // our own renderer, which would just be marking its own homework.
    let relkind: String =
        sqlx::query_scalar("SELECT relkind::text FROM pg_class WHERE relname = 'mvp_total'")
            .fetch_one(&pool)
            .await
            .expect("pg_class lookup");
    assert_eq!(
        relkind, "m",
        "relkind 'm' = materialized view ('v' = plain view)"
    );

    // The view was created over an empty table, so it holds zero rows...
    assert_eq!(
        MvpTotal::objects().count().await.expect("count"),
        0,
        "a fresh materialized view over an empty table holds nothing"
    );

    // ...and it STAYS empty as the table fills. This is the whole point: the rows are
    // stored, not recomputed. A plain view would already show 42 here.
    for (customer, amount) in [("ada", 30), ("ada", 12)] {
        MvpOrder::objects()
            .create(MvpOrder {
                id: 0,
                customer: customer.to_string(),
                amount,
            })
            .await
            .expect("seed order");
    }
    assert_eq!(
        MvpTotal::objects().count().await.expect("count"),
        0,
        "a materialized view does NOT see new rows until it is refreshed — if this \
         fails, the view was created as a plain VIEW and the whole feature is a lie"
    );

    // Now refresh, and it catches up.
    umbral::db::refresh_view::<MvpTotal>()
        .await
        .expect("refresh must succeed on postgres");

    let ada = MvpTotal::objects()
        .filter(mvp_total::CUSTOMER.eq("ada"))
        .first()
        .await
        .expect("query view")
        .expect("ada present after refresh");
    assert_eq!(ada.total, 42, "the refresh recomputed the SUM");
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn refresh_view_rejects_a_model_that_is_not_materialized() {
    // Guarding the caller against a silent no-op: refreshing a plain view or a table
    // is always a mistake, and returning Ok(()) would hide it forever.
    let err = format!(
        "{}",
        umbral::db::refresh_view::<MvpOrder>()
            .await
            .expect_err("a plain table cannot be refreshed")
    );
    assert!(err.contains("materialized_view"), "got: {err}");
}
