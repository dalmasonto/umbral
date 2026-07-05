//! audit_2 plugin-sessions #6 — out-of-request `set_data` must route through the
//! INSTALLED store, not always the raw SQL `session` table. Before the fix the
//! fallback wrote raw SQL; under a non-DB store that write hit an empty/absent
//! SQL table and was silently lost. This test installs a custom in-memory store
//! (NO SQL `session` table exists at all) and proves the write round-trips.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{Duration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral_sessions::{
    SessionRecord, SessionStore, SessionsPlugin, get_data, read_session, set_data,
};

/// A minimal non-DB session store: records live in a `HashMap` keyed by the raw
/// token. If out-of-request `set_data` still wrote raw SQL, nothing would ever
/// land here and `read_session` would see no data.
#[derive(Debug, Default)]
struct MemStore {
    map: Mutex<HashMap<String, SessionRecord>>,
}

#[async_trait::async_trait]
impl SessionStore for MemStore {
    async fn load(
        &self,
        token: &str,
    ) -> Result<Option<SessionRecord>, umbral_sessions::SessionError> {
        Ok(self.map.lock().unwrap().get(token).cloned())
    }

    async fn save(
        &self,
        token: &str,
        record: &SessionRecord,
    ) -> Result<String, umbral_sessions::SessionError> {
        self.map
            .lock()
            .unwrap()
            .insert(token.to_string(), record.clone());
        Ok(token.to_string())
    }

    async fn destroy(&self, token: &str) -> Result<(), umbral_sessions::SessionError> {
        self.map.lock().unwrap().remove(token);
        Ok(())
    }
}

async fn boot() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("mem_store.sqlite");
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
        // The active store is the in-memory one — there is deliberately NO
        // `session` SQL table, so any raw-SQL write would error/vanish.
        .plugin(SessionsPlugin::default().store(MemStore::default()))
        .build()
        .expect("App::build with SessionsPlugin + MemStore");
}

#[tokio::test]
async fn out_of_request_set_data_persists_through_a_non_db_store() {
    boot().await;

    // Seed a session directly in the store (as create_session would, but via
    // the store since there's no SQL table).
    let token = "raw-token-abc";
    let now = Utc::now();
    umbral_sessions::active_store()
        .save(
            token,
            &SessionRecord {
                user_id: Some("42".to_string()),
                data: "{}".to_string(),
                created_at: now,
                expires_at: now + Duration::days(1),
            },
        )
        .await
        .expect("seed session in the store");

    // Out-of-request set_data (no request scope → the fallback path).
    set_data(token, "cart_count", &3u32)
        .await
        .expect("set_data out of request");

    // read_session routes through the store; the key must be visible.
    let session = read_session(token)
        .await
        .expect("read_session")
        .expect("session present");
    let cart: Option<u32> = get_data(&session, "cart_count").expect("decode");
    assert_eq!(
        cart,
        Some(3),
        "out-of-request set_data must persist through the installed non-DB store"
    );
}
