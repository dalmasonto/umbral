#![allow(dead_code, private_interfaces)]

//! Regression coverage for SQLite JSON backup shape.
//!
//! BUG-4 in `bugs/db-testing.md` reported SQLite `dumpdata` writing
//! JSON columns as stringified JSON (`"{}"`) instead of preserving the
//! object/array value shape (`{}`). This test owns a tiny registry so
//! it can exercise the public `dump`/`load` APIs without sharing the
//! tables used by `tests/backup.rs`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use umbral::backup::{dump, load};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "backup_json_doc")]
struct BackupJsonDoc {
    id: i64,
    payload: serde_json::Value,
    meta: Option<serde_json::Value>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<BackupJsonDoc>()
            .build()
            .expect("App::build should succeed");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS backup_json_doc (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                payload TEXT NOT NULL,\
                meta TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create backup_json_doc");
    })
    .await;
}

#[tokio::test]
async fn dump_preserves_sqlite_json_values_as_json() {
    boot().await;
    let pool = umbral::db::pool();

    sqlx::query("DELETE FROM backup_json_doc")
        .execute(&pool)
        .await
        .expect("clean backup_json_doc");

    // Seed JSON as plain SQLite TEXT to mirror existing rows written
    // before the typed JSON binder fix. The dump path should parse
    // the column according to the model's SqlType, not preserve it as
    // a JSON string.
    sqlx::query("INSERT INTO backup_json_doc (payload, meta) VALUES (?, ?), (?, ?)")
        .bind(r#"{"feature":"dump","nested":{"ok":true}}"#)
        .bind(r#"[1,2,3]"#)
        .bind(r#"["array-root"]"#)
        .bind(None::<String>)
        .execute(&pool)
        .await
        .expect("seed JSON rows");

    let dumped = dump().await.expect("dump should succeed");
    let model = dumped
        .models
        .iter()
        .find(|model| model.table == "backup_json_doc")
        .expect("backup_json_doc should be dumped");
    assert_eq!(model.rows.len(), 2);

    let object_row = model
        .rows
        .iter()
        .find(|row| row["payload"]["feature"] == json!("dump"))
        .expect("object payload row should be present");
    assert_eq!(
        object_row["payload"],
        json!({ "feature": "dump", "nested": { "ok": true } })
    );
    assert_eq!(object_row["meta"], json!([1, 2, 3]));
    assert!(
        !object_row["payload"].is_string(),
        "payload should be a JSON object in the dump, not a string"
    );
    assert!(
        !object_row["meta"].is_string(),
        "nullable JSON value should be a JSON array in the dump, not a string"
    );

    let array_row = model
        .rows
        .iter()
        .find(|row| row["payload"] == json!(["array-root"]))
        .expect("array payload row should be present");
    assert!(array_row["meta"].is_null());

    sqlx::query("DELETE FROM backup_json_doc")
        .execute(&pool)
        .await
        .expect("wipe before load");

    let report = load(&dumped).await.expect("load should succeed");
    assert_eq!(report.rows_loaded, 2);

    let round_tripped: Vec<(Value, Option<Value>)> =
        sqlx::query_as("SELECT payload, meta FROM backup_json_doc ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("select loaded JSON rows");
    assert_eq!(round_tripped.len(), 2);
    assert_eq!(
        round_tripped[0],
        (
            json!({ "feature": "dump", "nested": { "ok": true } }),
            Some(json!([1, 2, 3]))
        )
    );
    assert_eq!(round_tripped[1], (json!(["array-root"]), None));
}
