//! gaps6 #7 — a misdeclared materialized field aborts boot with a clear error,
//! rather than silently never refreshing.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use umbral::orm::Materialized;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mv_booking")]
pub struct MvBooking {
    pub id: i64,
    pub total: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mv_rsvp")]
pub struct MvRsvp {
    pub id: i64,
    pub booking_id: i64,
}

#[tokio::test]
async fn a_target_column_that_does_not_exist_aborts_build() {
    let settings = umbral::Settings::from_env().unwrap();
    let pool = SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let err = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<MvBooking>()
        .model::<MvRsvp>()
        .materialize(
            Materialized::<MvBooking>::field("nonexistent_col")
                .from::<MvRsvp, _, _>(|r: &MvRsvp| Some(r.booking_id))
                .recompute_typed(|_id: i64| async move { 0i64 }),
        )
        .build();
    assert!(err.is_err(), "an unknown target column must abort build");
    let msg = format!("{:?}", err.err().unwrap());
    assert!(
        msg.contains("nonexistent_col"),
        "the error names the bad column: {msg}"
    );
}
