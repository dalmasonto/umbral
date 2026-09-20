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

use umbral::orm::DynQuerySet;
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
                    .busy_timeout(std::time::Duration::from_secs(5))
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
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

/// Serialises the async tests in this file — they all share the process-
/// global signal registry (`clear_for_tests()`) and the same `SecretRow`
/// table via `boot()`'s shared `OnceCell` pool, so running them
/// concurrently races one test's `clear_for_tests()` / subscribe against
/// another's in-flight save/delete.
fn test_lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
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
    let _g = test_lock().lock().await;
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

fn secretrow_meta() -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "secretrow")
        .expect("registered")
}

/// gaps6 #14 follow-up — CRITICAL: the full-row `post_delete` payload the
/// typed `QuerySet::delete()` emits when subscribed (gaps6 #14) MUST still
/// honor `#[umbral(signal_skip)]`, exactly like `save()`'s payload does.
/// Before the fix, the full row came from a raw `row_to_json` decode that
/// bypassed `serialize_for_signal`'s redaction entirely — a real secret leak
/// (`AuthUser.password_hash`) reachable by any `post_delete:<table>`
/// subscriber, including `RealtimePlugin` → WebSocket clients.
#[tokio::test]
async fn delete_post_delete_payload_omits_the_skipped_field_typed_path() {
    let _g = test_lock().lock().await;
    boot().await;
    clear_for_tests();

    let row = SecretRow::objects()
        .save(SecretRow {
            id: 0,
            name: "visible-typed".into(),
            secret: "topsecret-typed".into(),
        })
        .await
        .expect("save secretrow");

    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let c = captured.clone();
    subscribe("post_delete:secretrow", move |payload| {
        *c.lock().unwrap() = Some(payload.clone());
    });

    SecretRow::objects()
        .filter(secret_row::ID.eq(row.id))
        .delete()
        .await
        .expect("delete secretrow");

    let payload = captured.lock().unwrap().clone().expect("post_delete fired");
    let instance = &payload["instance"];

    assert_eq!(
        instance["name"], "visible-typed",
        "non-skipped fields must remain in the full-row post_delete payload"
    );
    assert!(
        instance.get("secret").is_none(),
        "the #[umbral(signal_skip)] field must be stripped from the full-row \
         post_delete payload too; got {instance}"
    );
    assert!(
        !serde_json::to_string(&payload)
            .unwrap()
            .contains("topsecret-typed"),
        "the secret value must not appear anywhere in the delete signal payload"
    );
}

/// gaps6 #14 follow-up — same redaction contract on the DYNAMIC delete path
/// (`DynQuerySet::delete()`), the one REST and the admin actually run
/// writes through. `ModelMeta` has no typed `SIGNAL_SKIP_FIELDS` const to
/// read, so this proves the runtime-threaded `ModelMeta::signal_skip_fields`
/// strips it too.
#[tokio::test]
async fn delete_post_delete_payload_omits_the_skipped_field_dyn_path() {
    let _g = test_lock().lock().await;
    boot().await;
    clear_for_tests();

    let row = SecretRow::objects()
        .save(SecretRow {
            id: 0,
            name: "visible-dyn".into(),
            secret: "topsecret-dyn".into(),
        })
        .await
        .expect("save secretrow");

    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let c = captured.clone();
    subscribe("post_delete:secretrow", move |payload| {
        *c.lock().unwrap() = Some(payload.clone());
    });

    let n = DynQuerySet::for_meta(&secretrow_meta())
        .filter_eq_string("id", &row.id.to_string())
        .delete()
        .await
        .expect("dyn delete secretrow");
    assert_eq!(n, 1);

    let payload = captured.lock().unwrap().clone().expect("post_delete fired");
    let instance = &payload["instance"];

    assert_eq!(
        instance["name"], "visible-dyn",
        "non-skipped fields must remain in the dyn-path full-row post_delete payload"
    );
    assert!(
        instance.get("secret").is_none(),
        "the #[umbral(signal_skip)] field must be stripped on the dyn delete \
         path too; got {instance}"
    );
    assert!(
        !serde_json::to_string(&payload)
            .unwrap()
            .contains("topsecret-dyn"),
        "the secret value must not appear anywhere in the dyn delete signal payload"
    );
}
