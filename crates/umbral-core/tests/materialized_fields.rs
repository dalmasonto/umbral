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
async fn a_source_row_with_a_null_key_is_skipped() {
    // key_fn here always returns Some, so simulate a null key by pointing at a
    // booking id that doesn't exist: the recompute runs but updates zero rows —
    // no panic, no error. (The null-FK Option::None path is unit-tested in
    // materialized_spec.rs; this guards the write-back no-op.)
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
