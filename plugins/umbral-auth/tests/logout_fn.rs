//! TDD: public `umbral_auth::logout` exposes a reusable logout that both
//! built-in surfaces and custom handlers can call.
//!
//! Boots a real SQLite pool, creates the session table directly, establishes
//! a session via `umbral_sessions::login_user_id`, then calls
//! `umbral_auth::logout` carrying that cookie and asserts a clearing
//! Set-Cookie is emitted.
//!
//! Pattern mirrors `plugins/umbral-sessions/tests/revoke_user_sessions.rs`.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("umbral_auth_logout_fn.sqlite");
        // Leak so the file outlives the test binary.
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

/// Establish a session via `umbral_sessions::login_user_id`, carry the
/// Set-Cookie back as a Cookie header, then call `umbral_auth::logout`.
/// A clearing Set-Cookie must appear on the response headers.
#[tokio::test]
async fn logout_clears_the_session_cookie() {
    boot().await;

    // Establish a session, capture the Set-Cookie.
    let mut set = http::HeaderMap::new();
    umbral_sessions::login_user_id(&http::HeaderMap::new(), &mut set, Some("1".into()))
        .await
        .unwrap();
    let cookie = set
        .get(http::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // Build a request carrying just the token part of the cookie (before ';').
    let mut req = http::HeaderMap::new();
    req.insert(
        http::header::COOKIE,
        cookie.split(';').next().unwrap().parse().unwrap(),
    );
    let mut resp = http::HeaderMap::new();
    umbral_auth::logout(&req, &mut resp).await.unwrap();

    // logout must emit a clearing Set-Cookie.
    assert!(
        resp.get(http::header::SET_COOKIE).is_some(),
        "logout sets a clearing cookie"
    );
}
