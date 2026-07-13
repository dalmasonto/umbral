//! Verifies that sliding expiry OFF (the DEFAULT) means a request that
//! resolves a live session does NOT change `expires_at`.
//!
//! Must live in its own binary so `SLIDING_EXPIRY_ENABLED` is set to
//! `false` (via `SessionsPlugin::default()`) here, independently of the
//! `sliding_expiry_and_clearsessions.rs` binary that sets it to `true`.

use chrono::Duration;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral_auth::{AuthPlugin, AuthUser};
use umbral_sessions::{SessionsPlugin, create_session, read_session};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sliding_off.sqlite");
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
            // Default: sliding_expiry = false. Sets SLIDING_EXPIRY_ENABLED = false.
            .plugin(SessionsPlugin::default())
            .build()
            .expect("App::build with SessionsPlugin (sliding OFF)");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

/// Sliding expiry OFF (default): a request through `session_layer` must
/// NOT change the session's `expires_at`. This is the zero-extra-write
/// guarantee for operators who haven't opted in.
#[tokio::test]
async fn sliding_expiry_off_does_not_change_expires_at() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;
    use umbral::web::header;
    use umbral_sessions::{COOKIE_NAME, SessionToken, set_data};

    boot().await;

    let token = create_session(None, Some(Duration::seconds(3600)))
        .await
        .expect("create session");

    let before = read_session(&token)
        .await
        .expect("read before")
        .expect("session exists")
        .expires_at;

    // Sleep briefly to give any unintended bump a chance to be detectable.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    async fn write_handler(
        axum::Extension(SessionToken(t)): axum::Extension<SessionToken>,
    ) -> &'static str {
        let _ = set_data(&t, "ping", &true).await;
        "ok"
    }

    let inner = axum::Router::new().route("/", get(write_handler));
    let router = SessionsPlugin::default().wrap_router(inner);

    let req = Request::builder()
        .uri("/")
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    let after = read_session(&token)
        .await
        .expect("read after")
        .expect("session still exists")
        .expires_at;

    // Tolerate sub-millisecond rounding from SQLite TEXT storage but
    // require that `after` did NOT advance by a meaningful margin.
    // A sliding bump would push it ~14 days forward; any advance > 1s is wrong.
    let drift = (after - before).num_seconds().abs();
    assert!(
        drift <= 1,
        "sliding expiry OFF: expires_at must not change; drift was {drift}s\n  before={before:?}\n  after= {after:?}"
    );
}
