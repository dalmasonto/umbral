//! audit_2 plugin-sessions #5 — the absolute session-age cap is OFF by default
//! (behavior-preserving). Without `max_session_age(..)`, an old session stays
//! governed only by `expires_at`, exactly as before the feature landed. This
//! guards against the cap accidentally defaulting on and expiring live sessions.

use chrono::{Duration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral_sessions::{SessionsPlugin, create_session, read_session};

async fn boot_no_cap() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("no_cap.sqlite");
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
        // No `.max_session_age(..)` — the default (no cap).
        .plugin(SessionsPlugin::default().sliding_expiry())
        .build()
        .expect("App::build with SessionsPlugin");

    sqlx::query(
        "CREATE TABLE session (\
            id TEXT PRIMARY KEY,\
            user_id TEXT,\
            data TEXT NOT NULL,\
            created_at TEXT NOT NULL,\
            expires_at TEXT NOT NULL\
         )",
    )
    .execute(&umbral::db::pool())
    .await
    .expect("create session table");
}

#[tokio::test]
async fn without_a_cap_an_old_session_is_still_valid() {
    boot_no_cap().await;

    let token = create_session(Some("7".to_string()), None)
        .await
        .expect("create session");
    // A year old, but `expires_at` is still 14 days out — with NO absolute cap
    // the session must remain valid (only `expires_at` governs).
    let past = (Utc::now() - Duration::days(365)).to_rfc3339();
    sqlx::query("UPDATE session SET created_at = ?")
        .bind(&past)
        .execute(&umbral::db::pool())
        .await
        .expect("backdate created_at");

    assert!(
        read_session(&token).await.unwrap().is_some(),
        "with no absolute cap configured, an old-but-unexpired session stays valid"
    );
}
