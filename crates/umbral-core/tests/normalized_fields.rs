//! gaps3 #34 — `#[umbral(trim)]` / `#[umbral(lowercase)]` normalize a column's
//! string value on the DYNAMIC write path (REST `insert_json`/`update_json` +
//! the admin `*_form` paths), exactly like `auto_now`. A field without the
//! attributes is stored verbatim.
//!
//! Behavioural: real rows through the actual public write path, read back to
//! confirm what landed — not an assertion on generated SQL.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::OnceCell;

use umbral::orm::DynQuerySet;
use umbral_core::db;

fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

/// `email` normalizes (trim + lowercase); `display_name` is left verbatim so
/// the test proves the attribute is per-field, not global.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "norm_account")]
pub struct Account {
    pub id: i64,
    #[umbral(trim, lowercase, unique)]
    pub email: String,
    pub display_name: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbral_norm_field_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Account>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

fn meta() -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "norm_account")
        .expect("registered")
}

fn json_body(email: &str, display: &str) -> serde_json::Map<String, Value> {
    let mut b = serde_json::Map::new();
    b.insert("email".to_string(), Value::String(email.to_string()));
    b.insert(
        "display_name".to_string(),
        Value::String(display.to_string()),
    );
    b
}

fn pk(row: &serde_json::Map<String, Value>) -> i64 {
    row.get("id").and_then(Value::as_i64).expect("pk")
}

async fn read(id: i64) -> (String, String) {
    let rows = DynQuerySet::for_meta(&meta())
        .filter_eq_string("id", &id.to_string())
        .fetch_as_strings()
        .await
        .expect("fetch");
    let row = rows.into_iter().next().expect("row exists");
    (
        row.get("email").cloned().unwrap_or_default(),
        row.get("display_name").cloned().unwrap_or_default(),
    )
}

#[tokio::test]
async fn insert_json_normalizes_flagged_field_only() {
    let _guard = test_lock().lock().await;
    boot().await;

    let row = DynQuerySet::for_meta(&meta())
        .insert_json(&json_body("  Dave@Example.COM ", "  Mixed Case Dave  "))
        .await
        .expect("insert");
    let (email, display) = read(pk(&row)).await;

    assert_eq!(email, "dave@example.com", "email is trimmed + lowercased");
    assert_eq!(
        display, "  Mixed Case Dave  ",
        "an un-flagged field is stored verbatim"
    );
}

#[tokio::test]
async fn update_json_normalizes_flagged_field() {
    let _guard = test_lock().lock().await;
    boot().await;

    let row = DynQuerySet::for_meta(&meta())
        .insert_json(&json_body("update-seed@example.com", "seed"))
        .await
        .expect("insert");
    let id = pk(&row);

    DynQuerySet::for_meta(&meta())
        .filter_eq_string("id", &id.to_string())
        .update_json(&json_body("  UPDATED@Example.Com  ", "seed"))
        .await
        .expect("update");
    let (email, _) = read(id).await;

    assert_eq!(
        email, "updated@example.com",
        "update_json normalizes the flagged field too"
    );
}

#[tokio::test]
async fn insert_form_normalizes_flagged_field() {
    let _guard = test_lock().lock().await;
    boot().await;

    // The admin form path (HashMap<String,String>) normalizes identically.
    let mut form = HashMap::new();
    form.insert("email".to_string(), "  Form@Example.COM ".to_string());
    form.insert("display_name".to_string(), "Form User".to_string());

    let pk_val = DynQuerySet::for_meta(&meta())
        .insert_form(&form, &[])
        .await
        .expect("insert_form");
    let (email, _) = read(pk_val).await;

    assert_eq!(
        email, "form@example.com",
        "the admin form path normalizes the flagged field"
    );
}

#[tokio::test]
async fn case_only_duplicate_collides_after_normalization() {
    let _guard = test_lock().lock().await;
    boot().await;

    DynQuerySet::for_meta(&meta())
        .insert_json(&json_body("dupe@example.com", "first"))
        .await
        .expect("first insert");

    // A case/whitespace variant normalizes to the same value → UNIQUE collision,
    // not a second row. This is the case-insensitive-uniqueness payoff.
    let dup = DynQuerySet::for_meta(&meta())
        .insert_json(&json_body("  DUPE@Example.com ", "second"))
        .await;
    assert!(
        dup.is_err(),
        "a case-only-different insert must collide on the UNIQUE email, got {dup:?}"
    );
}
