//! Phase 4 user-prefs tests.
//!
//! 1. GET /admin/api/prefs returns defaults for a new user.
//! 2. PUT /admin/api/prefs updates them.
//! 3. Invalid theme values are ignored (not rejected).

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbra_admin::AdminPlugin;
use umbra_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbra_sessions::SessionsPlugin;

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase4_prefs.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default())
            .build()
            .expect("App::build");

        let pool = umbra::db::pool();

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS auth_user (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL UNIQUE,\
                email TEXT NOT NULL,\
                password_hash TEXT NOT NULL,\
                is_active INTEGER NOT NULL DEFAULT 1,\
                is_staff INTEGER NOT NULL DEFAULT 0,\
                is_superuser INTEGER NOT NULL DEFAULT 0,\
                date_joined TEXT NOT NULL,\
                last_login TEXT\
            )",
        )
        .execute(&pool)
        .await
        .ok();

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS session (\
                id TEXT PRIMARY KEY,\
                user_id INTEGER,\
                data TEXT NOT NULL DEFAULT '{}',\
                created_at TEXT NOT NULL,\
                expires_at TEXT NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .ok();

        umbra_admin::models::ensure_tables(&pool)
            .await
            .expect("ensure_tables");

        app.into_router()
    })
    .await
}

async fn staff_cookie() -> String {
    let user = match create_user_with_flags(
        "prefs_user",
        "prefs@example.com",
        "pass123",
        true,
        false,
    )
    .await
    {
        Ok(u) => u,
        Err(_) => {
            let pool = umbra::db::pool();
            sqlx::query_as::<_, umbra_auth::AuthUser>(
                "SELECT id, username, email, password_hash, is_active, is_staff, is_superuser, date_joined, last_login \
                 FROM auth_user WHERE username = 'prefs_user'",
            )
            .fetch_one(&pool)
            .await
            .expect("lookup prefs_user")
        }
    };
    let tok = umbra_sessions::create_session(Some(user.id), None)
        .await
        .expect("session");
    format!("umbra_session={tok}")
}

// =========================================================================
// Tests
// =========================================================================

#[tokio::test]
async fn prefs_get_returns_defaults_for_new_user() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/api/prefs")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();

    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["theme"].as_str().unwrap_or(""), "dark");
    assert_eq!(json["density"].as_str().unwrap_or(""), "comfortable");
    assert_eq!(json["sidebar_collapsed"].as_bool().unwrap_or(true), false);
}

#[tokio::test]
async fn prefs_put_updates_and_get_reflects_change() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let put_req = Request::builder()
        .method("PUT")
        .uri("/admin/api/prefs")
        .header(header::COOKIE, cookie.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"theme":"light","density":"compact"}"#))
        .unwrap();

    let put_resp = router.clone().oneshot(put_req).await.unwrap();
    assert_eq!(put_resp.status(), StatusCode::OK);

    let get_req = Request::builder()
        .uri("/admin/api/prefs")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();

    let get_resp = router.clone().oneshot(get_req).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);

    let body = get_resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["theme"].as_str().unwrap_or(""), "light");
    assert_eq!(json["density"].as_str().unwrap_or(""), "compact");
}

#[tokio::test]
async fn prefs_put_ignores_invalid_theme_value() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let put_req = Request::builder()
        .method("PUT")
        .uri("/admin/api/prefs")
        .header(header::COOKIE, cookie.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"theme":"fuchsia"}"#))
        .unwrap();
    let resp = router.clone().oneshot(put_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
