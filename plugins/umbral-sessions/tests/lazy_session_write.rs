//! Lazy session creation — materialise-on-write (gaps2 #46).
//!
//! Own test binary (own ambient pool) so the global
//! `Session::objects().count()` assertions are isolated from other
//! tests sharing a DB.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use umbral::web::header;
use umbral_auth::{AuthPlugin, AuthUser};
use umbral_sessions::{COOKIE_NAME, Session, SessionToken, SessionsPlugin, read_session, set_data};

async fn boot() {
    let settings = umbral::Settings::from_env().expect("figment defaults load");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("lazy_session_write.sqlite");
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
}

/// A request to a handler that writes the session materialises
/// EXACTLY one row and the response carries a Set-Cookie. A second
/// request carrying that cookie reuses the same row (no duplicate) and
/// the written value round-trips.
#[tokio::test]
async fn write_handler_materialises_exactly_one_row_and_sets_cookie() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::{Extension, response::IntoResponse};
    use tower::ServiceExt;
    use umbral::plugin::Plugin;

    boot().await;
    assert_eq!(Session::objects().count().await.unwrap(), 0, "fresh DB");

    async fn writer(Extension(SessionToken(token)): Extension<SessionToken>) -> impl IntoResponse {
        set_data(&token, "cart_id", &7i64).await.expect("set_data");
        "wrote"
    }

    let inner = axum::Router::new().route("/", get(writer));
    let router = SessionsPlugin::default().wrap_router(inner);

    // First request: no cookie -> handler writes -> row materialises +
    // Set-Cookie emitted.
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("a written session must set its cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.starts_with(&format!("{COOKIE_NAME}=")));

    assert_eq!(
        Session::objects().count().await.unwrap(),
        1,
        "first write must materialise exactly one row",
    );

    let token = set_cookie
        .strip_prefix(&format!("{COOKIE_NAME}="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    // The value round-trips.
    let s = read_session(&token).await.unwrap().expect("present");
    let cart: Option<i64> = umbral_sessions::get_data(&s, "cart_id").expect("get");
    assert_eq!(cart, Some(7));
    assert!(s.user_id.is_none(), "anonymous lazy session has no user_id");

    // Second request carrying the cookie: same row reused, no
    // duplicate even though the handler writes again.
    let req2 = Request::builder()
        .uri("/")
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(Body::empty())
        .unwrap();
    let resp2 = router.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), http::StatusCode::OK);

    assert_eq!(
        Session::objects().count().await.unwrap(),
        1,
        "a returning request with the cookie must reuse the row, not duplicate it",
    );
}
