//! gaps6 #7 — behavioral: a computed column stays fresh automatically as its
//! source rows change, with NO manual recompute call. Real rows, the real
//! public ORM path.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::orm::{Aggregate, Materialized};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_booking")]
pub struct MfBooking {
    pub id: i64,
    pub payment_total: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_rsvp")]
pub struct MfRsvp {
    pub id: i64,
    pub booking_id: i64,
    pub amount: i64,
}

// A second, independent target/source pair whose `key_fn` genuinely returns
// `None` for some rows (a nullable FK), used to exercise the handler's
// `extract -> None` skip branch (materialized.rs: `let Some(pk_json) =
// extract(&instance) else { return }`) — distinct from
// `MfBooking`/`MfRsvp`, whose `key_fn` always returns `Some`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_target2")]
pub struct MfTarget2 {
    pub id: i64,
    pub cached: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_source2")]
pub struct MfSource2 {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub amount: i64,
}

#[derive(Debug)]
struct Boom;
impl std::fmt::Display for Boom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "boom")
    }
}
impl std::error::Error for Boom {}
impl From<sqlx::Error> for Boom {
    fn from(_: sqlx::Error) -> Self {
        Boom
    }
}
impl From<umbral::orm::write::WriteError> for Boom {
    fn from(_: umbral::orm::write::WriteError) -> Self {
        Boom
    }
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() -> sqlx::SqlitePool {
    static ONCE: tokio::sync::OnceCell<sqlx::SqlitePool> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mf.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<MfBooking>()
            .model::<MfRsvp>()
            .model::<MfTarget2>()
            .model::<MfSource2>()
            .materialize(
                Materialized::<MfBooking>::field(mf_booking::PAYMENT_TOTAL)
                    .from::<MfRsvp, _, _>(|r: &MfRsvp| Some(r.booking_id))
                    .recompute_typed(|booking_id: i64| async move {
                        let agg = MfRsvp::objects()
                            .filter(mf_rsvp::BOOKING_ID.eq(booking_id))
                            .aggregate(&[("total", Aggregate::sum("amount"))])
                            .await
                            .unwrap_or_default();
                        agg["total"].as_i64().unwrap_or(0)
                    }),
            )
            // key_fn returns `s.parent_id` directly (already `Option<i64>`) —
            // a source row with `parent_id: None` must never reach recompute.
            .materialize(
                Materialized::<MfTarget2>::field(mf_target2::CACHED)
                    .from::<MfSource2, _, _>(|s: &MfSource2| s.parent_id)
                    .recompute_typed(|_parent_id: i64| async move {
                        // Never runs in the None-key test — the extract ->
                        // None branch returns before recompute is called. A
                        // sentinel value makes an accidental invocation
                        // obvious if this assumption ever breaks.
                        999_i64
                    }),
            )
            .build()
            .unwrap();
        umbral_core::migrate::create_tables_for_tests()
            .await
            .unwrap();
        pool
    })
    .await
    .clone()
}

async fn payment_total(id: i64) -> i64 {
    MfBooking::objects()
        .filter(mf_booking::ID.eq(id))
        .get()
        .await
        .unwrap()
        .payment_total
}

#[tokio::test]
async fn creating_a_source_row_refreshes_the_target() {
    let _g = lock().lock().await;
    let _pool = boot().await;
    let b = MfBooking::objects()
        .create(MfBooking {
            id: 0,
            payment_total: 0,
        })
        .await
        .unwrap();

    MfRsvp::objects()
        .create(MfRsvp {
            id: 0,
            booking_id: b.id,
            amount: 30,
        })
        .await
        .unwrap();
    MfRsvp::objects()
        .create(MfRsvp {
            id: 0,
            booking_id: b.id,
            amount: 12,
        })
        .await
        .unwrap();

    assert_eq!(
        payment_total(b.id).await,
        42,
        "payment_total tracks the sum with no manual recompute"
    );
}

#[tokio::test]
async fn delete_of_a_source_row_refreshes_the_target() {
    let _g = lock().lock().await;
    let _pool = boot().await;
    let b = MfBooking::objects()
        .create(MfBooking {
            id: 0,
            payment_total: 0,
        })
        .await
        .unwrap();
    let r = MfRsvp::objects()
        .create(MfRsvp {
            id: 0,
            booking_id: b.id,
            amount: 50,
        })
        .await
        .unwrap();
    assert_eq!(payment_total(b.id).await, 50);

    MfRsvp::objects()
        .filter(mf_rsvp::ID.eq(r.id))
        .delete()
        .await
        .unwrap();
    assert_eq!(
        payment_total(b.id).await,
        0,
        "a deleted source row drops the cached value"
    );
}

#[tokio::test]
async fn a_rolled_back_source_write_does_not_refresh() {
    let _g = lock().lock().await;
    let pool = boot().await;
    let b = MfBooking::objects()
        .create(MfBooking {
            id: 0,
            payment_total: 0,
        })
        .await
        .unwrap();

    let bid = b.id;
    let res: Result<(), Boom> = db::transaction_sqlite(&pool, |tx| {
        Box::pin(async move {
            MfRsvp::objects()
                .on_tx(tx)
                .create(MfRsvp {
                    id: 0,
                    booking_id: bid,
                    amount: 99,
                })
                .await?;
            Err(Boom)
        })
    })
    .await;
    assert!(res.is_err());
    assert_eq!(
        payment_total(b.id).await,
        0,
        "a rolled-back source write fires no refresh"
    );
}

#[tokio::test]
async fn a_source_row_whose_key_fn_returns_none_triggers_no_refresh() {
    // MfSource2::parent_id is `Option<i64>`, and `key_fn = |s| s.parent_id`,
    // so a row with `parent_id: None` makes `extract(&instance)` return
    // `None` — the handler's `let Some(pk_json) = extract(&instance) else {
    // return }` branch. Prove the recompute never ran (not just "no panic"):
    // seed the target with a known cached value, insert the null-key source
    // row, and read the target back — it must be untouched, because a run
    // would have overwritten it with the sentinel 999.
    let _g = lock().lock().await;
    let _pool = boot().await;
    let t = MfTarget2::objects()
        .create(MfTarget2 { id: 0, cached: 7 })
        .await
        .unwrap();

    MfSource2::objects()
        .create(MfSource2 {
            id: 0,
            parent_id: None,
            amount: 5,
        })
        .await
        .unwrap();

    let refreshed = MfTarget2::objects()
        .filter(mf_target2::ID.eq(t.id))
        .get()
        .await
        .unwrap();
    assert_eq!(
        refreshed.cached, 7,
        "a None key_fn result must skip the recompute entirely — cached value stays untouched"
    );
}

#[tokio::test]
async fn a_source_row_pointing_at_a_nonexistent_target_is_a_silent_no_op() {
    // Here key_fn always returns Some, so simulate an orphaned FK by
    // pointing at a booking id that doesn't exist: extract() succeeds,
    // recompute runs, and the write-back matches zero rows via
    // filter_pk_eq — no panic, no error, nothing asserted beyond that.
    let _g = lock().lock().await;
    let _pool = boot().await;
    MfRsvp::objects()
        .create(MfRsvp {
            id: 0,
            booking_id: 999_999,
            amount: 5,
        })
        .await
        .unwrap();
    // No booking 999999 exists → nothing to assert beyond "no panic"; a prior
    // booking's total is untouched.
}
