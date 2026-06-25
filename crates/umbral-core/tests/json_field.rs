//! End-to-end coverage for the Phase 4 `Json` field type.
//!
//! - The derive recognises `serde_json::Value` as a `Json` field;
//! - `Manager::on(&pool).fetch()` round-trips the value through SQLite
//!   (the portable backend everyone gets in CI);
//! - `IS NULL` / `IS NOT NULL` filters route correctly through
//!   `NullableJsonCol`;
//! - The Postgres-pool surface (`.on_pg(...)`) typechecks against the
//!   same Manager.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "umbral_phase4_json_event")]
pub struct Event {
    pub id: i64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub meta: Option<serde_json::Value>,
}

async fn fresh_pool() -> SqlitePool {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");

    // The DDL is what M5's `render_operation` would emit for the same
    // model. Hand-rolling it keeps the test independent of the
    // migration engine boot path.
    sqlx::query(
        "CREATE TABLE umbral_phase4_json_event (\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\
             kind TEXT NOT NULL,\
             payload TEXT NOT NULL,\
             meta TEXT\
         )",
    )
    .execute(&pool)
    .await
    .expect("create table");

    pool
}

/// The derive emits a `Json` field with the right SqlType.
#[test]
fn derive_classifies_serde_json_value_as_json_sqltype() {
    use umbral::orm::{Model, SqlType};

    let by_name: std::collections::HashMap<&str, &umbral::orm::FieldSpec> = <Event as Model>::FIELDS
        .iter()
        .map(|f| (f.name, f))
        .collect();

    let payload = by_name.get("payload").expect("payload field");
    assert_eq!(payload.ty, SqlType::Json);
    assert!(!payload.nullable);

    let meta = by_name.get("meta").expect("meta field");
    assert_eq!(meta.ty, SqlType::Json);
    assert!(meta.nullable, "Option<Value> is the nullable variant");
}

/// Round-trip: insert a JSON object, fetch it back, structure intact.
#[tokio::test]
async fn json_value_round_trips_through_sqlite() {
    let pool = fresh_pool().await;

    let payload: Value = json!({ "level": "info", "count": 42, "tags": ["a", "b"] });

    sqlx::query("INSERT INTO umbral_phase4_json_event (kind, payload, meta) VALUES (?, ?, ?)")
        .bind("startup")
        .bind(&payload)
        .bind(Option::<Value>::None)
        .execute(&pool)
        .await
        .expect("insert event row");

    let rows = Event::objects()
        .on(&pool)
        .fetch()
        .await
        .expect("fetch events");
    assert_eq!(rows.len(), 1);

    let row = &rows[0];
    assert_eq!(row.kind, "startup");
    assert_eq!(row.payload, payload, "JSON object round-tripped intact");
    assert!(row.meta.is_none(), "nullable meta was NULL");
}

/// `NullableJsonCol::is_null` / `is_not_null` route to the right WHERE
/// fragment and filter against an in-memory dataset.
#[tokio::test]
async fn nullable_json_col_is_null_filters_correctly() {
    use umbral::orm::Model;
    let pool = fresh_pool().await;

    sqlx::query("INSERT INTO umbral_phase4_json_event (kind, payload, meta) VALUES (?, ?, ?)")
        .bind("a")
        .bind(json!({}))
        .bind(Option::<Value>::None)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO umbral_phase4_json_event (kind, payload, meta) VALUES (?, ?, ?)")
        .bind("b")
        .bind(json!({}))
        .bind(Some(json!({ "extra": true })))
        .execute(&pool)
        .await
        .unwrap();

    let with_meta = Event::objects()
        .filter(event::META.is_not_null())
        .on(&pool)
        .fetch()
        .await
        .expect("filter is_not_null");
    assert_eq!(with_meta.len(), 1);
    assert_eq!(with_meta[0].kind, "b");

    let without_meta = Event::objects()
        .filter(event::META.is_null())
        .on(&pool)
        .fetch()
        .await
        .expect("filter is_null");
    assert_eq!(without_meta.len(), 1);
    assert_eq!(without_meta[0].kind, "a");

    // Sanity check the SqlType also pins through Model::FIELDS.
    let payload_spec = <Event as Model>::FIELDS
        .iter()
        .find(|f| f.name == "payload")
        .unwrap();
    assert_eq!(payload_spec.ty, umbral::orm::SqlType::Json);
}

/// Type-level pin: the Postgres pool path accepts the same Event
/// model. The function never runs (the test body is empty); the
/// compiler checks `.on_pg(...)` is reachable through the manager.
#[test]
fn json_model_typechecks_against_pg_pool() {
    #[allow(dead_code)]
    async fn _unreachable(pg_pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
        let _events: Vec<Event> = Event::objects().on_pg(pg_pool).fetch().await?;
        Ok(())
    }
}

/// Regression for BUG-3 in `bugs/db-testing.md`: Postgres bulk
/// inserts must bind `serde_json::Value` through JSON/JSONB, not as
/// plain text parameters.
#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn bulk_create_json_values_round_trip_through_postgres() {
    let url =
        std::env::var("UMBRAL_TEST_POSTGRES_URL").expect("UMBRAL_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    sqlx::query("DROP TABLE IF EXISTS umbral_phase4_json_event")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE umbral_phase4_json_event ( \
            id BIGSERIAL PRIMARY KEY, \
            kind TEXT NOT NULL, \
            payload JSONB NOT NULL, \
            meta JSONB \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let affected = Event::objects()
        .bulk_create_pg(
            vec![
                Event {
                    id: 0,
                    kind: "startup".to_string(),
                    payload: json!({ "level": "info", "count": 42 }),
                    meta: Some(json!({ "source": "bulk" })),
                },
                Event {
                    id: 0,
                    kind: "shutdown".to_string(),
                    payload: json!({ "level": "warn", "count": 1 }),
                    meta: None,
                },
            ],
            &pool,
        )
        .await
        .unwrap();
    assert_eq!(affected, 2);

    let mut rows = Event::objects().fetch_pg(&pool).await.unwrap();
    rows.sort_by_key(|row| row.id);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].payload, json!({ "level": "info", "count": 42 }));
    assert_eq!(rows[0].meta, Some(json!({ "source": "bulk" })));
    assert_eq!(rows[1].payload, json!({ "level": "warn", "count": 1 }));
    assert!(rows[1].meta.is_none());
}
