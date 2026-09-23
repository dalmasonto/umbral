//! gaps6 #7 — the Materialized<M> builder erases M / S / Pk / V into a
//! non-generic MaterializedSpec. This tests the erasure boundary in isolation:
//! a source row's JSON maps to the right pk JSON, and the recompute closure's
//! typed value comes back as JSON.

use serde::{Deserialize, Serialize};
use serde_json::json;
use umbral_core::orm::Materialized;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ms_booking")]
pub struct MsBooking {
    pub id: i64,
    pub payment_total: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ms_rsvp")]
pub struct MsRsvp {
    pub id: i64,
    pub booking_id: i64,
    pub amount: i64,
}

#[tokio::test]
async fn builder_erases_source_extract_and_recompute_to_json() {
    let spec = Materialized::<MsBooking>::field(ms_booking::PAYMENT_TOTAL)
        .from::<MsRsvp, _, _>(|r: &MsRsvp| Some(r.booking_id))
        .recompute_typed(|booking_id: i64| async move { booking_id * 10 });

    assert_eq!(spec.target_col, "payment_total");
    assert_eq!(spec.sources.len(), 1);
    assert_eq!(spec.sources[0].table, "ms_rsvp");

    // extract: a source instance JSON → the affected target pk JSON
    let instance = json!({ "id": 5, "booking_id": 42, "amount": 3 });
    assert_eq!((spec.sources[0].extract)(&instance), Some(json!(42)));

    // recompute: pk JSON in → value JSON out
    let out = (spec.recompute)(json!(42)).await;
    assert_eq!(out, Some(json!(420)));
}
