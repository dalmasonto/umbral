//! Lazy session creation — login still creates exactly one row
//! (regression guard, gaps2 #46).
//!
//! Own test binary (own ambient pool) so the global
//! `Session::objects().count()` assertions are isolated.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use umbra::web::HeaderMap;
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_sessions::{Session, SessionsPlugin, read_session};

async fn boot() -> i64 {
    let settings = umbra::Settings::from_env().expect("figment defaults load");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("lazy_session_login.sqlite");
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

    umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(AuthPlugin::<AuthUser>::default())
        .plugin(SessionsPlugin::default())
        .build()
        .expect("App::build with AuthPlugin + SessionsPlugin");

    let pool = umbra::db::pool();
    sqlx::query(
        "CREATE TABLE auth_user (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            username TEXT NOT NULL UNIQUE,\
            email TEXT NOT NULL,\
            password_hash TEXT NOT NULL,\
            is_active INTEGER NOT NULL,\
            is_staff INTEGER NOT NULL,\
            is_superuser INTEGER NOT NULL,\
            date_joined TEXT NOT NULL,\
            last_login TEXT\
         )",
    )
    .execute(&pool)
    .await
    .expect("create auth_user");
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
    .expect("create session");

    let u: AuthUser = create_user("dave", "dave@example.com", "hunter2")
        .await
        .expect("create user");
    u.id
}

/// A login with no inbound cookie creates exactly one authenticated
/// row (net count behaves as before the lazy change — login's
/// fixation defense destroys any old session and mints one new authed
/// one).
#[tokio::test]
async fn login_creates_exactly_one_authenticated_row() {
    let user_id = boot().await;
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&umbra::db::pool())
        .await
        .unwrap();

    assert_eq!(
        Session::objects().count().await.unwrap(),
        0,
        "no sessions before login",
    );

    let mut resp_headers = HeaderMap::new();
    let token = umbra_auth::login(&mut resp_headers, &user)
        .await
        .expect("login");

    assert_eq!(
        Session::objects().count().await.unwrap(),
        1,
        "login with no inbound cookie must create exactly one row",
    );

    let s = read_session(&token).await.unwrap().expect("present");
    assert_eq!(s.user_id, Some(user_id.to_string()));
}
