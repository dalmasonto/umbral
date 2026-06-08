//! Gap #116 — `insert_form` / `update_form` on a JSON column must
//! PARSE the raw form string into a real JSON value before storing.
//!
//! Pre-fix behaviour: `form_str_to_sea_value` wrapped every form
//! value as `JsonValue::String(raw)`. Typing `{"key": "value"}` into
//! a JSON textarea stored the LITERAL TEXT `"{\"key\":\"value\"}"`
//! instead of a parsed object — the round-trip read back came as a
//! string, not as the original object. `{}` came back as the string
//! `"{}"`, breaking `obj["k"]` access for every downstream consumer.
//!
//! Post-fix: SqlType::Json (and Array) columns parse the form
//! string with `serde_json::from_str`. Valid JSON of any shape
//! (object / array / scalar) stores as the parsed value. Invalid
//! JSON surfaces as `WriteError::Validator { field, message:
//! "Not valid JSON: ..." }`, which the admin renders inline rather
//! than swallowing as a generic "database error".

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use umbra::orm::DynQuerySet;
use umbra_core::migrate::ModelMeta;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "jform_event")]
pub struct Event {
    pub id: i64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub meta: Option<serde_json::Value>,
}

async fn fresh_pool() -> SqlitePool {
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    sqlx::query(
        "CREATE TABLE jform_event (\
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

async fn boot() {
    static BOOT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment");
        let pool = fresh_pool().await;
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Event>()
            .build()
            .expect("App::build");
    })
    .await;
}

fn meta() -> ModelMeta {
    ModelMeta::for_::<Event>()
}

// =========================================================================
// Object input → stored + round-trips as an object (not a string).
// This is the headline bug. Pre-fix the round-trip came back as
// JsonValue::String, breaking `obj["k"]`.
// =========================================================================
#[tokio::test]
async fn json_object_form_input_round_trips_as_object() {
    boot().await;
    let mut form = std::collections::HashMap::new();
    form.insert("kind".to_string(), "user_login".to_string());
    form.insert(
        "payload".to_string(),
        r#"{"ip": "10.0.0.1", "count": 3}"#.to_string(),
    );

    let id = DynQuerySet::for_meta(&meta())
        .insert_form(&form, &[])
        .await
        .expect("insert");

    let row = Event::objects()
        .filter(event::ID.eq(id))
        .first()
        .await
        .expect("fetch")
        .expect("present");
    let payload = row.payload;

    // The bug: payload would come back as JsonValue::String(`"..."`),
    // so `is_object()` would be false and indexing would yield None.
    assert!(
        payload.is_object(),
        "REGRESSION: JSON form input round-tripped as {payload:?}; \
         expected an Object. Pre-fix the input was stored as a JSON \
         string literal rather than parsed."
    );
    assert_eq!(payload.get("ip").and_then(|v| v.as_str()), Some("10.0.0.1"));
    assert_eq!(payload.get("count").and_then(|v| v.as_i64()), Some(3));
}

// =========================================================================
// Array input → stored as a JSON array. Same shape regression as
// the object test, different top-level type.
// =========================================================================
#[tokio::test]
async fn json_array_form_input_round_trips_as_array() {
    boot().await;
    let mut form = std::collections::HashMap::new();
    form.insert("kind".to_string(), "tags".to_string());
    form.insert("payload".to_string(), r#"["a", "b", "c"]"#.to_string());

    let id = DynQuerySet::for_meta(&meta())
        .insert_form(&form, &[])
        .await
        .expect("insert");
    let row = Event::objects()
        .filter(event::ID.eq(id))
        .first()
        .await
        .expect("fetch")
        .expect("present");
    let arr = row
        .payload
        .as_array()
        .expect("payload comes back as array, not string");
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0].as_str(), Some("a"));
}

// =========================================================================
// Empty object `{}` is parsed (not stored as the string "{}").
// Common case: user opens the form, accepts the default placeholder,
// hits save. Pre-fix this stored `"{}"` as a string; obj["k"] would
// always be None even when later code wrote real keys.
// =========================================================================
#[tokio::test]
async fn empty_object_form_input_parses_to_empty_object() {
    boot().await;
    let mut form = std::collections::HashMap::new();
    form.insert("kind".to_string(), "blank".to_string());
    form.insert("payload".to_string(), "{}".to_string());

    let id = DynQuerySet::for_meta(&meta())
        .insert_form(&form, &[])
        .await
        .expect("insert");
    let row = Event::objects()
        .filter(event::ID.eq(id))
        .first()
        .await
        .expect("fetch")
        .expect("present");
    assert!(row.payload.is_object(), "got: {:?}", row.payload);
    assert_eq!(row.payload.as_object().unwrap().len(), 0);
}

// =========================================================================
// Invalid JSON surfaces a loud, actionable error mentioning the
// field name + the parser's reason. Pre-fix this would store
// JsonValue::String of the garbage input — a silent failure with
// data corruption downstream.
// =========================================================================
#[tokio::test]
async fn invalid_json_form_input_errors_loudly_with_field_name() {
    boot().await;
    let mut form = std::collections::HashMap::new();
    form.insert("kind".to_string(), "broken".to_string());
    form.insert("payload".to_string(), "{not valid json".to_string());

    let err = DynQuerySet::for_meta(&meta())
        .insert_form(&form, &[])
        .await
        .expect_err("invalid JSON must error");
    let msg = err.to_string();
    assert!(
        msg.contains("payload"),
        "error must name the offending field: {msg}"
    );
    assert!(
        msg.contains("Not valid JSON"),
        "error must explain the failure mode: {msg}"
    );
}

// =========================================================================
// A bare JSON scalar (`42`, `"text"`, `true`, `null`) is also valid
// JSON and should parse — JSON columns can hold any JSON value, not
// just objects.
// =========================================================================
#[tokio::test]
async fn json_scalar_form_input_parses() {
    boot().await;
    let mut form = std::collections::HashMap::new();
    form.insert("kind".to_string(), "scalar".to_string());
    form.insert("payload".to_string(), "42".to_string());
    let id = DynQuerySet::for_meta(&meta())
        .insert_form(&form, &[])
        .await
        .expect("insert");
    let row = Event::objects()
        .filter(event::ID.eq(id))
        .first()
        .await
        .expect("fetch")
        .expect("present");
    assert_eq!(row.payload, json!(42));
}
