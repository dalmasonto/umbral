//! Public session API routed through the request-scoped session (2a Task 3).
//!
//! Verifies the body rewrites of `set_data` / `get_data` / `current_session`
//! / `current_user_id_str` / `login_user_id`:
//!   (a) a handler that `set_data`s a key then reads it back IN THE SAME
//!       request sees the in-memory record (no mid-request DB write), and
//!       the value is persisted at layer exit (exactly 1 row);
//!   (b) a SECOND request carrying the same cookie reads the persisted
//!       value back via `current_session` / `get_data`;
//!   (c) `login_user_id` inside a request rotates the cookie token (a new
//!       Set-Cookie with a different token) and destroys the old session
//!       row.
//!
//! Own test binary (own ambient pool `OnceLock`) so the global
//! `Session::objects().count()` assertions are isolated from other suites.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral::web::header;
use umbral_auth::{AuthPlugin, AuthUser};
use umbral_sessions::{COOKIE_NAME, Session, SessionsPlugin};

static BOOT: OnceCell<()> = OnceCell::const_new();

/// Serialises the tests in this binary; each asserts on the GLOBAL
/// `Session::objects().count()` delta, so they must not overlap.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("request_session_data.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
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
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default())
            .build()
            .expect("App::build with AuthPlugin + SessionsPlugin");

        let pool = umbral::db::pool();
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
                last_login TEXT,\
                email_verified_at TEXT\
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
    })
    .await;
}

fn token_from_set_cookie(set_cookie: &str) -> String {
    set_cookie
        .strip_prefix(&format!("{COOKIE_NAME}="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// (a) `set_data` in a request, read it back via `current_session` +
/// `get_data` in the SAME request (served from the in-memory record),
/// then it's persisted at exit (exactly 1 row), and (b) a second request
/// with the same cookie sees the persisted value.
#[tokio::test]
async fn set_data_in_request_reads_back_and_persists_once() {
    use axum::body::Body;
    use axum::extract::Request;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;
    use umbral::web::HeaderMap;

    boot().await;
    let _guard = SERIAL.lock().await;

    // Handler #1: set a key, then read it back IN THE SAME request from the
    // in-memory record (the token comes from the SessionToken extension
    // installed by session_layer).
    async fn writer(headers: HeaderMap, req: Request) -> impl IntoResponse {
        let token = req
            .extensions()
            .get::<umbral_sessions::SessionToken>()
            .map(|t| t.0.clone())
            .expect("session token in extensions");
        umbral_sessions::set_data(&token, "k", &42i64)
            .await
            .expect("set_data");
        // Same-request read-back via the in-memory record.
        let s = umbral_sessions::current_session(&headers)
            .await
            .expect("current_session ok")
            .expect("in-request session view");
        let v: Option<i64> = umbral_sessions::get_data(&s, "k").expect("get_data");
        assert_eq!(
            v,
            Some(42),
            "same-request read-back sees the in-memory write"
        );
        "wrote"
    }

    let inner = axum::Router::new().route("/w", get(writer));
    let router = SessionsPlugin::default().wrap_router(inner);

    let before = Session::objects().count().await.expect("count before");

    let req = Request::builder().uri("/w").body(Body::empty()).unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("a written session sets a cookie")
        .to_str()
        .unwrap()
        .to_string();
    let token = token_from_set_cookie(&set_cookie);

    let after = Session::objects().count().await.expect("count after");
    assert_eq!(
        after,
        before + 1,
        "the in-request set_data persists exactly one row at exit",
    );

    // (b) A SECOND request with the same cookie sees the persisted value.
    async fn reader(headers: HeaderMap) -> impl IntoResponse {
        let s = umbral_sessions::current_session(&headers)
            .await
            .expect("current_session ok")
            .expect("session present");
        let v: Option<i64> = umbral_sessions::get_data(&s, "k").expect("get_data");
        assert_eq!(v, Some(42), "second request sees the persisted value");
        "read"
    }

    let inner2 = axum::Router::new().route("/r", get(reader));
    let router2 = SessionsPlugin::default().wrap_router(inner2);
    let req2 = Request::builder()
        .uri("/r")
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(Body::empty())
        .unwrap();
    let resp2 = router2.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), http::StatusCode::OK);
}

/// (c) `login_user_id` inside a request rotates the cookie token (new
/// Set-Cookie, different token) and destroys the old session row.
#[tokio::test]
async fn login_rotates_cookie_and_destroys_old_session() {
    use axum::body::Body;
    use axum::extract::Request;
    use axum::response::Response;
    use axum::routing::get;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;
    use umbral::web::HeaderMap;
    use umbral_sessions::{create_session, read_session};

    boot().await;
    let _guard = SERIAL.lock().await;

    // Establish a live anonymous session with some carry-over data.
    let old_token = create_session(None, None).await.expect("create_session");
    umbral_sessions::set_data(&old_token, "cart", &"abc")
        .await
        .expect("seed cart");
    assert!(
        read_session(&old_token).await.unwrap().is_some(),
        "old session exists before login",
    );

    async fn login_handler(headers: HeaderMap) -> Response {
        let mut resp = axum::response::IntoResponse::into_response("logged in");
        umbral_sessions::login_user_id(&headers, resp.headers_mut(), Some("7".to_string()))
            .await
            .expect("login_user_id");
        resp
    }

    let inner = axum::Router::new().route("/login", get(login_handler));
    let router = SessionsPlugin::default().wrap_router(inner);

    let req = Request::builder()
        .uri("/login")
        .header(header::COOKIE, format!("{COOKIE_NAME}={old_token}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("login sets a fresh cookie")
        .to_str()
        .unwrap()
        .to_string();
    let new_token = token_from_set_cookie(&set_cookie);
    assert_ne!(new_token, old_token, "login rotates the cookie token");

    // Old session destroyed (fixation defense).
    assert!(
        read_session(&old_token).await.unwrap().is_none(),
        "the old session row is destroyed on login",
    );

    // New session is authenticated and carried the data over.
    let s = read_session(&new_token)
        .await
        .unwrap()
        .expect("new authed session present");
    assert_eq!(s.user_id, Some("7".to_string()), "new session is authed");
}
