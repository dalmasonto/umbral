// The private test model emits public column constants from `#[derive(Model)]`.
#![allow(dead_code, private_interfaces)]

//! `bigdecimal::BigDecimal` — arbitrary-precision `numeric`, the sibling of
//! `rust_decimal::Decimal` for values past its ~28-significant-digit ceiling.
//!
//! Coverage layers:
//!
//! - **Derive classification.** A `BigDecimal` field lands as
//!   `SqlType::BigDecimal`; `Option<BigDecimal>` as nullable.
//! - **DDL.** Postgres renders unbounded `numeric` (no `(p, s)`), so the column
//!   stores as many digits as the value carries.
//! - **Backend gating.** BigDecimal against SQLite fails the boot system check
//!   exactly like Decimal.
//! - **Live PG round-trip.** A 44-significant-digit value — which
//!   `rust_decimal::Decimal` cannot represent without truncation — survives the
//!   dynamic write path (`coerce_bigdecimal`) and both the dynamic and typed
//!   read paths byte-for-byte. This is the whole reason BigDecimal exists.

use std::str::FromStr;

use umbral::migrate::ModelMeta;
use umbral::orm::{DynQuerySet, Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "umbral_bigdecimal_ledger")]
struct Ledger {
    id: i64,
    amount: bigdecimal::BigDecimal,
    note: Option<bigdecimal::BigDecimal>,
}

/// The value that motivates the whole type: 35 integer digits + 9 fractional =
/// 44 significant digits, well past `rust_decimal`'s ~28-digit capacity.
const HUGE: &str = "12345678901234567890123456789012345.123456789";

#[test]
fn derive_classifies_bigdecimal_as_bigdecimal_sqltype() {
    let by_name: std::collections::HashMap<&str, &umbral::orm::FieldSpec> =
        <Ledger as Model>::FIELDS
            .iter()
            .map(|f| (f.name, f))
            .collect();

    let amount = by_name.get("amount").expect("amount field");
    assert_eq!(amount.ty, SqlType::BigDecimal);
    assert!(!amount.nullable);

    let note = by_name.get("note").expect("note field");
    assert_eq!(note.ty, SqlType::BigDecimal);
    assert!(note.nullable, "Option<BigDecimal> is the nullable variant");
}

#[test]
fn postgres_ddl_renders_unbounded_numeric() {
    use umbral::migrate::{Column, Operation, render_operation_for};

    let op = Operation::CreateTable {
        table: "umbral_bigdecimal_ledger".to_string(),
        columns: vec![Column::from(
            <Ledger as Model>::FIELDS
                .iter()
                .find(|f| f.name == "amount")
                .unwrap(),
        )],
        indexes: Vec::new(),
        unique_together: Vec::new(),
    };
    let sql = render_operation_for(&op, "postgres")
        .join("\n")
        .to_lowercase();
    // `decimal` and `numeric` are exact synonyms in Postgres; sea-query renders
    // the unbounded form as `decimal`. Either spelling is the arbitrary-
    // precision type.
    assert!(
        sql.contains("decimal") || sql.contains("numeric"),
        "BigDecimal must render as unbounded decimal/numeric; got: {sql}"
    );
    assert!(
        !sql.contains("(19"),
        "BigDecimal is unbounded — it must NOT carry the fixed (19, 4) dimensions: {sql}"
    );
}

// Live Postgres round-trip. Skipped without `UMBRAL_TEST_POSTGRES_URL`.
#[tokio::test]
#[ignore = "needs a live Postgres (UMBRAL_TEST_POSTGRES_URL); BigDecimal is Postgres-only"]
async fn bigdecimal_round_trips_a_value_beyond_rust_decimal() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        return;
    };

    // Sanity: the motivating value genuinely overflows rust_decimal. If this
    // ever starts parsing, the test has lost its point.
    assert!(
        rust_decimal::Decimal::from_str(HUGE).is_err(),
        "HUGE must be un-representable in rust_decimal for this test to prove anything"
    );
    // ...but it IS representable in bigdecimal.
    let expected =
        bigdecimal::BigDecimal::from_str(HUGE).expect("bigdecimal parses the 44-digit value");

    let pool = umbral_core::db::connect_postgres(&url)
        .await
        .expect("pg pool");
    let mut settings = umbral::Settings::from_env().expect("figment defaults");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Ledger>()
        .build()
        .expect("App::build (BigDecimal is valid on Postgres)");

    // Fresh table via the ORM migration DDL path (also exercises the `numeric`
    // render end to end).
    sqlx::query("DROP TABLE IF EXISTS umbral_bigdecimal_ledger")
        .execute(&pool)
        .await
        .unwrap();
    umbral_core::migrate::create_tables_for_tests()
        .await
        .expect("create the BigDecimal test table");

    // WRITE via the dynamic path — this is where `coerce_bigdecimal` runs.
    let meta = ModelMeta::for_::<Ledger>();
    let mut body = serde_json::Map::new();
    body.insert("amount".into(), serde_json::json!(HUGE));
    let returned = DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect("dynamic insert of a 44-digit numeric must succeed");

    // Dynamic read path: the JSON the write returned must carry the full value.
    // Postgres' base-10000 numeric wire format pads the scale up to a multiple
    // of 4 (here 9 → 12 fractional digits), so the *string* gains trailing
    // zeros — but the number is unchanged. Compare numerically after stripping
    // trailing zeros; a truncated value (rust_decimal's failure mode) would
    // differ in the significant digits, which this still catches.
    let returned_amount = returned
        .get("amount")
        .and_then(|v| v.as_str())
        .expect("amount present in the returned row");
    assert_eq!(
        bigdecimal::BigDecimal::from_str(returned_amount)
            .unwrap()
            .normalized(),
        expected.normalized(),
        "the dynamic write/read path must preserve all 44 significant digits \
         (returned string was {returned_amount})"
    );
    let new_id = returned
        .get("id")
        .and_then(|v| v.as_i64())
        .expect("returned row carries the new id");

    // Typed read path: fetch_pg decodes the column straight into BigDecimal.
    let rows = Ledger::objects()
        .filter(ledger::ID.eq(new_id))
        .fetch_pg(&pool)
        .await
        .expect("fetch_pg");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].amount.normalized(),
        expected.normalized(),
        "the typed decode must round-trip the exact arbitrary-precision value"
    );
}
