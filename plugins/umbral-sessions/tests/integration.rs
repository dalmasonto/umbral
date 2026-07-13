//! End-to-end coverage for umbral-sessions: create a session via the
//! helper, look it up via the cookie header, hydrate the user via
//! `current_user`, destroy on logout.
//!
//! Boots once via OnceCell with AuthPlugin + SessionsPlugin
//! registered. Tempfile-backed sqlite so every pool connection sees
//! the same database, same pattern umbral-auth + umbral-admin tests
//! use.

use std::path::PathBuf;

use chrono::Duration;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral::web::{HeaderMap, header};
use umbral_auth::current_user;
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::{
    COOKIE_NAME, SessionsPlugin, clear_cookie_header, cookie_from_headers, create_session,
    destroy_session, get_data, read_session, set_cookie_header, set_data,
};

static BOOT: OnceCell<i64> = OnceCell::const_new();

async fn boot() -> i64 {
    *BOOT
        .get_or_init(|| async {
            let settings = umbral::Settings::from_env().expect("figment defaults load");
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("sessions_integration.sqlite");
            std::mem::forget(tmp);
            let pool = SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(
                    SqliteConnectOptions::new()
                        .busy_timeout(std::time::Duration::from_secs(5))
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

            umbral::migrate::create_tables_for_tests()
                .await
                .expect("create the test schema");

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
    let token = create_session(Some(user_id.to_string()), None)
        .await
        .expect("create");
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
    assert_eq!(s.user_id, Some(user_id.to_string()));
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
    let id = create_session(Some(user_id.to_string()), Some(Duration::seconds(-1)))
        .await
        .expect("create");
    let result = read_session(&id).await.expect("read");
    assert!(
        result.is_none(),
        "expired session should return None; got {result:?}"
    );

    // Confirm the row was actually deleted.
    let pool = umbral::db::pool();
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
    let id = create_session(Some(user_id.to_string()), None)
        .await
        .expect("create");
    assert!(read_session(&id).await.unwrap().is_some());
    destroy_session(&id).await.expect("destroy");
    assert!(read_session(&id).await.unwrap().is_none());
}

/// cookie_from_headers parses a Cookie header looking for the
/// umbral_session value among other cookies.
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
    // `Secure` is environment-gated (off in Dev so session cookies work
    // over plain http://localhost; on in Prod). The test process's
    // ambient environment isn't fixed here, so we assert the always-on
    // flags only.
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
    let id = create_session(Some(user_id.to_string()), None)
        .await
        .expect("create");

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
    let id = create_session(Some(user_id.to_string()), None)
        .await
        .expect("create");
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
    let id = create_session(Some(user_id.to_string()), None)
        .await
        .expect("create");

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
    let pool = umbral::db::pool();
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("fetch user");
    let before_login = user.last_login;

    let mut response_headers = HeaderMap::new();
    let token = umbral_auth::login(&mut response_headers, &user)
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
    // `Secure` is environment-gated (off in Dev) — see
    // `set_cookie_header_carries_secure_defaults`.

    // The session row is readable via the returned token.
    let session = read_session(&token).await.expect("read").expect("present");
    assert_eq!(session.user_id, Some(user_id.to_string()));

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
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap();
    let mut login_headers = HeaderMap::new();
    let token = umbral_auth::login(&mut login_headers, &user)
        .await
        .expect("login");

    // Simulate the browser sending the cookie back.
    let mut request_headers = HeaderMap::new();
    request_headers.insert(
        header::COOKIE,
        format!("{COOKIE_NAME}={token}").parse().unwrap(),
    );

    let mut response_headers = HeaderMap::new();
    umbral_sessions::logout(&request_headers, &mut response_headers)
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
    umbral_sessions::logout(&request_headers, &mut response_headers)
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
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap();
    let mut login_headers = HeaderMap::new();
    let token = umbral_auth::login(&mut login_headers, &user).await.unwrap();

    let req = http::Request::builder()
        .uri("/")
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(())
        .unwrap();
    let (mut parts, _) = req.into_parts();
    let umbral_auth::OptionalUser(opt) =
        umbral_auth::OptionalUser::from_request_parts(&mut parts, &())
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
    let umbral_auth::OptionalUser(opt) =
        umbral_auth::OptionalUser::from_request_parts(&mut parts, &())
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
    let err = umbral_auth::User::from_request_parts(&mut parts, &())
        .await
        .expect_err("anonymous should 401");
    assert_eq!(err.0, http::StatusCode::UNAUTHORIZED);
}

// =====================================================================
// Messages framework — flash messages over the session data store.
// =====================================================================

#[tokio::test]
async fn messages_add_and_drain_round_trip() {
    use umbral_sessions::{MessageLevel, Messages};
    let user_id = boot().await;
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap();
    let mut login_headers = HeaderMap::new();
    let token = umbral_auth::login(&mut login_headers, &user).await.unwrap();

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
    use umbral_sessions::Messages;
    let user_id = boot().await;
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap();
    let mut login_headers = HeaderMap::new();
    let token = umbral_auth::login(&mut login_headers, &user).await.unwrap();

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
    use umbral_sessions::Messages;
    let _user_id = boot().await;
    let msgs = Messages::new(None);
    assert!(!msgs.is_active());
    // These all silently no-op.
    msgs.success("vanishing").await;
    msgs.error("also vanishing").await;
    // drain returns empty.
    assert!(msgs.drain().await.is_empty());
}

// =====================================================================
// Anonymous sessions, the SessionLayer middleware, and the
// login-fixation defense.
// =====================================================================

/// `create_session(None, ...)` produces a session row with
/// `user_id = NULL`. That's the anonymous-session base case the
/// middleware uses on first visit.
#[tokio::test]
async fn create_session_with_none_produces_anonymous_row() {
    let _ = boot().await;
    let token = create_session(None, None).await.expect("create anon");
    let s = read_session(&token).await.unwrap().unwrap();
    assert!(s.user_id.is_none(), "anonymous session has no user_id");
    assert_eq!(s.data, "{}");
}

/// Drive a full request through an axum Router wrapped by
/// `SessionsPlugin`. Lazy semantics (gaps2 #46): a handler that WRITES
/// the session on the first (cookie-less) visit materialises an
/// anonymous row and gets a Set-Cookie; the second visit reuses the
/// same session. (A non-writing first visit leaves no row / no cookie
/// — that's covered in `tests/lazy_session.rs`.)
#[tokio::test]
async fn router_through_sessions_plugin_creates_anon_session_on_first_write() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::{Extension, response::IntoResponse};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use umbral_sessions::SessionToken;

    let _ = boot().await;

    // Handler writes the session using the middleware-injected token,
    // so the row materialises lazily on first write.
    async fn writer(Extension(SessionToken(token)): Extension<SessionToken>) -> impl IntoResponse {
        set_data(&token, "visited", &true).await.expect("set_data");
        "ok"
    }

    // Build a tiny router and let the plugin wrap it (auto layer = on).
    let inner = axum::Router::new().route("/", get(writer));
    let plugin = SessionsPlugin::default();
    use umbral::plugin::Plugin;
    let router = plugin.wrap_router(inner);

    // First request: no cookie. The handler writes the session, so a
    // Set-Cookie is expected in the response.
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("a written session should set its cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.starts_with(&format!("{COOKIE_NAME}=")));

    // The token from the response cookie should resolve to a real
    // anonymous session row.
    let token = set_cookie
        .strip_prefix(&format!("{COOKIE_NAME}="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let _bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let session = read_session(&token).await.unwrap().expect("present");
    assert!(session.user_id.is_none(), "should be anonymous");

    // Second request: send the cookie back. No new Set-Cookie
    // expected (session already exists, the cookie's already with the
    // client).
    let req2 = Request::builder()
        .uri("/")
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(Body::empty())
        .unwrap();
    let resp2 = router.oneshot(req2).await.unwrap();
    assert!(
        resp2.headers().get(header::SET_COOKIE).is_none(),
        "existing session should not trigger another Set-Cookie",
    );
}

/// Regression: when a handler sets its own `Set-Cookie` (the
/// shape `login_with_request` uses to rotate the token after
/// credential check — session-fixation defense), the session_layer
/// must NOT overwrite it with the anonymous cookie it minted at
/// request entry. The previous behaviour silently clobbered every
/// cookie-based login: the row got `user_id = Some(u.id)`, but the
/// response carried the anonymous cookie, so the very next request
/// looked unauthenticated to the server.
#[tokio::test]
async fn session_layer_does_not_clobber_handler_set_cookie() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use tower::ServiceExt;

    let _ = boot().await;

    let handler_cookie = format!("{COOKIE_NAME}=handler-mint; Path=/; HttpOnly");
    let cookie_for_handler = handler_cookie.clone();
    let inner = axum::Router::new().route(
        "/",
        get(move || {
            let value = cookie_for_handler.clone();
            async move {
                let mut response = "ok".into_response();
                response
                    .headers_mut()
                    .insert(header::SET_COOKIE, value.parse().unwrap());
                response
            }
        }),
    );
    let plugin = SessionsPlugin::default();
    use umbral::plugin::Plugin;
    let router = plugin.wrap_router(inner);

    // Request without an inbound cookie: the layer would normally
    // mint an anonymous session and stamp Set-Cookie on the way
    // back. The handler beat it to the punch — the layer must back
    // off.
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("handler-set Set-Cookie should survive")
        .to_str()
        .unwrap();
    assert_eq!(
        set_cookie, handler_cookie,
        "the layer overwrote the handler's Set-Cookie; got {set_cookie:?}",
    );
}

/// Messages that an anonymous user adds (e.g. a "you signed up
/// successfully" toast before redirect to /login) survive across
/// requests — the whole point of having anonymous sessions.
#[tokio::test]
async fn anonymous_user_can_write_and_drain_flash_messages() {
    use umbral_sessions::Messages;
    let _ = boot().await;
    let token = create_session(None, None).await.expect("create anon");

    let msgs = Messages::new(Some(token.clone()));
    assert!(msgs.is_active());
    msgs.success("Welcome!").await;

    // Simulate a follow-up request by constructing a fresh handle
    // with the same token.
    let msgs2 = Messages::new(Some(token));
    let drained = msgs2.drain().await;
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].text, "Welcome!");
}

/// Session-fixation defense: when a user logs in, the existing
/// anonymous session is destroyed and a new authenticated session
/// is created with a fresh token. A pre-login attacker holding the
/// anonymous cookie can't ride it into the authed account.
#[tokio::test]
async fn login_destroys_anonymous_session_and_issues_new_token() {
    let user_id = boot().await;
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap();

    // Simulate the browser arriving with an anonymous session.
    let anon_token = create_session(None, None).await.expect("create anon");
    let mut req_headers = HeaderMap::new();
    req_headers.insert(
        header::COOKIE,
        format!("{COOKIE_NAME}={anon_token}").parse().unwrap(),
    );

    let mut resp_headers = HeaderMap::new();
    let new_token = umbral_auth::login_with_request(&req_headers, &mut resp_headers, &user)
        .await
        .expect("login_with_request");

    // The anon token is gone (fixation defense).
    assert!(
        read_session(&anon_token).await.unwrap().is_none(),
        "anonymous session must be destroyed on login"
    );
    // The new authed token is alive.
    let new_session = read_session(&new_token).await.unwrap().unwrap();
    assert_eq!(new_session.user_id, Some(user_id.to_string()));
    // They differ — fresh token = fresh cookie = no fixation surface.
    assert_ne!(anon_token, new_token);
}

/// Flash messages added before login should survive the
/// login-induced token regeneration (the data column transfers).
#[tokio::test]
async fn flash_messages_survive_login_token_regeneration() {
    use umbral_sessions::Messages;
    let user_id = boot().await;
    let user: AuthUser = sqlx::query_as("SELECT * FROM auth_user WHERE id = ?")
        .bind(user_id)
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap();

    let anon_token = create_session(None, None).await.unwrap();
    Messages::new(Some(anon_token.clone()))
        .info("Saved your draft before login")
        .await;

    let mut req_headers = HeaderMap::new();
    req_headers.insert(
        header::COOKIE,
        format!("{COOKIE_NAME}={anon_token}").parse().unwrap(),
    );
    let mut resp_headers = HeaderMap::new();
    let new_token = umbral_auth::login_with_request(&req_headers, &mut resp_headers, &user)
        .await
        .unwrap();

    // The new authed session carries the pre-login flash.
    let drained = Messages::new(Some(new_token)).drain().await;
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].text, "Saved your draft before login");
}
