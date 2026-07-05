//! audit_2 core-app-config #10 — `#[umbral(signal_skip)]` strips a field from
//! the ORM signal payloads that fan out to every subscriber, so secrets / PII
//! (password hashes, tokens) don't leak into an audit-log subscriber that
//! logs or persists the payload.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral_core::signals::{clear_for_tests, subscribe};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "secretrow")]
pub struct SecretRow {
    pub id: i64,
    pub name: String,
    /// Sensitive — must never reach a signal subscriber.
    #[umbral(signal_skip)]
    pub secret: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("signal_skip.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<SecretRow>()
            .build()
            .expect("App::build");
        sqlx::query(
            "CREATE TABLE secretrow (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL,\
                secret TEXT NOT NULL\
             )",
        )
        .execute(&umbral::db::pool())
        .await
        .expect("CREATE TABLE secretrow");
    })
    .await;
}

#[test]
fn the_const_lists_the_marked_field() {
    assert_eq!(
        <SecretRow as umbral::orm::Model>::SIGNAL_SKIP_FIELDS,
        &["secret"],
        "the derive must lower #[umbral(signal_skip)] into SIGNAL_SKIP_FIELDS"
    );
}

#[tokio::test]
async fn signal_payload_omits_the_skipped_field() {
    boot().await;
    clear_for_tests();

    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let c = captured.clone();
    subscribe("post_save:secretrow", move |payload| {
        *c.lock().unwrap() = Some(payload.clone());
    });

    // `.save()` fires the single-row `post_save` (full-instance payload); the
    // `.create()` bulk path carries only PKs, so there's nothing to strip there.
    SecretRow::objects()
        .save(SecretRow {
            id: 0,
            name: "visible".into(),
            secret: "topsecret".into(),
        })
        .await
        .expect("save secretrow");

    let payload = captured.lock().unwrap().clone().expect("post_save fired");
    let instance = &payload["instance"];

    // Non-sensitive fields still fan out...
    assert_eq!(
        instance["name"], "visible",
        "non-skipped fields must remain"
    );
    assert!(
        instance.get("id").is_some(),
        "the PK must remain in the payload"
    );

    // ...but the signal_skip field is gone entirely (not null — absent).
    assert!(
        instance.get("secret").is_none(),
        "the #[umbral(signal_skip)] field must be stripped from the payload; got {instance}"
    );
    // And its value never appears anywhere in the serialized payload.
    assert!(
        !serde_json::to_string(&payload)
            .unwrap()
            .contains("topsecret"),
        "the secret value must not appear anywhere in the signal payload"
    );
}
