//! Behavioral round-trip for the `Masked<T>` encrypt-at-rest field.
//!
//! Declares a model with both a non-null and a nullable masked column,
//! creates a row through the public ORM path, reads the raw column value
//! back to prove it's *ciphertext* (not the plaintext), then reveals it
//! through the loaded object and confirms the plaintext survives the
//! encrypt → store → load → decrypt trip.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::orm::{MaskKeyring, Masked, set_mask_keyring};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "masked_secret")]
pub struct Secret {
    pub id: i64,
    pub label: String,
    /// Non-null masked column — always encrypted.
    pub api_key: Masked<String>,
    /// Nullable masked column — `None` stays NULL, `Some` is encrypted.
    pub recovery_code: Option<Masked<String>>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        // A known keypair for the whole test binary so every test in this
        // file seals/opens against the same keyring.
        let (public, secret) = MaskKeyring::generate();
        set_mask_keyring(MaskKeyring::from_base64(&public, Some(&secret)).unwrap());

        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("masked_roundtrip.sqlite");
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
            .settings(settings)
            .database("default", pool)
            .model::<Secret>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

#[tokio::test]
async fn masked_column_stores_ciphertext_and_reveals_plaintext() {
    boot().await;

    let created = Secret::objects()
        .create(Secret {
            id: 0,
            label: "stripe".to_string(),
            api_key: Masked::new("sk_live_SECRET_VALUE"),
            recovery_code: Some(Masked::new("8675309")),
        })
        .await
        .expect("create");

    // The raw stored column must NOT be the plaintext.
    let pool = umbral::db::pool();
    let raw: String = sqlx::query_scalar("SELECT api_key FROM masked_secret WHERE id = ?")
        .bind(created.id)
        .fetch_one(&pool)
        .await
        .expect("raw select");
    assert_ne!(
        raw, "sk_live_SECRET_VALUE",
        "stored column must be ciphertext, not plaintext"
    );
    assert!(
        !raw.contains("SECRET_VALUE"),
        "plaintext must not appear anywhere in the stored ciphertext"
    );

    // Loading the row and revealing returns the original plaintext.
    let loaded = Secret::objects()
        .filter(secret::ID.eq(created.id))
        .first()
        .await
        .expect("query")
        .expect("row exists");
    assert_eq!(loaded.api_key.reveal().unwrap(), "sk_live_SECRET_VALUE");
    assert_eq!(
        loaded.recovery_code.as_ref().unwrap().reveal().unwrap(),
        "8675309"
    );
}

#[tokio::test]
async fn nullable_masked_column_keeps_none_as_null() {
    boot().await;

    let created = Secret::objects()
        .create(Secret {
            id: 0,
            label: "no-recovery".to_string(),
            api_key: Masked::new("k"),
            recovery_code: None,
        })
        .await
        .expect("create");

    let pool = umbral::db::pool();
    let raw: Option<String> =
        sqlx::query_scalar("SELECT recovery_code FROM masked_secret WHERE id = ?")
            .bind(created.id)
            .fetch_one(&pool)
            .await
            .expect("raw select");
    assert_eq!(raw, None, "a None masked field stays SQL NULL");

    let loaded = Secret::objects()
        .filter(secret::ID.eq(created.id))
        .first()
        .await
        .expect("query")
        .expect("row exists");
    assert!(loaded.recovery_code.is_none());
}

#[tokio::test]
async fn dynamic_insert_json_seals_masked_column() {
    boot().await;

    // The DYNAMIC write path — exactly what REST create and admin form-submit
    // use — must SEAL a masked column too. A plaintext JSON string handed to
    // `insert_json` must land as ciphertext, not plaintext (audit_2 core-orm C1).
    let meta = umbral::migrate::model_meta_for_table("masked_secret").expect("meta");
    let body = serde_json::json!({ "label": "dyn", "api_key": "plaintext-via-json" });
    umbral::orm::DynQuerySet::for_meta(&meta)
        .insert_json(body.as_object().unwrap())
        .await
        .expect("insert_json");

    let pool = umbral::db::pool();
    let raw: String = sqlx::query_scalar("SELECT api_key FROM masked_secret WHERE label = 'dyn'")
        .fetch_one(&pool)
        .await
        .expect("raw select");
    assert_ne!(
        raw, "plaintext-via-json",
        "dynamic insert_json must seal a masked column, not store plaintext"
    );
    assert!(
        !raw.contains("plaintext-via-json"),
        "plaintext must not appear anywhere in the stored ciphertext"
    );

    // ...and it is valid ciphertext: the typed load reveals the original.
    let loaded = Secret::objects()
        .filter(secret::LABEL.eq("dyn"))
        .first()
        .await
        .expect("query")
        .expect("row exists");
    assert_eq!(loaded.api_key.reveal().unwrap(), "plaintext-via-json");
}

#[tokio::test]
async fn empty_form_value_leaves_masked_ciphertext_unchanged() {
    // gaps4 #5 — the admin edit crypto-shred. The admin renders a Masked column
    // as a blank editable field, so a save carries `api_key=""`. That empty
    // string used to be SEALED and written, destroying the real secret on every
    // edit. An empty (or redaction-marker) submit must mean "leave unchanged".
    boot().await;

    let created = Secret::objects()
        .create(Secret {
            id: 0,
            label: "shred-test".to_string(),
            api_key: Masked::new("sk_live_KEEP_ME"),
            recovery_code: None,
        })
        .await
        .expect("create");

    let meta = umbral::migrate::model_meta_for_table("masked_secret").expect("meta");

    // An admin form save that touches `label` and carries an EMPTY api_key —
    // exactly the shape the edit form submits for a redacted Masked field.
    let mut form = std::collections::HashMap::new();
    form.insert("label".to_string(), "shred-test-renamed".to_string());
    form.insert("api_key".to_string(), String::new());
    umbral::orm::DynQuerySet::for_meta(&meta)
        .filter_eq_string("id", &created.id.to_string())
        .update_form(&form, &[])
        .await
        .expect("update_form");

    let loaded = Secret::objects()
        .filter(secret::ID.eq(created.id))
        .first()
        .await
        .expect("query")
        .expect("row exists");
    assert_eq!(
        loaded.label, "shred-test-renamed",
        "the non-masked field the user actually edited should be saved"
    );
    assert_eq!(
        loaded.api_key.reveal().unwrap(),
        "sk_live_KEEP_ME",
        "an empty masked form value must NOT overwrite the stored secret"
    );
}

#[tokio::test]
async fn presealed_insert_stores_ciphertext_verbatim_not_double_sealed() {
    // gaps4 #2 — the dumpdata->loaddata double-seal. A backup dump writes the
    // sealed ciphertext (via unredacted_for_backup); a restore must store it
    // VERBATIM. Sealing it again yields seal(ciphertext), and reveal() then
    // returns the inner ciphertext, silently losing the plaintext.
    boot().await;

    // Create a row so we have a real sealed ciphertext to "restore".
    let created = Secret::objects()
        .create(Secret {
            id: 0,
            label: "backup-src".to_string(),
            api_key: Masked::new("sk_live_RESTORE_ME"),
            recovery_code: None,
        })
        .await
        .expect("create");

    // What a dump emits: the stored ciphertext, read unredacted.
    let meta = umbral::migrate::model_meta_for_table("masked_secret").expect("meta");
    let dumped = umbral::orm::DynQuerySet::for_meta(&meta)
        .unredacted_for_backup()
        .filter_eq_string("id", &created.id.to_string())
        .fetch_as_json()
        .await
        .expect("dump")
        .into_iter()
        .next()
        .expect("row");
    let ciphertext = dumped.get("api_key").and_then(|v| v.as_str()).expect("ct");

    // Restore it as a NEW row through the presealed insert path (what
    // load_fixture now uses), then reveal.
    let restore = serde_json::json!({ "label": "backup-dst", "api_key": ciphertext });
    umbral::orm::DynQuerySet::for_meta(&meta)
        .presealed()
        .insert_json(restore.as_object().unwrap())
        .await
        .expect("presealed insert");

    let loaded = Secret::objects()
        .filter(secret::LABEL.eq("backup-dst"))
        .first()
        .await
        .expect("query")
        .expect("row exists");
    assert_eq!(
        loaded.api_key.reveal().unwrap(),
        "sk_live_RESTORE_ME",
        "a presealed restore must round-trip the plaintext, not double-encrypt it"
    );
}
