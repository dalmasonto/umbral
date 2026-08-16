//! Live-Postgres proof for `chrono::NaiveDateTime` — the real bug the SQLite
//! round-trip can't catch (SQLite stores every temporal as text, so it can't
//! tell `TIMESTAMP` from `TIMESTAMPTZ`).
//!
//! Gated on `UMBRAL_TEST_POSTGRES_URL`. Its own binary because `App::build`
//! publishes process-global state (one build per binary).
//!
//! Asserts two things against a real PG:
//!   1. the migration engine emits `timestamp without time zone` (NOT
//!      `timestamptz`) for a `NaiveDateTime` field, and
//!   2. a naive wall-clock value round-trips through create + fetch — i.e. it
//!      decodes back out of the bare `TIMESTAMP` column that broke the
//!      `DateTime<Utc>` model.

use chrono::NaiveDate;

use umbral::prelude::*;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "naive_event_pg")]
pub struct NaiveEventPg {
    pub id: i64,
    pub label: String,
    pub happened_at: chrono::NaiveDateTime,
    pub cleared_at: Option<chrono::NaiveDateTime>,
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn naive_datetime_is_timestamp_without_tz_and_round_trips_on_postgres() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    let db = umbral::db::connect(&url)
        .await
        .expect("connect to Postgres");
    let umbral::db::DbPool::Postgres(pool) = &db else {
        panic!("UMBRAL_TEST_POSTGRES_URL must be a Postgres URL");
    };
    let pool = pool.clone();

    // Clean slate.
    sqlx::query("DROP TABLE IF EXISTS naive_event_pg")
        .execute(&pool)
        .await
        .expect("drop prior table");

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", db)
        .model::<NaiveEventPg>()
        .build()
        .expect("App::build");
    umbral_core::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema on Postgres");

    // 1. The DDL type is `timestamp without time zone`, distinct from the
    //    `timestamp with time zone` that a `DateTime<Utc>` field produces.
    let dtype: (String,) = sqlx::query_as(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_name = 'naive_event_pg' AND column_name = 'happened_at'",
    )
    .fetch_one(&pool)
    .await
    .expect("column exists");
    assert_eq!(
        dtype.0, "timestamp without time zone",
        "a NaiveDateTime field must be a bare TIMESTAMP, not TIMESTAMPTZ"
    );

    // 2. A naive wall-clock value round-trips out of the bare TIMESTAMP column.
    let ts = NaiveDate::from_ymd_opt(2026, 8, 16)
        .unwrap()
        .and_hms_opt(14, 30, 15)
        .unwrap();
    NaiveEventPg::objects()
        .create(NaiveEventPg {
            id: 1,
            label: "launch".to_string(),
            happened_at: ts,
            cleared_at: None,
        })
        .await
        .expect("create on Postgres");

    let rows = NaiveEventPg::objects()
        .filter(naive_event_pg::ID.eq(1))
        .fetch()
        .await
        .expect("fetch from Postgres — decodes the bare TIMESTAMP");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].happened_at, ts);
    assert_eq!(rows[0].cleared_at, None);

    sqlx::query("DROP TABLE IF EXISTS naive_event_pg")
        .execute(&pool)
        .await
        .ok();
}
