// The private test model emits public column constants from `#[derive(Model)]`.
#![allow(dead_code, private_interfaces)]

//! Round-trip sweep for every SQLite-portable `SqlType`.
//!
//! One model carries one field per column type SQLite supports, and the test
//! inserts a row through the ORM (`create`) and reads it back through the ORM
//! (`get`), asserting the value survives the write → DDL → bind → decode round
//! trip byte-for-byte. This is the behavioural proof — separate from the
//! DDL-shape unit tests — that each field type is actually *usable*, not merely
//! mappable to a `ColumnType`.
//!
//! The nine Postgres-only types (Decimal, Array, Inet/Cidr/MacAddr, Xml, Ltree,
//! Bit, tsvector) live in `roundtrip_postgres.rs`, which self-skips unless
//! `UMBRAL_TEST_POSTGRES_URL` is set.

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use serde_json::json;
use sqlx::SqlitePool;
use tokio::sync::OnceCell;
use uuid::Uuid;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
)]
#[umbral(table = "roundtrip_sqlite_all")]
struct Sweep {
    id: i64,
    // Integer widths.
    f_small: i16,
    f_int: i32,
    f_big: i64,
    // Floats.
    f_real: f32,
    f_double: f64,
    // Scalar leaves.
    f_bool: bool,
    f_text: String,
    // Temporal.
    f_date: NaiveDate,
    f_time: NaiveTime,
    f_ts: DateTime<Utc>,
    // Opaque / structured.
    f_uuid: Uuid,
    f_json: serde_json::Value,
    f_bytes: Vec<u8>,
    // Nullable coverage: the `Option<T>` leg of the same round trip.
    f_opt_int: Option<i32>,
    f_opt_text: Option<String>,
}

static BOOT: OnceCell<SqlitePool> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let mut settings = umbral::Settings::from_env().expect("settings");
        settings.database_url = "sqlite::memory:".to_string();
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Sweep>()
            .build_deferred()
            .expect("App::build_deferred");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        pool
    })
    .await;
}

/// A representative, boundary-heavy value for every column. Constructed once so
/// the create and the assert share exactly one source of truth.
fn sample() -> Sweep {
    Sweep {
        id: 0,
        f_small: i16::MIN,
        f_int: i32::MAX,
        f_big: i64::MIN,
        f_real: 1.5,    // exactly representable — no float-compare slop
        f_double: 2.25, // exactly representable
        f_bool: true,
        f_text: "üñîçødé — with a 'quote' and a \\ backslash".to_string(),
        f_date: NaiveDate::from_ymd_opt(2024, 2, 29).unwrap(), // leap day
        f_time: NaiveTime::from_hms_micro_opt(23, 59, 58, 123_456).unwrap(),
        f_ts: Utc.with_ymd_and_hms(2026, 8, 16, 12, 34, 56).unwrap(),
        f_uuid: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
        f_json: json!({"nested": {"a": [1, 2, 3], "b": null}, "s": "x"}),
        f_bytes: vec![0u8, 255, 1, 128, 0, 42],
        f_opt_int: Some(-7),
        f_opt_text: None,
    }
}

#[tokio::test]
async fn every_portable_type_round_trips_through_the_orm() {
    boot().await;
    let created = Sweep::objects()
        .create(sample())
        .await
        .expect("create a row with every portable field type");

    let fetched = Sweep::objects()
        .get(sweep::ID.eq(created.id))
        .await
        .expect("read the row back by primary key");

    // The row read back must equal what we asked to store, field for field.
    // `PartialEq` compares every column at once; the message narrows the blame.
    assert_eq!(
        fetched,
        Sweep {
            id: created.id,
            ..sample()
        },
        "a stored row must round-trip unchanged across all SQLite-portable types",
    );
}

/// The nullable columns must round-trip a `Some` value too (the test above
/// stores `None` for `f_opt_text`); this pins the `Some` leg for both.
#[tokio::test]
async fn nullable_columns_round_trip_a_present_value() {
    boot().await;
    let mut input = sample();
    input.f_opt_int = None;
    input.f_opt_text = Some("present".to_string());
    let created = Sweep::objects().create(input).await.expect("create");

    let fetched = Sweep::objects()
        .get(sweep::ID.eq(created.id))
        .await
        .expect("read back");

    assert_eq!(fetched.f_opt_int, None);
    assert_eq!(fetched.f_opt_text.as_deref(), Some("present"));
}
