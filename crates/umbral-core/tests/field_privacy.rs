//! The read-side field policy: `#[umbral(private)]`, `#[umbral(secret)]`, and `Masked<T>`.
//!
//! These are behavioural tests against a real table with real rows, driven through the same
//! `DynQuerySet` JSON path that REST, GraphQL and the admin all sit on. Asserting that a
//! `Column` carries `secret: true` would prove only that a bool made it through a macro; the
//! thing worth proving is that the *bytes do not come back*.

use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::ModelMeta;
use umbral::orm::{DynQuerySet, MaskKeyring, Masked, Model, set_mask_keyring};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "fp_product")]
pub struct FpProduct {
    pub id: i64,
    pub name: String,
    /// Confidential, but staff legitimately see it — so it is unlockable.
    #[umbral(private)]
    pub cost: String,
    /// Never leaves the process, for anyone.
    #[umbral(secret)]
    pub signing_key: String,
    /// Encrypted at rest. Gets `secret` with NO annotation at all — that is the point.
    pub api_token: Masked<String>,
    /// Not on the model's denylist anywhere; caught by name, because a model author who
    /// forgets the annotation is exactly who this protects.
    pub password_hash: String,
}

async fn boot() -> ModelMeta {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        // `Masked<T>` seals on write, so the test needs a keyring — the point of the
        // api_token column here is that an ENCRYPTED field is `secret` without anyone
        // annotating it, and proving that requires actually writing one.
        let (public, secret) = MaskKeyring::generate();
        set_mask_keyring(MaskKeyring::from_base64(&public, Some(&secret)).expect("keyring"));

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("fp.sqlite");
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
            .model::<FpProduct>()
            .build()
            .expect("build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let p = umbral::db::pool();
        sqlx::query(
            "INSERT INTO fp_product (name, cost, signing_key, api_token, password_hash) VALUES \
             ('Widget', '4.20', 'sk_signing_do_not_leak', 'enc:deadbeef', '$argon2id$leak')",
        )
        .execute(&p)
        .await
        .expect("seed the product row");
    })
    .await;
    ModelMeta::for_::<FpProduct>()
}

/// The baseline: a normal read hands back the ordinary columns and NONE of the protected
/// ones. Four different reasons for four different columns, one uniform outcome.
#[tokio::test]
async fn a_plain_read_returns_no_protected_column() {
    let meta = boot().await;
    let rows = DynQuerySet::for_meta(&meta)
        .fetch_as_json()
        .await
        .expect("fetch");
    let row = &rows[0];

    assert_eq!(
        row["name"],
        json!("Widget"),
        "ordinary columns still come back"
    );

    assert!(!row.contains_key("cost"), "private: {row:?}");
    assert!(!row.contains_key("signing_key"), "secret: {row:?}");
    assert!(
        !row.contains_key("api_token"),
        "Masked<T> is secret with no annotation — that is the whole point: {row:?}"
    );
    assert!(
        !row.contains_key("password_hash"),
        "caught by the core name denylist even though the model never said so: {row:?}"
    );

    // Absent, not null. A key present with a null value still tells the reader the column
    // exists, and would round-trip into an UPDATE that blanks it.
    assert!(
        row.get("cost").is_none() && row.get("signing_key").is_none(),
        "protected columns must be ABSENT, not null: {row:?}"
    );
}

/// `private` is *unlockable* — that is what separates it from `secret`. Staff can see cost.
#[tokio::test]
async fn allow_private_unlocks_exactly_what_it_names() {
    let meta = boot().await;
    let row = DynQuerySet::for_meta(&meta)
        .allow_private(&["cost"])
        .first_as_json()
        .await
        .expect("fetch")
        .expect("row");

    assert_eq!(
        row["cost"],
        json!("4.20"),
        "the unlock must actually unlock"
    );

    // ...and unlocks NOTHING else. An unlock that widens beyond what it names is how a
    // staff endpoint quietly starts serving signing keys.
    assert!(!row.contains_key("signing_key"), "{row:?}");
    assert!(!row.contains_key("password_hash"), "{row:?}");
}

