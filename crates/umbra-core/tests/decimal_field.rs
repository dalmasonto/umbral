//! Postgres fixed-point NUMERIC field type — `rust_decimal::Decimal`
//! classifies as `SqlType::Decimal` (`NUMERIC(19, 4)`). Decimal is
//! Postgres-only (rust_decimal only implements the sqlx traits for
//! Postgres), so — like the network types in `network_field.rs` — a model
//! with a Decimal field must use the `_pg` query terminals, and the live
//! round-trip is behind `#[ignore]`. Derive classification runs anywhere.
//!
//! `Option<Decimal>` now classifies as `NullableDecimal` (closes gaps2 #70).

use umbra::orm::{Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "umbra_decimal_invoice")]
pub struct Invoice {
    pub id: i64,
    pub total: rust_decimal::Decimal,
}

/// Model with a nullable `Option<Decimal>` field — was "M3 doesn't support
/// this field type" before gaps2 #70 was closed.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "umbra_decimal_quote")]
pub struct Quote {
    pub id: i64,
    /// Non-nullable sanity check — must still work alongside nullable.
    pub required_total: rust_decimal::Decimal,
    /// Nullable NUMERIC — previously unsupported (gaps2 #70).
    pub discount: Option<rust_decimal::Decimal>,
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

// =============================================================================
// NullableDecimal — `Option<rust_decimal::Decimal>` support (gaps2 #70)
// =============================================================================

/// `Option<Decimal>` classifies as nullable `SqlType::Decimal`.
/// Before this fix the derive emitted "M3 doesn't support this field type".
#[test]
fn nullable_decimal_classifies_as_nullable_decimal_sqltype() {
    let by_name: std::collections::HashMap<&str, &umbra::orm::FieldSpec> =
        <Quote as Model>::FIELDS
            .iter()
            .map(|f| (f.name, f))
            .collect();

    let required_total = by_name.get("required_total").expect("required_total field");
    assert_eq!(required_total.ty, SqlType::Decimal);
    assert!(!required_total.nullable, "non-nullable Decimal must not be nullable");

    let discount = by_name.get("discount").expect("discount field");
    assert_eq!(
        discount.ty,
        SqlType::Decimal,
        "Option<Decimal> should classify as SqlType::Decimal"
    );
    assert!(discount.nullable, "Option<Decimal> must be nullable");
}

/// Column constants for `Option<Decimal>` expose `NullableDecimalCol`.
#[test]
fn nullable_decimal_produces_nullable_decimal_col_constant() {
    use umbra::orm::column::{DecimalCol, NullableDecimalCol};
    let _: DecimalCol<Quote> = quote::REQUIRED_TOTAL;
    let _: NullableDecimalCol<Quote> = quote::DISCOUNT;
}
