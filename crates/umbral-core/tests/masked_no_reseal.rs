//! review_3 (Masked re-seal cluster): the dynamic UPDATE path must NOT
//! re-seal a "no change" submission for a `Masked<T>` column. An admin
//! edit renders a masked field as empty (the plaintext is never shown),
//! so an untouched field submits `field=` (empty) or the redaction marker
//! `"••••••"`. Sealing that would overwrite the stored ciphertext with
//! `seal("")` / `seal("••••••")` — crypto-shredding the secret on any
//! unrelated edit. The fix: treat empty / redaction-marker as no-change
//! and omit the column, mirroring the `Masked` Deserialize contract.

use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::ModelMeta;
use umbral::orm::{DynQuerySet, MaskKeyring, Masked, Model, set_mask_keyring};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "mnr_account")]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub api_key: Masked<String>,
}

async fn boot() -> ModelMeta {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let (public, secret) = MaskKeyring::generate();
        set_mask_keyring(MaskKeyring::from_base64(&public, Some(&secret)).expect("keyring"));

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("mnr.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(umbral::Settings::from_env().expect("settings"))
            .database("default", pool)
            .model::<Account>()
            .build()
            .expect("build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create schema");
    })
    .await;
    ModelMeta::for_::<Account>()
}

fn m(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    v.as_object().cloned().expect("json object")
}

async fn stored_secret() -> String {
    Account::objects()
        .first()
        .await
        .expect("query")
        .expect("row")
        .api_key
        .reveal()
        .expect("reveal")
}

/// Editing an unrelated column while the masked field is submitted EMPTY
/// (or as the redaction marker) must preserve the stored secret rather
/// than seal the placeholder over the ciphertext.
#[tokio::test]
async fn no_change_masked_submissions_preserve_the_secret_on_update() {
    let meta = boot().await;

    // Seed a real secret through the dynamic write path (it seals).
    DynQuerySet::for_meta(&meta)
        .insert_json(&m(json!({ "name": "Ada", "api_key": "super-secret-123" })))
        .await
        .expect("insert");
    assert_eq!(stored_secret().await, "super-secret-123", "baseline sealed");

    // Admin edits `name`; the masked field comes back EMPTY (untouched).
    DynQuerySet::for_meta(&meta)
        .filter_eq_string("name", "Ada")
        .update_json(&m(json!({ "name": "Ada Lovelace", "api_key": "" })))
        .await
        .expect("update (empty masked)");
    assert_eq!(
        stored_secret().await,
        "super-secret-123",
        "empty masked submission must be a no-op, not seal(\"\") over the ciphertext"
    );

    // And the redaction marker echoed back is likewise a no-op.
    let redacted = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}"; // "••••••"
    DynQuerySet::for_meta(&meta)
        .filter_eq_string("name", "Ada Lovelace")
        .update_json(&m(json!({ "name": "Grace", "api_key": redacted })))
        .await
        .expect("update (redaction marker)");
    assert_eq!(
        stored_secret().await,
        "super-secret-123",
        "the redaction marker must be treated as no-change"
    );

    // A genuine new secret DOES replace it — the guard only skips no-ops.
    DynQuerySet::for_meta(&meta)
        .filter_eq_string("name", "Grace")
        .update_json(&m(
            json!({ "name": "Grace", "api_key": "rotated-secret-456" }),
        ))
        .await
        .expect("update (real new secret)");
    assert_eq!(
        stored_secret().await,
        "rotated-secret-456",
        "a non-empty masked value still rotates the secret"
    );
}
