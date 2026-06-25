//! Tests for the `SessionStore` trait and the `DbStore` implementation.
//!
//! Boot pattern: App::build with a sqlite pool + CREATE TABLE session.
//! Copied from `tests/lazy_session.rs`. Each test file is its own
//! binary (separate process → separate ambient `OnceLock`), so the
//! count assertions here are not polluted by other test suites.

use chrono::{Duration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral_sessions::store::{DbStore, SessionRecord, SessionStore, active_store, install_store};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("store_dbstore.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE session (\
                id TEXT PRIMARY KEY,\
                user_id TEXT,\
                data TEXT NOT NULL,\
                created_at TEXT NOT NULL,\
                expires_at TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create session table");
    })
    .await;
}

/// `DbStore::save` then `DbStore::load` returns the same record.
/// user_id and data round-trip correctly.
#[tokio::test]
async fn save_and_load_round_trip() {
    boot().await;
    let store = DbStore::default();
    let now = Utc::now();
    let record = SessionRecord {
        user_id: Some("42".to_string()),
        data: r#"{"cart":99}"#.to_string(),
        created_at: now,
        expires_at: now + Duration::seconds(3600),
    };

    store.save("tok-round-trip", &record).await.expect("save");
    let loaded = store
        .load("tok-round-trip")
        .await
        .expect("load")
        .expect("present");

    assert_eq!(loaded.user_id, Some("42".to_string()));
    assert_eq!(loaded.data, r#"{"cart":99}"#);
    assert!(loaded.expires_at > Utc::now());
}

/// `load` for a token that was never saved returns `None`.
#[tokio::test]
async fn load_missing_token_returns_none() {
    boot().await;
    let store = DbStore::default();
    let result = store.load("tok-missing").await.expect("no error");
    assert!(result.is_none(), "non-existent token → None");
}

/// A record with `expires_at` in the past:
///   - `load` returns `None` (lazy expiry)
///   - the row is deleted from the DB
#[tokio::test]
async fn load_expired_record_returns_none_and_deletes_row() {
    boot().await;
    let store = DbStore::default();
    let now = Utc::now();
    let record = SessionRecord {
        user_id: None,
        data: "{}".to_string(),
        created_at: now - Duration::seconds(10),
        expires_at: now - Duration::seconds(1), // already expired
    };

    store.save("tok-expired", &record).await.expect("save");

    // load should return None and delete the row.
    let result = store.load("tok-expired").await.expect("no error");
    assert!(result.is_none(), "expired session → None");

    // Confirm row is gone.
    let pool = umbral::db::pool();
    use umbral_sessions::store::hash_token_pub;
    let hash = hash_token_pub("tok-expired");
    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM session WHERE id = ?")
        .bind(&hash)
        .fetch_optional(&pool)
        .await
        .expect("select");
    assert!(
        row.is_none(),
        "expired row should have been deleted by load"
    );
}

/// `destroy` removes the row; subsequent `load` returns `None`.
#[tokio::test]
async fn destroy_removes_row() {
    boot().await;
    let store = DbStore::default();
    let now = Utc::now();
    let record = SessionRecord {
        user_id: Some("7".to_string()),
        data: "{}".to_string(),
        created_at: now,
        expires_at: now + Duration::seconds(3600),
    };

    store.save("tok-destroy", &record).await.expect("save");
    assert!(
        store.load("tok-destroy").await.unwrap().is_some(),
        "should be present before destroy"
    );

    store.destroy("tok-destroy").await.expect("destroy");
    assert!(
        store.load("tok-destroy").await.unwrap().is_none(),
        "should be gone after destroy"
    );
}

/// `destroy` is idempotent — calling it a second time succeeds.
#[tokio::test]
async fn destroy_is_idempotent() {
    boot().await;
    let store = DbStore::default();
    // Never saved — destroy should succeed (no error).
    store
        .destroy("tok-never-existed")
        .await
        .expect("destroy on non-existent token should be a no-op");
}

/// The DB `id` column stores the SHA-256 hash of the token, not the
/// raw token value.
#[tokio::test]
async fn db_id_is_hashed_token_not_raw_token() {
    boot().await;
    let store = DbStore::default();
    let now = Utc::now();
    let record = SessionRecord {
        user_id: None,
        data: "{}".to_string(),
        created_at: now,
        expires_at: now + Duration::seconds(3600),
    };

    store.save("tok-hash-check", &record).await.expect("save");

    use umbral_sessions::store::hash_token_pub;
    let expected_hash = hash_token_pub("tok-hash-check");

    let pool = umbral::db::pool();
    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM session WHERE id = ?")
        .bind(&expected_hash)
        .fetch_optional(&pool)
        .await
        .expect("select");
    assert!(
        row.is_some(),
        "row should exist at id=hash(token), not raw token"
    );
    assert_eq!(row.unwrap().0, expected_hash);
    assert_ne!(
        expected_hash, "tok-hash-check",
        "hash differs from raw token"
    );
    assert_eq!(expected_hash.len(), 64, "SHA-256 hex is 64 chars");
}

/// `active_store()` returns a working `DbStore` even when no store
/// was installed via `install_store`.
#[tokio::test]
async fn active_store_returns_default_dbstore_when_none_installed() {
    boot().await;
    let store = active_store();
    let now = Utc::now();
    let record = SessionRecord {
        user_id: None,
        data: "{}".to_string(),
        created_at: now,
        expires_at: now + Duration::seconds(3600),
    };
    // Just verify it can do a round-trip through the DB.
    store
        .save("tok-active-default", &record)
        .await
        .expect("save");
    let loaded = store.load("tok-active-default").await.expect("load");
    assert!(loaded.is_some());
}

/// `install_store` is idempotent — a second call warns and keeps the
/// first store installed.
#[tokio::test]
async fn install_store_is_idempotent() {
    boot().await;
    use std::sync::Arc;
    let s1 = Arc::new(DbStore::default()) as Arc<dyn SessionStore + Send + Sync>;
    let s2 = Arc::new(DbStore::default()) as Arc<dyn SessionStore + Send + Sync>;
    // Both calls should succeed (second silently warns + keeps first).
    install_store(s1);
    install_store(s2);
}
