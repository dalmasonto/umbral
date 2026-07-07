//! gaps2 #33 — `restore_last_path` flag=false.
//!
//! (a) `/admin/` always renders the dashboard (200) even with a stored
//!     `last_path` — the redirect is suppressed.
//! (b) The changelist handler does NOT write `last_path` — no dead data.
//! (c) The "Home" breadcrumb is plain (no `?dashboard=1` suffix).
//!
//! Split from the flag=true cases (gaps2_33_rlp_on.rs) because the
//! process-global `BRANDING` / `ENGINE` / settings OnceLocks can only be
//! sealed once per process — each test binary gets exactly one AdminPlugin.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbral_sessions::SessionsPlugin;

// Model registered so `/admin/rlp_off_item/` is a valid changelist route.
// Must be `pub` per project convention (test model structs must be public).
#[derive(Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct RlpOffItem {
    pub id: i64,
    pub name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("gaps2_33_rlp_off.sqlite");
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
            .expect("pool");

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(
                AdminPlugin::default()
                    .restore_last_path(false)
                    .register(AdminModel::new("rlp_off_item")),
            )
            .model::<RlpOffItem>()
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();

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
                last_login TEXT,\
                email_verified_at TEXT\
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
            "CREATE TABLE IF NOT EXISTS rlp_off_item (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL DEFAULT ''\
            )",
        )
        .execute(&pool)
        .await
        .ok();

        umbral_admin::models::ensure_tables_for_tests(&pool)
            .await
            .expect("ensure_tables");

        app.into_router()
    })
    .await
}

async fn staff_cookie(username: &str, email: &str) -> (i64, String) {
    let user = match create_user_with_flags(username, email, "pass123", true, false).await {
        Ok(u) => u,
        Err(_) => {
            let pool = umbral::db::pool();
            sqlx::query_as::<_, AuthUser>(
                "SELECT id, username, email, password_hash, is_active, is_staff, \
                 is_superuser, date_joined, last_login, email_verified_at \
                 FROM auth_user WHERE username = ?",
            )
            .bind(username)
            .fetch_one(&pool)
            .await
            .expect("lookup user")
        }
    };
    let tok = umbral_sessions::create_session(Some(user.id.to_string()), None)
        .await
        .expect("session");
    (user.id, format!("umbral_session={tok}"))
}

// ── (a) flag=false → /admin/ always renders dashboard ────────────────────────

#[tokio::test]
async fn restore_off_always_renders_dashboard_even_with_stored_last_path() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let (uid, cookie) = staff_cookie("rlp_off_user", "rlp_off@example.com").await;

    // Seed a last_path — with the flag off the index must ignore it.
    umbral_admin::models::set_last_path(uid, "/admin/rlp_off_item/")
        .await
        .expect("seed last_path");

    let req = Request::builder()
        .uri("/admin/")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "flag=false → /admin/ should always render the dashboard (200), not redirect"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("umbral-admin"),
        "expected dashboard HTML, not a redirect response body"
    );
}

// ── (b) flag=false → changelist does NOT write last_path ─────────────────────

#[tokio::test]
async fn restore_off_changelist_does_not_write_last_path() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let (uid, cookie) = staff_cookie("rlp_off_writer", "rlp_off_writer@example.com").await;

    // Wipe prefs so we start with no last_path.
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM admin_user_pref WHERE user_id = ?")
        .bind(uid)
        .execute(&pool)
        .await
        .ok();

    // Visit the changelist — should render 200 regardless of flag.
    let req = Request::builder()
        .uri("/admin/rlp_off_item/")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "changelist should render (200) regardless of flag"
    );

    // With flag=false, the changelist handler must NOT have written last_path.
    let stored = umbral_admin::models::get_last_path(uid)
        .await
        .expect("read last_path");
    assert!(
        stored.is_none(),
        "flag=false → changelist must not write last_path, got: {stored:?}"
    );
}

// ── (c) Home breadcrumb is plain (no ?dashboard=1) ───────────────────────────

#[tokio::test]
async fn sidebar_home_link_plain_when_flag_off() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let (_uid, cookie) = staff_cookie("rlp_off_sidebar", "rlp_off_sidebar@example.com").await;

    let req = Request::builder()
        .uri("/admin/")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);

    // When the flag is off, the "Home" link must NOT carry ?dashboard=1.
    assert!(
        !html.contains("/admin/?dashboard=1"),
        "Home breadcrumb must not carry ?dashboard=1 when restore_last_path=false"
    );
}
