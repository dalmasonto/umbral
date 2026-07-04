//! audit_2 H3 — a `#[umbral(privileged)]` column is a default-DENY
//! mass-assignment guard on the untrusted dynamic write paths
//! (`insert_json`/`update_json` + the admin `*_form` paths). The client can't
//! set it by smuggling it into the body; a caller that has verified the
//! requester is authorized opts it back in with `DynQuerySet::allow_privileged`.
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

/// Serialise the tests: they share one file-backed SQLite DB, and parallel
/// writers race on SQLite's file lock. Same pattern as `tests/dyn_signals.rs`.
fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

/// `is_admin` is privileged: an untrusted create/update must not be able to set
/// it. `default = "false"` mirrors the built-in `AuthUser` shape so a stripped
/// INSERT fills the safe value at the DB instead of tripping NOT NULL.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "priv_account")]
pub struct Account {
    pub id: i64,
    #[umbral(string)]
    pub username: String,
    #[umbral(privileged, default = "false")]
    pub is_admin: bool,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbral_priv_field_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Account>()
            .build()
            .expect("App::build");
        sqlx::query(
            "CREATE TABLE priv_account (
                id       INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL,
                is_admin BOOLEAN NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE");
    })
    .await;
}

fn meta() -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "priv_account")
        .expect("registered")
}

fn body(username: &str, is_admin: bool) -> serde_json::Map<String, Value> {
    let mut b = serde_json::Map::new();
    b.insert("username".to_string(), Value::String(username.to_string()));
    b.insert("is_admin".to_string(), Value::Bool(is_admin));
    b
}

async fn admin_flag(id: i64) -> bool {
    let rows = DynQuerySet::for_meta(&meta())
        .filter_eq_string("id", &id.to_string())
        .fetch_as_strings()
        .await
        .expect("fetch");
    let row = rows.into_iter().next().expect("row exists");
    // SQLite stores bool as 0/1 text.
    matches!(
        row.get("is_admin").map(String::as_str),
        Some("1") | Some("true")
    )
}

fn pk(row: &serde_json::Map<String, Value>) -> i64 {
    row.get("id").and_then(Value::as_i64).expect("pk")
}

#[tokio::test]
async fn insert_json_strips_privileged_by_default() {
    let _guard = test_lock().lock().await;
    boot().await;

    // Attacker smuggles `is_admin: true` into an unauthorized create.
    let row = DynQuerySet::for_meta(&meta())
        .insert_json(&body("mallory", true))
        .await
        .expect("insert");

    // The privileged column was stripped → DB default `false` won, NOT the
    // client's `true`. No self-service privilege escalation.
    assert!(
        !admin_flag(pk(&row)).await,
        "unauthorized create must not set the privileged is_admin flag"
    );
}

#[tokio::test]
async fn insert_json_honors_privileged_when_authorized() {
    let _guard = test_lock().lock().await;
    boot().await;

    // A caller that verified the requester may set is_admin opts it in.
    let row = DynQuerySet::for_meta(&meta())
        .allow_privileged(&["is_admin"])
        .insert_json(&body("root", true))
        .await
        .expect("insert");

    assert!(
        admin_flag(pk(&row)).await,
        "authorized create must set the privileged flag through"
    );
}

#[tokio::test]
async fn update_json_strips_privileged_by_default() {
    let _guard = test_lock().lock().await;
    boot().await;

    // Seed an admin via an authorized create.
    let row = DynQuerySet::for_meta(&meta())
        .allow_privileged(&["is_admin"])
        .insert_json(&body("victim", true))
        .await
        .expect("insert");
    let id = pk(&row);
    assert!(admin_flag(id).await);

    // An unauthorized update tries to FLIP the flag (and would equally be
    // blocked trying to grant it). The privileged column is stripped, so the
    // existing value is untouched — the attacker can neither grant nor revoke.
    DynQuerySet::for_meta(&meta())
        .filter_eq_string("id", &id.to_string())
        .update_json(&body("victim", false))
        .await
        .expect("update");

    assert!(
        admin_flag(id).await,
        "unauthorized update must not change the privileged flag"
    );
}

#[tokio::test]
async fn update_json_honors_privileged_when_authorized() {
    let _guard = test_lock().lock().await;
    boot().await;

    let row = DynQuerySet::for_meta(&meta())
        .insert_json(&body("promote_me", false))
        .await
        .expect("insert");
    let id = pk(&row);
    assert!(!admin_flag(id).await);

    // Authorized update grants the flag.
    DynQuerySet::for_meta(&meta())
        .filter_eq_string("id", &id.to_string())
        .allow_privileged(&["is_admin"])
        .update_json(&body("promote_me", true))
        .await
        .expect("update");

    assert!(
        admin_flag(id).await,
        "authorized update must set the privileged flag through"
    );
}

#[tokio::test]
async fn insert_form_strips_privileged_by_default() {
    let _guard = test_lock().lock().await;
    boot().await;

    // The admin FORM path (string map) must apply the same guard as JSON.
    let mut form: HashMap<String, String> = HashMap::new();
    form.insert("username".to_string(), "form_mallory".to_string());
    form.insert("is_admin".to_string(), "true".to_string());

    let mut tx = db::begin().await.expect("tx");
    let new_pk = DynQuerySet::for_meta(&meta())
        .insert_form_in_tx(&mut tx, &form, &[])
        .await
        .expect("insert form");
    tx.commit().await.expect("commit");

    assert!(
        !admin_flag(new_pk).await,
        "unauthorized form create must not set the privileged flag"
    );
}

#[tokio::test]
async fn insert_form_honors_privileged_when_authorized() {
    let _guard = test_lock().lock().await;
    boot().await;

    let mut form: HashMap<String, String> = HashMap::new();
    form.insert("username".to_string(), "form_root".to_string());
    form.insert("is_admin".to_string(), "true".to_string());

    let mut tx = db::begin().await.expect("tx");
    let new_pk = DynQuerySet::for_meta(&meta())
        .allow_privileged(&["is_admin"])
        .insert_form_in_tx(&mut tx, &form, &[])
        .await
        .expect("insert form");
    tx.commit().await.expect("commit");

    assert!(
        admin_flag(new_pk).await,
        "authorized form create must set the privileged flag through"
    );
}
