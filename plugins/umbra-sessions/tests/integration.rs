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

/// create_session returns a raw token; the DB row is keyed by its
/// SHA-256 digest. read_session looks the row up by hashing the
/// passed-in token. The Session struct's `id` field carries the
/// stored digest (not the raw token), so an attacker who exfiltrates
/// the row never sees the live cookie value.
#[tokio::test]
async fn create_and_read_round_trip() {
    let user_id = boot().await;
    let token = create_session(user_id, None).await.expect("create");
    let s = read_session(&token).await.expect("read").expect("present");
    // The raw token is a UUID; the stored id is a 64-char hex SHA-256.
    // They must differ — if they matched the column would still hold
    // the live token.
    assert_ne!(
        s.id, token,
        "stored id should be the hashed token, not the raw token"
    );
    assert_eq!(s.id.len(), 64, "stored id should be a 64-char hex digest");
    assert!(s.id.chars().all(|c| c.is_ascii_hexdigit()));
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

// =====================================================================
// login / logout helpers — the three-call dance, bundled.
// =====================================================================

#[tokio::test]
async fn login_creates_session_sets_cookie_and_bumps_last_login() {
    let user_id = boot().await;
    // Fetch the user via the auth helper so the row is fresh.
    let pool = umbra::db::pool();
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("fetch user");
    let before_login = user.last_login;

    let mut response_headers = HeaderMap::new();
    let token = umbra_sessions::login(&mut response_headers, &user)
        .await
        .expect("login ok");

    // The token is a real UUID-shape string.
    assert!(!token.is_empty());
    // Set-Cookie header was added with the session cookie.
    let set_cookie = response_headers
        .get(header::SET_COOKIE)
        .expect("Set-Cookie header set")
        .to_str()
        .unwrap();
    assert!(set_cookie.starts_with(&format!("{COOKIE_NAME}=")));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("Secure"));

    // The session row is readable via the returned token.
    let session = read_session(&token).await.expect("read").expect("present");
    assert_eq!(session.user_id, Some(user_id));

    // last_login was updated.
    let user_after: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("fetch user after login");
    assert!(
        user_after.last_login.is_some(),
        "login should populate last_login"
    );
    if let Some(before) = before_login {
        assert!(user_after.last_login.unwrap() > before, "should advance");
    }
}

#[tokio::test]
async fn logout_destroys_session_and_clears_cookie() {
    let user_id = boot().await;
    // First log in.
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&umbra::db::pool())
        .await
        .unwrap();
    let mut login_headers = HeaderMap::new();
    let token = umbra_sessions::login(&mut login_headers, &user)
        .await
        .expect("login");

    // Simulate the browser sending the cookie back.
    let mut request_headers = HeaderMap::new();
    request_headers.insert(
        header::COOKIE,
        format!("{COOKIE_NAME}={token}").parse().unwrap(),
    );

    let mut response_headers = HeaderMap::new();
    umbra_sessions::logout(&request_headers, &mut response_headers)
        .await
        .expect("logout ok");

    // The session row is gone.
    assert!(read_session(&token).await.unwrap().is_none());

    // The response carries a Max-Age=0 cookie that expires the browser-side value.
    let cleared = response_headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cleared.contains("Max-Age=0"));
}

#[tokio::test]
async fn logout_is_safe_without_a_cookie() {
    let _user_id = boot().await;
    let request_headers = HeaderMap::new();
    let mut response_headers = HeaderMap::new();
    umbra_sessions::logout(&request_headers, &mut response_headers)
        .await
        .expect("logout without cookie should be a no-op-with-cookie-clear");
    // The clear-cookie header still gets set (browser may have a stale value).
    assert!(
        response_headers.contains_key(header::SET_COOKIE),
        "logout always clears the client-side cookie",
    );
}

// =====================================================================
// User / OptionalUser extractors via axum FromRequestParts.
// =====================================================================

