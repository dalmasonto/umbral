//! Phase 4 command palette tests.
//!
//! 1. GET /admin/api/palette returns HTML.
//! 2. The fragment contains jump links for registered models.
//! 3. Fixed commands (Toggle theme, Logout) are present.
//! 4. Unauthenticated request gets a redirect.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbra_admin::{AdminModel, AdminPlugin};
use umbra_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbra_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Article {
    id: i64,
    title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase4_palette.sqlite");
        std::mem::forget(tmp);
        let pool_obj = SqlitePoolOptions::new()
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
            .database("default", pool_obj)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(
                AdminPlugin::default().register(
                    AdminModel::new("article")
                        .label("Article")
                        .icon("file-text"),
                ),
            )
            .model::<Article>()
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
                user_id TEXT,\
                data TEXT NOT NULL DEFAULT '{}',\
                created_at TEXT NOT NULL,\
                expires_at TEXT NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .ok();

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS article (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .ok();

        app.into_router()
    })
    .await
}

async fn staff_cookie() -> String {
    let user = match create_user_with_flags(
        "palette_user",
        "pal@example.com",
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
                 FROM auth_user WHERE username = 'palette_user'",
            )
            .fetch_one(&pool)
            .await
            .expect("lookup palette_user")
        }
    };
    let tok = umbra_sessions::create_session(Some(user.id.to_string()), None)
        .await
        .expect("session");
    format!("umbra_session={tok}")
}

// =========================================================================
// Tests
// =========================================================================

#[tokio::test]
async fn palette_fragment_returns_ok() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/api/palette")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn palette_includes_registered_model() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/api/palette")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("/admin/article/") || html.contains("article/"),
        "expected article jump link in palette; got: {html}"
    );
}

#[tokio::test]
async fn palette_includes_fixed_commands() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/api/palette")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("Toggle theme"),
        "expected Toggle theme command"
    );
    assert!(html.contains("Logout"), "expected Logout command");
}

#[tokio::test]
async fn palette_unauthenticated_redirects() {
    let _guard = LOCK.lock().await;
    let router = boot().await;

    let req = Request::builder()
        .uri("/admin/api/palette")
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let s = resp.status();
    assert!(
        s == StatusCode::SEE_OTHER || s == StatusCode::FOUND || s == StatusCode::TEMPORARY_REDIRECT,
        "expected redirect, got {s}"
    );
}
