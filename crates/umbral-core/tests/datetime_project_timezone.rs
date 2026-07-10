//! gaps3 #42 — a naive datetime is interpreted in `Settings::time_zone`.
//!
//! Sibling of `datetime_utc_offsets.rs`, which covers offset-bearing input under
//! the default (UTC) project timezone. This binary sets
//! `time_zone = America/New_York` — a zone with DST, so the interesting cases
//! exist — and lives apart because `Settings` is published through a
//! process-global `OnceLock`.
//!
//! Two of the three cases here are the reason a naive datetime is not just
//! "UTC with the offset left off":
//!
//! - **Ambiguous.** When the clocks go back, `2026-11-01T01:30` happens twice in
//!   New York: once at 05:30Z (EDT) and once at 06:30Z (EST). There is no way to
//!   know which the user meant.
//! - **Nonexistent.** When the clocks go forward, `2026-03-08T02:30` never
//!   happens at all — the local clock jumps 02:00 → 03:00.
//!
//! `timezone::naive_local_to_utc` returns `None` for both, and its doc says the
//! caller "should surface a validation error rather than silently pick one of the
//! two possible UTC instants." These tests hold the write path to that.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use sqlx::SqlitePool;
use tokio::sync::OnceCell;
use umbral::migrate::ModelMeta;
use umbral::orm::DynQuerySet;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "dttz_event")]
pub struct DttzEvent {
    pub id: i64,
    pub label: String,
    pub at: DateTime<Utc>,
}

static BOOT: OnceCell<SqlitePool> = OnceCell::const_new();

async fn boot() -> SqlitePool {
    BOOT.get_or_init(build_once).await.clone()
}

async fn build_once() -> SqlitePool {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();
    settings.time_zone = Some("America/New_York".to_string());

    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<DttzEvent>()
        .build_deferred()
        .expect("App::build_deferred");

    sqlx::query(
        "CREATE TABLE dttz_event (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            label TEXT NOT NULL,
            at TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");

    pool
}

async fn insert_at(label: &str, at: &str) -> Result<(), String> {
    let meta = ModelMeta::for_::<DttzEvent>();
    let mut values = serde_json::Map::new();
    values.insert("label".into(), json!(label));
    values.insert("at".into(), json!(at));
    DynQuerySet::for_meta(&meta)
        .insert_json(&values)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn stored(label: &str) -> DateTime<Utc> {
    DttzEvent::objects()
        .filter(dttz_event::LABEL.eq(label))
        .first()
        .await
        .expect("query")
        .unwrap_or_else(|| panic!("row `{label}` should exist"))
        .at
}

/// The everyday case: a `<input type="datetime-local">` posts wall-clock time
/// with no offset, and it means wall-clock time *in the project's timezone*.
/// Noon in New York on 10 July is 16:00Z (EDT, UTC-4).
#[tokio::test]
async fn a_naive_input_is_interpreted_in_the_project_timezone() {
    boot().await;

    insert_at("summer", "2026-07-10T12:00:00")
        .await
        .expect("unambiguous summer time");
    assert_eq!(
        stored("summer").await,
        Utc.with_ymd_and_hms(2026, 7, 10, 16, 0, 0).unwrap(),
        "noon EDT is 16:00Z",
    );

    // And in winter the same wall-clock reads 17:00Z (EST, UTC-5) — proof the
    // offset comes from the zone's rules on that date, not a fixed number.
    insert_at("winter", "2026-01-10T12:00:00")
        .await
        .expect("unambiguous winter time");
    assert_eq!(
        stored("winter").await,
        Utc.with_ymd_and_hms(2026, 1, 10, 17, 0, 0).unwrap(),
        "noon EST is 17:00Z",
    );
}

/// An offset in the input always wins over the project timezone. `+03:00` means
/// `+03:00` no matter where the server thinks it lives.
#[tokio::test]
async fn an_explicit_offset_beats_the_project_timezone() {
    boot().await;

    insert_at("explicit", "2026-07-10T12:00:00+03:00")
        .await
        .expect("offset input");
    assert_eq!(
        stored("explicit").await,
        Utc.with_ymd_and_hms(2026, 7, 10, 9, 0, 0).unwrap(),
        "the carried offset is ground truth; the project tz must not re-interpret it",
    );
}

/// **The bug.** `2026-11-01T01:30` occurs twice in New York — 05:30Z and 06:30Z.
/// The write path must refuse it, not silently invent a third instant.
///
/// Before the fix, `json_to_sea_value` did
/// `naive_local_to_utc(naive).unwrap_or_else(|| naive.and_utc())`, storing
/// 01:30**Z** — four hours off either candidate, and a value the user never
/// meant. `naive_local_to_utc`'s own docs say the caller should surface a
/// validation error here.
#[tokio::test]
async fn an_ambiguous_local_time_is_rejected_not_silently_shifted() {
    boot().await;

    let err = insert_at("ambiguous", "2026-11-01T01:30:00")
        .await
        .expect_err("the DST overlap hour is ambiguous and must be rejected");

    assert!(
        err.to_lowercase().contains("at"),
        "the error must name the offending field; got: {err}",
    );
    assert!(
        err.to_lowercase().contains("ambiguous"),
        "the error must say the local time is ambiguous, so the caller can ask \
         for an explicit offset; got: {err}",
    );

    // And nothing was written.
    let count = DttzEvent::objects()
        .filter(dttz_event::LABEL.eq("ambiguous"))
        .count()
        .await
        .expect("count");
    assert_eq!(count, 0, "a rejected write must not leave a row");
}

/// The mirror case: `2026-03-08T02:30` never happens in New York — the clock
/// jumps 02:00 → 03:00. Storing it as 02:30Z would be a time the user could not
/// have meant.
#[tokio::test]
async fn a_nonexistent_local_time_is_rejected() {
    boot().await;

    let err = insert_at("nonexistent", "2026-03-08T02:30:00")
        .await
        .expect_err("the spring-forward gap hour does not exist and must be rejected");

    assert!(
        err.to_lowercase().contains("at"),
        "the error must name the offending field; got: {err}",
    );
    assert!(
        err.to_lowercase().contains("does not exist") || err.to_lowercase().contains("nonexistent"),
        "the error must say the local time does not exist; got: {err}",
    );
}
