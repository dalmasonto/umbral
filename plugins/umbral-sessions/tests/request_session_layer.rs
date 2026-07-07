//! Request-scoped session + `session_layer` lazy load/save (2a Task 2).
//!
//! Verifies the new `RequestSession` path end-to-end through the wrapped
//! router:
//!   (a) a handler that does NOT write the session leaves 0 rows + no
//!       Set-Cookie (lazy creation, gaps2 #46, preserved);
//!   (b) a handler that mutates the session via `current_mut(set_raw)`
//!       materialises exactly 1 row + emits Set-Cookie, and the written
//!       value round-trips;
//!   (c) a request carrying a live cookie + a non-writing handler adds no
//!       new row and sets no cookie.
//!
//! Own test binary (own ambient pool `OnceLock`) so the global
//! `Session::objects().count()` assertions are isolated from other suites
//! sharing a DB.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral::web::header;
use umbral_auth::{AuthPlugin, AuthUser};
use umbral_sessions::{COOKIE_NAME, Session, SessionsPlugin};

static BOOT: OnceCell<()> = OnceCell::const_new();

/// Serialises the three tests in this binary. They each assert on the
/// GLOBAL `Session::objects().count()` delta, so they must not overlap —
/// a concurrent test inserting a row would corrupt another's count.
/// (Separating each into its own binary, the `lazy_session.rs` route,
/// would also work; one mutex is lighter for three closely-related cases.)
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("request_session_layer.sqlite");
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
        .expect("create session");
    })
    .await;
}

/// (a) A non-writing handler must leave zero rows + no Set-Cookie. Lazy
/// creation (gaps2 #46) preserved under the new load/save flow.
#[tokio::test]
async fn non_writing_handler_persists_no_rows_no_cookie() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;

    boot().await;
    let _guard = SERIAL.lock().await;

    let before = Session::objects().count().await.expect("count before");

    let inner = axum::Router::new().route("/", get(|| async { "ok" }));
    let router = SessionsPlugin::default().wrap_router(inner);

    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(
        resp.headers().get(header::SET_COOKIE).is_none(),
        "a non-writing request must not set a session cookie",
    );

    let after = Session::objects().count().await.expect("count after");
    assert_eq!(
        after, before,
        "a non-writing request must persist no new session row",
    );
}

/// (b) A handler that mutates the session via `current_mut(set_raw)` must
/// materialise exactly one row + emit Set-Cookie, and the value must
/// round-trip from the stored row.
#[tokio::test]
async fn writing_handler_materialises_row_and_sets_cookie() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use serde_json::json;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;

    boot().await;
    let _guard = SERIAL.lock().await;

    async fn writer() -> impl IntoResponse {
        umbral_sessions::current_mut(|s| s.set_raw("k", json!(1))).expect("inside a request scope");
        "wrote"
    }

    let inner = axum::Router::new().route("/w", get(writer));
    let router = SessionsPlugin::default().wrap_router(inner);

    let before = Session::objects().count().await.expect("count before");

    let req = Request::builder().uri("/w").body(Body::empty()).unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("a written session must set its cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.starts_with(&format!("{COOKIE_NAME}=")));

    let after = Session::objects().count().await.expect("count after");
    assert_eq!(
        after,
        before + 1,
        "a writing request must materialise exactly one new row",
    );

    // The value round-trips from the stored row.
    let token = set_cookie
        .strip_prefix(&format!("{COOKIE_NAME}="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let s = umbral_sessions::read_session(&token)
        .await
        .unwrap()
        .expect("row present");
    let v: Option<i64> = umbral_sessions::get_data(&s, "k").expect("get_data");
    assert_eq!(v, Some(1), "the written value must round-trip");
}

/// (c) A request carrying a live cookie + a non-writing handler must add
/// no new row and set no cookie.
#[tokio::test]
async fn live_cookie_non_writing_handler_adds_no_row() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;
    use umbral_sessions::create_session;

    boot().await;
    let _guard = SERIAL.lock().await;

    // Establish a live session out-of-band, then ride it through a
    // non-writing handler.
    let token = create_session(None, None).await.expect("create_session");
    let before = Session::objects().count().await.expect("count before");

    let inner = axum::Router::new().route("/r", get(|| async { "read-only" }));
    let router = SessionsPlugin::default().wrap_router(inner);

    let req = Request::builder()
        .uri("/r")
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(
        resp.headers().get(header::SET_COOKIE).is_none(),
        "a live-cookie non-writing request must not re-issue a cookie",
    );

    let after = Session::objects().count().await.expect("count after");
    assert_eq!(
        after, before,
        "a live-cookie non-writing request must add no new row",
    );
}
