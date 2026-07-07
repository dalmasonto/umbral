//! Lazy session creation — core reproduction (gaps2 #46).
//!
//! A session row is created lazily on first WRITE, not
//! eagerly on every cookie-less request. A fresh browser firing
//! several parallel cookie-less requests (page + favicon + assets)
//! must NOT leave a pile of anonymous rows behind — none of those
//! requests writes the session, so none materialises a row.
//!
//! This file is its OWN test binary (separate process → its own
//! ambient pool `OnceLock`), so the global `Session::objects().count()`
//! assertion isn't polluted by other tests writing the shared DB.
//! It contains exactly one test for that reason.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use umbral::web::header;
use umbral_auth::{AuthPlugin, AuthUser};
use umbral_sessions::{COOKIE_NAME, Session, SessionsPlugin};

async fn boot() {
    let settings = umbral::Settings::from_env().expect("figment defaults load");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("lazy_session.sqlite");
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
}

/// Reproduction / core fix: three sequential cookie-less requests
/// through `session_layer` to a handler that does NOT write the
/// session must leave the `session` table with ZERO rows and emit no
/// Set-Cookie. Before the fix the eager `create_session(None, None)`
/// on entry left 3 rows; after the fix, 0.
#[tokio::test]
async fn cookieless_requests_to_non_writing_handler_persist_no_rows() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;

    boot().await;

    let before = Session::objects().count().await.expect("count before");
    assert_eq!(before, 0, "fresh DB starts empty");

    let inner = axum::Router::new().route("/", get(|| async { "ok" }));
    let router = SessionsPlugin::default().wrap_router(inner);

    for _ in 0..3 {
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert!(
            resp.headers().get(header::SET_COOKIE).is_none(),
            "a non-writing cookie-less request must not set a session cookie",
        );
    }

    let after = Session::objects().count().await.expect("count after");
    assert_eq!(
        after, 0,
        "three cookie-less non-writing requests must persist zero session rows; got {after}",
    );

    let _ = COOKIE_NAME;
}
