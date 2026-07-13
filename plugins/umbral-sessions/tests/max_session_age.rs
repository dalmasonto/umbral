//! audit_2 plugin-sessions #5 — an absolute session-lifetime cap expires a
//! session past `created_at + max_age` even when `expires_at` (which sliding
//! expiry keeps bumping) is still far in the future. Without the cap, a session
//! used at least once per TTL window never expires.

use chrono::{Duration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral_sessions::{Session, SessionsPlugin, create_session, read_session};

async fn boot_with_cap(secs: i64) {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("max_age.sqlite");
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
        // Sliding expiry ON + a 1-second absolute cap: the whole point is that
        // the absolute bound wins over the sliding one.
        .plugin(
            SessionsPlugin::default()
                .sliding_expiry()
                .max_session_age(secs),
        )
        .build()
        .expect("App::build with SessionsPlugin");

    umbral::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema");
}

#[tokio::test]
async fn absolute_cap_expires_and_destroys_an_over_age_session() {
    boot_with_cap(1).await;

    // A fresh session (created just now) is comfortably under the 1s cap.
    let token = create_session(Some("7".to_string()), None)
        .await
        .expect("create session");
    assert!(
        read_session(&token).await.unwrap().is_some(),
        "a just-created session must be readable — the cap must not over-reject"
    );

    // Backdate `created_at` an hour into the past while leaving `expires_at`
    // 14 days out (sliding expiry would keep this alive forever). Only the
    // absolute cap can now expire it.
    let past = (Utc::now() - Duration::hours(1)).to_rfc3339();
    sqlx::query("UPDATE session SET created_at = ?")
        .bind(&past)
        .execute(&umbral::db::pool())
        .await
        .expect("backdate created_at");

    // The absolute cap rejects it...
    assert!(
        read_session(&token).await.unwrap().is_none(),
        "a session older than the absolute max age must resolve to None"
    );
    // ...and destroys the stale row (not just hides it).
    assert_eq!(
        Session::objects().count().await.unwrap(),
        0,
        "the over-age session must be destroyed, not left behind"
    );
}
