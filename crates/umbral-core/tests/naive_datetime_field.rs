//! `chrono::NaiveDateTime` as a model field type → Postgres `TIMESTAMP`
//! (without time zone), SQLite `TEXT`/`DATETIME`.
//!
//! Motivation: a foreign schema (Prisma's `DateTime`, Django's naive
//! `DateTimeField` on some configs) stores timestamps WITHOUT a time zone. The
//! existing `DateTime<Utc>` field maps to `TIMESTAMPTZ` and sqlx refuses to
//! decode a bare `TIMESTAMP` into it, so `inspectdb`-generated models over such
//! a DB can't be read. A `NaiveDateTime` field closes that gap.
//!
//! Two binaries' worth of concern folded into one: a SQLite round-trip (the
//! common dev path) and a live-Postgres round-trip gated on
//! `UMBRAL_TEST_POSTGRES_URL` (the real `TIMESTAMP`-decode bug).

use chrono::NaiveDate;
use tokio::sync::OnceCell;

use umbral::db;
use umbral::prelude::*;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "naive_event")]
pub struct NaiveEvent {
    pub id: i64,
    pub label: String,
    /// Naive wall-clock timestamp — no time zone.
    pub happened_at: chrono::NaiveDateTime,
    /// Nullable variant to exercise the `Option<NaiveDateTime>` column path.
    pub cleared_at: Option<chrono::NaiveDateTime>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<NaiveEvent>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

/// A `NaiveDateTime` field round-trips through the typed create + fetch path on
/// SQLite: the exact wall-clock value goes in and comes back, and the nullable
/// sibling stores `NULL`.
#[tokio::test]
async fn naive_datetime_round_trips_on_sqlite() {
    boot().await;

    let ts = NaiveDate::from_ymd_opt(2026, 8, 16)
        .unwrap()
        .and_hms_opt(14, 30, 15)
        .unwrap();

    NaiveEvent::objects()
        .create(NaiveEvent {
            id: 1,
            label: "launch".to_string(),
            happened_at: ts,
            cleared_at: None,
        })
        .await
        .expect("create with a naive timestamp");

    let rows = NaiveEvent::objects()
        .filter(naive_event::ID.eq(1))
        .fetch()
        .await
        .expect("fetch");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].happened_at, ts,
        "naive wall-clock value round-trips"
    );
    assert_eq!(rows[0].cleared_at, None, "nullable naive datetime is NULL");
}

// The real bug — Postgres `TIMESTAMP` decode — lives in its own binary
// (`naive_datetime_postgres.rs`), because `App::build` publishes process-global
// state and can only run once per binary.
