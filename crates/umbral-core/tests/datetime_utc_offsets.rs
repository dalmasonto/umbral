//! gaps3 #42 — does a `DateTime<Utc>` field convert a non-UTC input to UTC?
//!
//! Storage is UTC everywhere (`TIMESTAMPTZ` on Postgres, ISO-8601 text on
//! SQLite). The only place a timezone exists is the marshalling boundary, and
//! there are two kinds of input:
//!
//! - **Offset-bearing** (`2026-07-10T12:00:00+03:00`, `...Z`) — the offset is
//!   ground truth. It must be converted to the same instant in UTC, whatever
//!   `Settings::time_zone` says.
//! - **Naive** (`2026-07-10T12:00:00`, and the `2026-07-10T12:00` that
//!   `<input type="datetime-local">` posts) — no offset, so it is interpreted in
//!   the project timezone, then converted.
//!
//! This binary pins the first kind, with the default `time_zone = None` (i.e.
//! UTC), and drives the real dynamic write path (`DynQuerySet::insert_json`,
//! what the admin form-submit and REST create both call) against real SQLite
//! rows. `datetime_project_timezone.rs` covers the second kind; it lives in its
//! own binary because `Settings` is published through a process-global
//! `OnceLock`.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use sqlx::SqlitePool;
use tokio::sync::OnceCell;
use umbral::migrate::ModelMeta;
use umbral::orm::DynQuerySet;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "dt_event")]
pub struct DtEvent {
    pub id: i64,
    pub label: String,
    pub at: DateTime<Utc>,
}

/// `App::build*()` publishes process-global `OnceLock`s and panics on a second
/// call, so the whole binary shares one boot.
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
    assert!(
        settings.time_zone.is_none(),
        "this binary asserts the default (UTC) project timezone",
    );

    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<DtEvent>()
        .build_deferred()
        .expect("App::build_deferred");

    sqlx::query(
        "CREATE TABLE dt_event (
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

/// Insert through the dynamic JSON path — the one the admin form-submit and
/// umbral-rest's create handler both go through — and read the row back through
/// the typed ORM, so the value crosses both the encode and the decode boundary.
async fn insert_at(label: &str, at: &str) -> Result<(), String> {
    let meta = ModelMeta::for_::<DtEvent>();
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
    DtEvent::objects()
        .filter(dt_event::LABEL.eq(label))
        .first()
        .await
        .expect("query")
        .unwrap_or_else(|| panic!("row `{label}` should exist"))
        .at
}

/// The question as asked: a datetime arriving with a non-UTC offset must land in
/// the column as the same *instant*, expressed in UTC. `12:00+03:00` is `09:00Z`.
#[tokio::test]
async fn offsets_are_converted_to_the_same_instant_in_utc() {
    boot().await;

    let expected = Utc.with_ymd_and_hms(2026, 7, 10, 9, 0, 0).unwrap();

    // Four spellings of one instant: east of UTC, west of UTC, `Z`, and `+00:00`.
    insert_at("east", "2026-07-10T12:00:00+03:00")
        .await
        .expect("east");
    insert_at("west", "2026-07-10T04:00:00-05:00")
        .await
        .expect("west");
    insert_at("zulu", "2026-07-10T09:00:00Z").await.expect("z");
    insert_at("zero", "2026-07-10T09:00:00+00:00")
        .await
        .expect("zero");

    for label in ["east", "west", "zulu", "zero"] {
        assert_eq!(
            stored(label).await,
            expected,
            "`{label}` must normalise to the same UTC instant",
        );
    }
}

/// With no project timezone configured, a naive input is UTC — the historical
/// behaviour, and what `time_zone = None` means.
#[tokio::test]
async fn a_naive_input_is_utc_when_no_project_timezone_is_set() {
    boot().await;

    insert_at("naive_seconds", "2026-07-10T09:00:00")
        .await
        .expect("naive with seconds");
    // The literal shape `<input type="datetime-local">` posts.
    insert_at("naive_minutes", "2026-07-10T09:00")
        .await
        .expect("naive without seconds");

    let expected = Utc.with_ymd_and_hms(2026, 7, 10, 9, 0, 0).unwrap();
    assert_eq!(stored("naive_seconds").await, expected);
    assert_eq!(stored("naive_minutes").await, expected);
}

/// Normalising on write is what makes comparison correct. Two rows written with
/// different offsets must order and filter by their real instants, not by the
/// text they arrived as — `04:00-05:00` is *later* than `06:00+03:00`.
#[tokio::test]
async fn rows_written_with_different_offsets_compare_by_instant() {
    boot().await;

    // 03:00Z — arrives spelled as 06:00 in a +03:00 zone.
    insert_at("earlier", "2026-07-10T06:00:00+03:00")
        .await
        .expect("earlier");
    // 09:00Z — arrives spelled as 04:00 in a -05:00 zone.
    insert_at("later", "2026-07-10T04:00:00-05:00")
        .await
        .expect("later");

    // Every test in this binary shares the one booted pool and table, and they
    // run concurrently — so scope to this test's own two rows.
    let mine = || dt_event::LABEL.eq("earlier") | dt_event::LABEL.eq("later");

    let noon_utc = Utc.with_ymd_and_hms(2026, 7, 10, 6, 0, 0).unwrap();
    let after: Vec<String> = DtEvent::objects()
        .filter(mine() & dt_event::AT.gt(noon_utc))
        .fetch()
        .await
        .expect("filter")
        .into_iter()
        .map(|e| e.label)
        .collect();

    assert_eq!(
        after,
        vec!["later".to_string()],
        "only the 09:00Z row is after 06:00Z; a lexicographic compare on the raw \
         input text would have picked `earlier` instead (\"06:00\" > \"04:00\")",
    );

    let ordered: Vec<String> = DtEvent::objects()
        .filter(mine())
        .order_by(dt_event::AT.asc())
        .fetch()
        .await
        .expect("order")
        .into_iter()
        .map(|e| e.label)
        .collect();
    assert_eq!(ordered, vec!["earlier".to_string(), "later".to_string()]);
}

/// A string that isn't a datetime is a type error, not a silent zero.
#[tokio::test]
async fn garbage_is_rejected_rather_than_defaulted() {
    boot().await;

    let err = insert_at("bad", "not a date")
        .await
        .expect_err("a non-datetime string must be rejected");
    assert!(
        err.to_lowercase().contains("at"),
        "the error should name the offending field; got: {err}",
    );
}