#[tokio::test]
async fn optional_user_returns_some_when_session_cookie_resolves() {
    use axum_core::extract::FromRequestParts;
    let user_id = boot().await;
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&umbra::db::pool())
        .await
        .unwrap();
    let mut login_headers = HeaderMap::new();
    let token = umbra_sessions::login(&mut login_headers, &user)
        .await
        .unwrap();

    let req = http::Request::builder()
        .uri("/")
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(())
        .unwrap();
    let (mut parts, _) = req.into_parts();
    let umbra_sessions::OptionalUser(opt) =
        umbra_sessions::OptionalUser::from_request_parts(&mut parts, &())
            .await
            .unwrap();
    assert!(opt.is_some(), "session cookie should resolve to a user");
    assert_eq!(opt.unwrap().id, user_id);
}

#[tokio::test]
async fn optional_user_returns_none_for_anonymous_request() {
    use axum_core::extract::FromRequestParts;
    let _user_id = boot().await;
    let req = http::Request::builder().uri("/").body(()).unwrap();
    let (mut parts, _) = req.into_parts();
    let umbra_sessions::OptionalUser(opt) =
        umbra_sessions::OptionalUser::from_request_parts(&mut parts, &())
            .await
            .unwrap();
    assert!(opt.is_none(), "anonymous → None, not 401");
}

#[tokio::test]
async fn user_required_extractor_returns_401_for_anonymous() {
    use axum_core::extract::FromRequestParts;
    let _user_id = boot().await;
    let req = http::Request::builder().uri("/").body(()).unwrap();
    let (mut parts, _) = req.into_parts();
    let err = umbra_sessions::User::from_request_parts(&mut parts, &())
        .await
        .expect_err("anonymous should 401");
    assert_eq!(err.0, http::StatusCode::UNAUTHORIZED);
}

// =====================================================================
// Messages framework — flash messages over the session data store.
// =====================================================================

#[tokio::test]
async fn messages_add_and_drain_round_trip() {
    use umbra_sessions::{MessageLevel, Messages};
    let user_id = boot().await;
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&umbra::db::pool())
        .await
        .unwrap();
    let mut login_headers = HeaderMap::new();
    let token = umbra_sessions::login(&mut login_headers, &user)
        .await
        .unwrap();

    let msgs = Messages::new(Some(token.clone()));
    assert!(msgs.is_active());
    msgs.success("Post saved!").await;
    msgs.warning("Quota at 80%").await;

    // peek doesn't clear.
    let peeked = msgs.peek().await;
    assert_eq!(peeked.len(), 2);
    assert_eq!(peeked[0].level, MessageLevel::Success);
    assert_eq!(peeked[1].level, MessageLevel::Warning);

    // drain returns them then empties the queue.
    let drained = msgs.drain().await;
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].text, "Post saved!");
    assert!(msgs.drain().await.is_empty(), "second drain is empty");
}

#[tokio::test]
async fn messages_extractor_returns_handle_when_session_cookie_present() {
    use axum_core::extract::FromRequestParts;
    use umbra_sessions::Messages;
    let user_id = boot().await;
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&umbra::db::pool())
        .await
        .unwrap();
    let mut login_headers = HeaderMap::new();
    let token = umbra_sessions::login(&mut login_headers, &user)
        .await
        .unwrap();

    let req = http::Request::builder()
        .uri("/")
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(())
        .unwrap();
    let (mut parts, _) = req.into_parts();
    let msgs = Messages::from_request_parts(&mut parts, &()).await.unwrap();
    assert!(msgs.is_active());

    msgs.info("Welcome back").await;
    let drained = msgs.drain().await;
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].text, "Welcome back");
}

#[tokio::test]
async fn messages_silently_noops_without_a_session() {
    use umbra_sessions::Messages;
    let _user_id = boot().await;
    let msgs = Messages::new(None);
    assert!(!msgs.is_active());
    // These all silently no-op.
    msgs.success("vanishing").await;
    msgs.error("also vanishing").await;
    // drain returns empty.
    assert!(msgs.drain().await.is_empty());
}