/// **The tier with no escape hatch.** Naming a `secret` column in `allow_private` does
/// nothing — deliberately. If this ever starts passing, the tier is gone.
#[tokio::test]
async fn allow_private_cannot_unlock_a_secret() {
    let meta = boot().await;
    let row = DynQuerySet::for_meta(&meta)
        .allow_private(&["signing_key", "api_token", "password_hash"])
        .first_as_json()
        .await
        .expect("fetch")
        .expect("row");

    assert!(
        !row.contains_key("signing_key")
            && !row.contains_key("api_token")
            && !row.contains_key("password_hash"),
        "`secret` has no unlock; allow_private must not be one: {row:?}"
    );
}

/// An explicit `select_cols` asking for a protected column does not get it either.
///
/// Otherwise the policy would be a *default* rather than a rule, and any caller who spelled
/// the column name out loud would walk straight past it.
#[tokio::test]
async fn naming_a_protected_column_in_select_cols_does_not_defeat_the_policy() {
    let meta = boot().await;
    let row = DynQuerySet::for_meta(&meta)
        .select_cols(&["name".into(), "cost".into(), "password_hash".into()])
        .first_as_json()
        .await
        .expect("fetch")
        .expect("row");

    assert_eq!(row["name"], json!("Widget"));
    assert!(!row.contains_key("cost"), "{row:?}");
    assert!(!row.contains_key("password_hash"), "{row:?}");
}

/// A database dump is not a client.
///
/// This is the one escape, and it has to exist: a fixture without `password_hash` restores a
/// database where nobody can log in, and one without the `Masked` ciphertext restores empty
/// encrypted columns. Backups are the reason `secret` cannot simply mean "never SELECT".
#[tokio::test]
async fn a_backup_reads_everything() {
    let meta = boot().await;
    let row = DynQuerySet::for_meta(&meta)
        .unredacted_for_backup()
        .first_as_json()
        .await
        .expect("fetch")
        .expect("row");

    assert_eq!(row["cost"], json!("4.20"));
    assert_eq!(row["signing_key"], json!("sk_signing_do_not_leak"));
    assert_eq!(row["password_hash"], json!("$argon2id$leak"));
    assert!(
        row.contains_key("api_token"),
        "a dump missing the ciphertext restores an empty encrypted column: {row:?}"
    );
}

/// The write path echoes the created row back — and that echo is a serialized response too.
///
/// Easy to miss: you redact every read, ship it, and `POST /products` hands the caller the
/// private field right back in the 201 body.
#[tokio::test]
async fn the_row_echoed_after_a_write_is_redacted_too() {
    let meta = boot().await;
    let mut body = serde_json::Map::new();
    body.insert("name".into(), json!("Gadget"));
    body.insert("cost".into(), json!("9.99"));
    body.insert("signing_key".into(), json!("sk_two"));
    body.insert("api_token".into(), json!("enc:cafe"));
    body.insert("password_hash".into(), json!("$argon2id$two"));

    let created = DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect("insert");

    assert_eq!(created["name"], json!("Gadget"), "the write did happen");
    assert!(
        !created.contains_key("cost"),
        "echo leaked private: {created:?}"
    );
    assert!(
        !created.contains_key("signing_key") && !created.contains_key("password_hash"),
        "echo leaked secret: {created:?}"
    );

    // The value was still WRITTEN — redaction is about what comes back, not about dropping
    // the caller's data on the floor.
    let stored = DynQuerySet::for_meta(&meta)
        .unredacted_for_backup()
        .filter_eq_string("name", "Gadget")
        .first_as_json()
        .await
        .expect("fetch")
        .expect("row");
    assert_eq!(stored["cost"], json!("9.99"), "the write must not be lossy");
}
