//! Postgres fixed-point NUMERIC field type — `rust_decimal::Decimal`
//! classifies as `SqlType::Decimal` (`NUMERIC(19, 4)`). Decimal is
//! Postgres-only (rust_decimal only implements the sqlx traits for
//! Postgres), so — like the network types in `network_field.rs` — a model
//! with a Decimal field must use the `_pg` query terminals, and the live
//! round-trip is behind `#[ignore]`. Derive classification runs anywhere.
//!
//! Note: only a *non-nullable* Decimal is supported today; `Option<Decimal>`
//! has no `NullableDecimal` classification yet (tracked in gaps2 #70).

use umbra::orm::{Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "umbra_decimal_invoice")]
pub struct Invoice {
    pub id: i64,
    pub total: rust_decimal::Decimal,
}

#[test]
fn derive_classifies_decimal_as_decimal_sqltype() {
    let by_name: std::collections::HashMap<&str, &umbra::orm::FieldSpec> =
        <Invoice as Model>::FIELDS
            .iter()
            .map(|f| (f.name, f))
            .collect();

    let total = by_name.get("total").expect("total field");
    assert_eq!(total.ty, SqlType::Decimal);
    assert!(!total.nullable);
}

// Live Postgres round-trip — a NUMERIC value persists and decodes back
// losslessly. Uses `fetch_pg` (the Postgres-only terminal): the cross-backend
// `fetch()` can't satisfy the dual-backend `FromRow` bound because
// `rust_decimal::Decimal` only decodes from Postgres rows. Skipped without a
// Postgres URL.
#[tokio::test]
#[ignore = "needs a live Postgres (UMBRA_TEST_POSTGRES_URL); Decimal is Postgres-only"]
async fn decimal_round_trips_on_postgres() {
    let Ok(url) = std::env::var("UMBRA_TEST_POSTGRES_URL") else {
        return;
    };
    let pool = umbra_core::db::connect_postgres(&url)
        .await
        .expect("pg pool");
    let settings = umbra::Settings::from_env().expect("figment defaults");
    umbra::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Invoice>()
        .build()
        .expect("App::build (Decimal is valid on Postgres)");

    sqlx::query("DROP TABLE IF EXISTS umbra_decimal_invoice")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE umbra_decimal_invoice (id BIGSERIAL PRIMARY KEY, total NUMERIC(19,4) NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO umbra_decimal_invoice (total) VALUES ($1)")
        .bind(rust_decimal::Decimal::new(12345, 2)) // 123.45
        .execute(&pool)
        .await
        .unwrap();

    let rows = Invoice::objects().fetch_pg(&pool).await.expect("fetch_pg");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].total, rust_decimal::Decimal::new(12345, 2));
}
