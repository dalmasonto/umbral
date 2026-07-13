//! gaps3 #12 — an unknown / unconfigured provider key is a CLIENT error (404),
//! not a server fault (500). Driven through the real mounted route + session
//! layer (login extracts a `SessionToken`, so the session middleware must be
//! present). Own test binary → own ambient pool `OnceLock`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;
use umbral::plugin::Plugin;
use umbral_oauth::OAuthPlugin;
use umbral_oauth::providers::GoogleProvider;
use umbral_sessions::SessionsPlugin;

async fn boot() {
    let settings = umbral::Settings::from_env().expect("figment defaults load");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("unknown_provider.sqlite");
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
        .plugin(SessionsPlugin::default())
        .build()
        .expect("App::build with SessionsPlugin");

    umbral::migrate::create_tables_for_tests()
        .await
        .expect("create the test schema");
}

#[tokio::test]
async fn unknown_provider_login_is_404_not_500() {
    boot().await;

    // `google` is registered; `nonexistent` is not.
    let oauth = OAuthPlugin::new("https://app.example.com")
        .provider(GoogleProvider::new("client123", "secret"));
    let router = SessionsPlugin::default().wrap_router(oauth.routes());

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/oauth/nonexistent/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot login");

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "an unregistered provider key must be 404, not 500"
    );
}
