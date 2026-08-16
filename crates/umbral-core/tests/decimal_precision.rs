// The private test model emits public column constants from `#[derive(Model)]`.
#![allow(dead_code, private_interfaces)]

//! `#[umbral(precision = N, scale = M)]` — caller-chosen `numeric(N, M)`
//! dimensions for a `rust_decimal::Decimal` column, instead of the default
//! `numeric(19, 4)`.
//!
//! - **Classification.** The attribute lands the field as
//!   `SqlType::DecimalN(DecimalSpec { precision, scale })`.
//! - **DDL.** Postgres renders `numeric(10, 2)`.
//! - **Live round-trip.** The declared scale is enforced by the database:
//!   `123.45` stores exactly, and the column is `numeric(10, 2)` in the
//!   catalog. Skipped without `UMBRAL_TEST_POSTGRES_URL` (Decimal is PG-only).

use std::str::FromStr;

use umbral::orm::{DecimalSpec, Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "umbral_decimal_precision_price")]
struct Price {
    id: i64,
    // Money with an explicit shape: 10 total digits, 2 after the point.
    #[umbral(precision = 10, scale = 2)]
    amount: rust_decimal::Decimal,
    // A default-dimension decimal for contrast (numeric(19, 4)).
    fallback: rust_decimal::Decimal,
}

#[test]
fn precision_scale_attribute_classifies_as_decimaln() {
    let by_name: std::collections::HashMap<&str, &umbral::orm::FieldSpec> =
        <Price as Model>::FIELDS
            .iter()
            .map(|f| (f.name, f))
            .collect();

    match by_name.get("amount").unwrap().ty {
        SqlType::DecimalN(DecimalSpec { precision, scale }) => {
            assert_eq!(precision, 10);
            assert_eq!(scale, 2);
        }
        other => panic!("expected DecimalN(10, 2), got {other:?}"),
    }
    // A field without the attribute stays the default fixed Decimal.
    assert_eq!(by_name.get("fallback").unwrap().ty, SqlType::Decimal);
}

#[test]
fn postgres_ddl_renders_the_chosen_dimensions() {
    use umbral::migrate::{Column, Operation, render_operation_for};

    let cols: Vec<Column> = <Price as Model>::FIELDS.iter().map(Column::from).collect();
    let op = Operation::CreateTable {
        table: "umbral_decimal_precision_price".to_string(),
        columns: cols,
        indexes: Vec::new(),
        unique_together: Vec::new(),
    };
    let sql = render_operation_for(&op, "postgres")
        .join("\n")
        .to_lowercase();
    assert!(
        sql.contains("numeric(10, 2)") || sql.contains("decimal(10, 2)"),
        "the precision/scale column must render numeric(10, 2); got:\n{sql}"
    );
    // The default-dimension column still renders (19, 4).
    assert!(
        sql.contains("(19, 4)"),
        "the attribute-free column keeps the (19, 4) default; got:\n{sql}"
    );
}

#[tokio::test]
#[ignore = "needs a live Postgres (UMBRAL_TEST_POSTGRES_URL); Decimal is Postgres-only"]
async fn decimaln_round_trips_and_enforces_scale() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        return;
    };
    let pool = umbral_core::db::connect_postgres(&url)
        .await
        .expect("pg pool");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Price>()
        .build()
        .expect("App::build (DecimalN is valid on Postgres)");

    sqlx::query("DROP TABLE IF EXISTS umbral_decimal_precision_price")
        .execute(&pool)
        .await
        .unwrap();
    umbral_core::migrate::create_tables_for_tests()
        .await
        .expect("create the numeric(10,2) table");

    // The catalog must report the declared precision/scale.
    let (prec, scale): (i32, i32) = sqlx::query_as(
        "SELECT numeric_precision, numeric_scale FROM information_schema.columns \
         WHERE table_name = 'umbral_decimal_precision_price' AND column_name = 'amount'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((prec, scale), (10, 2), "column must be numeric(10, 2)");

    // Insert through the ORM dynamic write path (coerce -> numeric bind). The
    // typed `create()` can't be used for a Decimal model (rust_decimal only
    // decodes from Postgres, so the cross-backend FromRow bound fails), so REST/
    // admin route through the dynamic path — which is what runs `coerce_decimal`.
    let meta = umbral::migrate::ModelMeta::for_::<Price>();
    let mut body = serde_json::Map::new();
    body.insert("amount".into(), serde_json::json!("123.45"));
    body.insert("fallback".into(), serde_json::json!("1.0"));
    umbral::orm::DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect("insert a numeric(10,2) row");

    let rows = Price::objects().fetch_pg(&pool).await.expect("fetch_pg");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].amount,
        rust_decimal::Decimal::from_str("123.45").unwrap(),
        "the numeric(10,2) value must round-trip exactly"
    );
}
