//! Test for `revoke_user_sessions` — the "log out everywhere" primitive.
//!
//! Creates sessions for two users (one anonymous), revokes one user's
//! sessions, and verifies only the other user's + anonymous session remain.

use chrono::Duration;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("revoke_user_sessions.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
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

#[tokio::test]
async fn revoke_removes_all_of_one_users_sessions_only() {
    boot().await;

    // Two sessions for user "7", one for user "9", one anonymous.
    umbral_sessions::create_session(Some("7".into()), Some(Duration::seconds(3600)))
        .await
        .unwrap();
    umbral_sessions::create_session(Some("7".into()), Some(Duration::seconds(3600)))
        .await
        .unwrap();
    umbral_sessions::create_session(Some("9".into()), Some(Duration::seconds(3600)))
        .await
        .unwrap();
    umbral_sessions::create_session(None, Some(Duration::seconds(3600)))
        .await
        .unwrap();

    let removed = umbral_sessions::revoke_user_sessions("7").await.unwrap();
    assert_eq!(removed, 2, "both of user 7's sessions removed");

    // user 9 + anonymous remain.
    let remaining = umbral_sessions::Session::objects().count().await.unwrap();
    assert_eq!(remaining, 2);
}
