//! End-to-end coverage for umbra-sessions: create a session via the
//! helper, look it up via the cookie header, hydrate the user via
//! `current_user`, destroy on logout.
//!
//! Boots once via OnceCell with AuthPlugin + SessionsPlugin
//! registered. Tempfile-backed sqlite so every pool connection sees
//! the same database, same pattern umbra-auth + umbra-admin tests
//! use.

use std::path::PathBuf;

use chrono::Duration;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbra::web::{HeaderMap, header};
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_sessions::{
    COOKIE_NAME, SessionsPlugin, clear_cookie_header, cookie_from_headers, create_session,
    current_user, destroy_session, get_data, read_session, set_cookie_header, set_data,
};

static BOOT: OnceCell<i64> = OnceCell::const_new();

async fn boot() -> i64 {
    *BOOT
        .get_or_init(|| async {
            let settings = umbra::Settings::from_env().expect("figment defaults load");
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("sessions_integration.sqlite");
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
                .plugin(AuthPlugin)
                .plugin(SessionsPlugin)
                .build()
                .expect("App::build with AuthPlugin + SessionsPlugin");

            // Create both tables.
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
                    user_id INTEGER,\
                    data TEXT NOT NULL,\
                    created_at TEXT NOT NULL,\
                    expires_at TEXT NOT NULL\
                 )",
            )
            .execute(&pool)
            .await
            .expect("create session");

            // Seed a user we'll log in as.
            let u: AuthUser = create_user("carol", "carol@example.com", "hunter2")
                .await
                .expect("create user");
            u.id
        })
        .await
}

/// create_session writes a row keyed by the returned id, with the
/// right user_id, an `{}` data string, and an expires_at in the
/// future. read_session pulls the row back.
#[tokio::test]
async fn create_and_read_round_trip() {
    let user_id = boot().await;
    let id = create_session(user_id, None).await.expect("create");
    let s = read_session(&id).await.expect("read").expect("present");
    assert_eq!(s.id, id);
    assert_eq!(s.user_id, Some(user_id));
    assert_eq!(s.data, "{}");
    assert!(s.expires_at > chrono::Utc::now());
}

/// A session whose expires_at is in the past returns None AND is
/// deleted from the DB on the read (lazy cleanup, no scheduled job
/// required).
#[tokio::test]
async fn read_session_returns_none_for_expired_and_deletes_the_row() {
    let user_id = boot().await;
    // Create with a negative TTL so the row is already expired.
    let id = create_session(user_id, Some(Duration::seconds(-1)))
        .await
        .expect("create");
    let result = read_session(&id).await.expect("read");
    assert!(
        result.is_none(),
        "expired session should return None; got {result:?}"
    );

    // Confirm the row was actually deleted.
    let pool = umbra::db::pool();
    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM session WHERE id = ?")
        .bind(&id)
        .fetch_optional(&pool)
        .await
        .expect("select");
    assert!(
        row.is_none(),
        "read_session should have deleted the expired row"
    );
}

/// destroy_session deletes the row; subsequent reads return None.
#[tokio::test]
async fn destroy_session_removes_the_row() {
    let user_id = boot().await;
    let id = create_session(user_id, None).await.expect("create");
    assert!(read_session(&id).await.unwrap().is_some());
    destroy_session(&id).await.expect("destroy");
    assert!(read_session(&id).await.unwrap().is_none());
}

/// cookie_from_headers parses a Cookie header looking for the
/// umbra_session value among other cookies.
#[tokio::test]
async fn cookie_extraction_handles_multiple_cookies() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("other=foo; {COOKIE_NAME}=abc-def; tracking=xyz")
            .parse()
            .unwrap(),
    );
    let got = cookie_from_headers(&headers);
    assert_eq!(got.as_deref(), Some("abc-def"));
}

/// set_cookie_header sets all the secure-by-default flags from the
/// security-defaults outline.
#[test]
fn set_cookie_header_carries_secure_defaults() {
    let s = set_cookie_header("the-id", None);
    assert!(s.contains("the-id"));
    assert!(s.contains("HttpOnly"));
    assert!(s.contains("Secure"));
    assert!(s.contains("SameSite=Lax"));
    assert!(s.contains("Path=/"));
    // Default TTL is 14 days = 1_209_600 seconds.
    assert!(s.contains("Max-Age=1209600"));
}

/// clear_cookie_header emits Max-Age=0 so the browser drops the
/// cookie on logout.
#[test]
fn clear_cookie_header_zeroes_max_age() {
    let s = clear_cookie_header();
    assert!(s.contains("Max-Age=0"));
}

/// current_user is the one-call handler helper: cookie → session →
/// user. Returns Some(AuthUser) on the happy path.
#[tokio::test]
async fn current_user_round_trip_hydrates_the_logged_in_user() {
    let user_id = boot().await;
    let id = create_session(user_id, None).await.expect("create");

    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("{COOKIE_NAME}={id}").parse().unwrap(),
    );

    let user = current_user(&headers)
        .await
        .expect("current_user")
        .expect("should hydrate");
    assert_eq!(user.id, user_id);
    assert_eq!(user.username, "carol");
}

/// current_user with no cookie returns Ok(None) — anonymous request.
#[tokio::test]
async fn current_user_returns_none_when_no_cookie() {
    boot().await;
    let headers = HeaderMap::new();
    let result = current_user(&headers).await.expect("no error");
    assert!(result.is_none());
}

/// current_user with a destroyed session returns Ok(None) — the
/// cookie is stale.
#[tokio::test]
async fn current_user_returns_none_for_destroyed_session() {
    let user_id = boot().await;
    let id = create_session(user_id, None).await.expect("create");
    destroy_session(&id).await.expect("destroy");

    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("{COOKIE_NAME}={id}").parse().unwrap(),
    );

    let result = current_user(&headers).await.expect("no error");
    assert!(result.is_none());
}

/// get_data / set_data round-trip a typed value through the JSON
/// data column without clobbering other keys.
#[tokio::test]
async fn data_round_trip_through_json_column() {
    let user_id = boot().await;
    let id = create_session(user_id, None).await.expect("create");

    set_data(&id, "cart_id", &42i64).await.expect("set cart_id");
    set_data(&id, "flash", &"welcome back")
        .await
        .expect("set flash");

    let s = read_session(&id).await.unwrap().unwrap();
    let cart: Option<i64> = get_data(&s, "cart_id").expect("get cart_id");
    let flash: Option<String> = get_data(&s, "flash").expect("get flash");
    let missing: Option<String> = get_data(&s, "nope").expect("get missing");

    assert_eq!(cart, Some(42));
    assert_eq!(flash.as_deref(), Some("welcome back"));
    assert_eq!(missing, None);
}

// Quiet a probable unused-import warning for PathBuf if Rust ever
// reshuffles which test references it.
#[allow(dead_code)]
fn _unused_pathbuf_marker() -> Option<PathBuf> {
    None
}
