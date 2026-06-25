// The private test model emits public column constants from `#[derive(Model)]`.
#![allow(dead_code, private_interfaces)]

//! Regression coverage for Postgres-backed `dumpdata` / `loaddata`.
//!
//! The test self-skips unless `UMBRAL_TEST_POSTGRES_URL` points at a
//! writable Postgres database. It lives in its own test binary because
//! `App::build()` publishes process-wide ambient state.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "umbral_backup_pg_record")]
struct PgBackupRecord {
    id: i64,
    name: String,
    count: i32,
    payload: serde_json::Value,
    uid: uuid::Uuid,
    tags: Vec<String>,
    scores: Option<Vec<i64>>,
    addr: Option<ipnetwork::IpNetwork>,
    mac: Option<mac_address::MacAddress>,
    blob: Vec<u8>,
    price: rust_decimal::Decimal,
    created_at: DateTime<Utc>,
}

#[tokio::test]
async fn postgres_dump_and_load_round_trip_core_types() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping Postgres backup test: UMBRAL_TEST_POSTGRES_URL is not set");
        return;
    };

    let pool = PgPool::connect(&url)
        .await
        .expect("connect to UMBRAL_TEST_POSTGRES_URL");

    sqlx::query("DROP TABLE IF EXISTS umbral_backup_pg_record")
        .execute(&pool)
        .await
        .expect("drop prior backup test table");
    sqlx::query(
        "CREATE TABLE umbral_backup_pg_record (\
             id BIGSERIAL PRIMARY KEY, \
             name TEXT NOT NULL, \
             count INTEGER NOT NULL, \
             payload JSONB NOT NULL, \
             uid UUID NOT NULL, \
             tags TEXT[] NOT NULL, \
             scores BIGINT[], \
             addr INET, \
             mac MACADDR, \
             blob BYTEA NOT NULL, \
             price NUMERIC(19, 4) NOT NULL, \
             created_at TIMESTAMPTZ NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("create backup test table");

    let uid = Uuid::parse_str("018f9b9a-4a50-72dc-99f6-341ab7f2a8ef").unwrap();
    let addr = ipnetwork::IpNetwork::from_str("10.0.0.1/24").unwrap();
    let mac = mac_address::MacAddress::from_str("aa:bb:cc:dd:ee:ff").unwrap();
    let price = Decimal::from_str("19.9900").unwrap();
    let created_at = DateTime::parse_from_rfc3339("2026-06-06T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    sqlx::query(
        "INSERT INTO umbral_backup_pg_record \
         (name, count, payload, uid, tags, scores, addr, mac, blob, price, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind("round-trip")
    .bind(42i32)
    .bind(json!({"ok": true, "items": [1, 2, 3]}))
    .bind(uid)
    .bind(vec!["blue".to_string(), "green".to_string()])
    .bind(Some(vec![10i64, 20, 30]))
    .bind(Some(addr))
    .bind(Some(mac))
    .bind(vec![1u8, 2, 255])
    .bind(price)
    .bind(created_at)
    .execute(&pool)
    .await
    .expect("seed backup test row");

    let mut settings = umbral::Settings::from_env().expect("settings defaults load");
    settings.database_url = url;
    let _app = umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<PgBackupRecord>()
        .build()
        .expect("Postgres App build succeeds");

    let dump = umbral::backup::dump()
        .await
        .expect("dump should read from the Postgres pool");
    let table = dump
        .models
        .iter()
        .find(|model| model.table == "umbral_backup_pg_record")
        .expect("dump should include the registered Postgres model");
    assert_eq!(table.rows.len(), 1);
    assert_eq!(
        table.rows[0]["payload"],
        json!({"ok": true, "items": [1, 2, 3]})
    );
    assert_eq!(table.rows[0]["uid"], uid.to_string());
    assert_eq!(table.rows[0]["tags"], json!(["blue", "green"]));
    assert_eq!(table.rows[0]["scores"], json!([10, 20, 30]));
    assert_eq!(table.rows[0]["addr"], addr.to_string());
    assert_eq!(table.rows[0]["mac"], mac.to_string());
    assert_eq!(table.rows[0]["blob"], json!([1, 2, 255]));
    assert_eq!(table.rows[0]["price"], "19.9900");

    sqlx::query("DELETE FROM umbral_backup_pg_record")
        .execute(&pool)
        .await
        .expect("wipe table before load");

    let report = umbral::backup::load(&dump)
        .await
        .expect("load should write through the Postgres pool");
    assert_eq!(report.rows_loaded, 1);
    assert!(
        report
            .tables_loaded
            .contains(&"umbral_backup_pg_record".to_string())
    );

    let row = sqlx::query(
        "SELECT name, count, payload, uid, tags, scores, addr, mac, blob, price, created_at \
         FROM umbral_backup_pg_record",
    )
    .fetch_one(&pool)
    .await
    .expect("load should restore row");

    assert_eq!(row.try_get::<String, _>("name").unwrap(), "round-trip");
    assert_eq!(row.try_get::<i32, _>("count").unwrap(), 42);
    assert_eq!(
        row.try_get::<serde_json::Value, _>("payload").unwrap(),
        json!({"ok": true, "items": [1, 2, 3]})
    );
    assert_eq!(row.try_get::<Uuid, _>("uid").unwrap(), uid);
    assert_eq!(
        row.try_get::<Vec<String>, _>("tags").unwrap(),
        vec!["blue".to_string(), "green".to_string()]
    );
    assert_eq!(
        row.try_get::<Option<Vec<i64>>, _>("scores").unwrap(),
        Some(vec![10, 20, 30])
    );
    assert_eq!(
        row.try_get::<Option<ipnetwork::IpNetwork>, _>("addr")
            .unwrap(),
        Some(addr)
    );
    assert_eq!(
        row.try_get::<Option<mac_address::MacAddress>, _>("mac")
            .unwrap(),
        Some(mac)
    );
    assert_eq!(row.try_get::<Vec<u8>, _>("blob").unwrap(), vec![1, 2, 255]);
    assert_eq!(row.try_get::<Decimal, _>("price").unwrap(), price);
    assert_eq!(
        row.try_get::<DateTime<Utc>, _>("created_at").unwrap(),
        created_at
    );
}
