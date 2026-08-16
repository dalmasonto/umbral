//! `DynQuerySet::revealed()` / `.reveal([..])` — the authorized-audience
//! read switch. A hidden column (`private` / `secret` / `Masked<T>`) is
//! stripped by default; a revealed one is included, and a `Masked<T>` one
//! is DECRYPTED to plaintext in the output (not the stored ciphertext).
//!
//! Behavioural: a real row through the same `DynQuerySet` JSON path REST /
//! GraphQL / admin all sit on. The thing worth proving is that the bytes
//! come back ONLY when revealed, and as plaintext for Masked.

use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::ModelMeta;
use umbral::orm::{DynQuerySet, MaskKeyring, Masked, Model, set_mask_keyring};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "reveal_account")]
pub struct Account {
    pub id: i64,
    pub name: String,
    #[umbral(private)]
    pub cost: String,
    pub api_key: Masked<String>,
}

async fn boot() -> ModelMeta {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let (public, secret) = MaskKeyring::generate();
        set_mask_keyring(MaskKeyring::from_base64(&public, Some(&secret)).expect("keyring"));

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("reveal.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
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
            .expect("schema");

        // Seed a row through the dynamic write path so api_key is sealed.
        DynQuerySet::for_meta(&ModelMeta::for_::<Account>())
            .insert_json(
                json!({ "name": "Ada", "cost": "42.00", "api_key": "sk_live_SECRET" })
                    .as_object()
                    .unwrap(),
            )
            .await
            .expect("seed");
    })
    .await;
    ModelMeta::for_::<Account>()
}

/// Default read strips the private column and the masked column.
#[tokio::test]
async fn default_read_hides_private_and_masked() {
    let meta = boot().await;
    let rows = DynQuerySet::for_meta(&meta)
        .fetch_as_json()
        .await
        .expect("fetch");
    let row = &rows[0];
    assert_eq!(row["name"], json!("Ada"));
    assert!(
        row.get("cost").is_none(),
        "private `cost` must be stripped by default"
    );
    assert!(
        row.get("api_key").is_none(),
        "masked `api_key` (secret) must be stripped by default"
    );
}

/// `.reveal(["cost", "api_key"])` includes the private column and DECRYPTS
/// the masked column to plaintext.
#[tokio::test]
async fn reveal_specific_cols_includes_and_decrypts() {
    let meta = boot().await;
    let rows = DynQuerySet::for_meta(&meta)
        .reveal(&["cost", "api_key"])
        .fetch_as_json()
        .await
        .expect("fetch");
    let row = &rows[0];
    assert_eq!(
        row["cost"],
        json!("42.00"),
        "revealed private column present"
    );
    assert_eq!(
        row["api_key"],
        json!("sk_live_SECRET"),
        "revealed masked column decrypted to plaintext, not ciphertext"
    );
}

/// `.revealed()` reveals everything; and a non-revealed masked read (via
/// unredacted, e.g. backup) would carry ciphertext — `.revealed()` carries
/// plaintext. Here we assert `.revealed()` decrypts.
#[tokio::test]
async fn revealed_all_decrypts_masked() {
    let meta = boot().await;
    let rows = DynQuerySet::for_meta(&meta)
        .revealed()
        .fetch_as_json()
        .await
        .expect("fetch");
    let row = &rows[0];
    assert_eq!(row["api_key"], json!("sk_live_SECRET"));
    assert_eq!(row["cost"], json!("42.00"));
}
