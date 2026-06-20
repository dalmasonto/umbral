//! gaps2 #33 — `restore_last_path` flag=true (the default).
//!
//! (a) `/admin/` + stored `last_path` → 303 redirect to it.
//! (b) `/admin/?dashboard=1` always renders the dashboard (200).
//! (c) No stored `last_path` → `/admin/` renders the dashboard (200).
//! (d) The "Home" breadcrumb carries `?dashboard=1` (escape affordance).
//!
//! Split from the flag=false cases (gaps2_33_rlp_off.rs) because the
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

use umbra_admin::{AdminModel, AdminPlugin};
use umbra_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbra_sessions::SessionsPlugin;

// Model registered so `/admin/rlp_on_item/` is a valid changelist route.
// Must be `pub` per project convention (test model structs must be public).
#[derive(Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct RlpOnItem {
    pub id: i64,
    pub name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("gaps2_33_rlp_on.sqlite");
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

        // AdminPlugin::default() — restore_last_path is true by default.
        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(AdminModel::new("rlp_on_item")))
            .model::<RlpOnItem>()
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
            "CREATE TABLE IF NOT EXISTS rlp_on_item (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL DEFAULT ''\
            )",
        )
        .execute(&pool)
        .await
        .ok();

        umbra_admin::models::ensure_tables_for_tests(&pool)
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
            let pool = umbra::db::pool();
            sqlx::query_as::<_, AuthUser>(
                "SELECT id, username, email, password_hash, is_active, is_staff, \
                 is_superuser, date_joined, last_login \
                 FROM auth_user WHERE username = ?",
            )
            .bind(username)
            .fetch_one(&pool)
            .await
            .expect("lookup user")
        }
    };
    let tok = umbra_sessions::create_session(Some(user.id.to_string()), None)
        .await
        .expect("session");
    (user.id, format!("umbra_session={tok}"))
}

// ── (a) stored last_path → /admin/ redirects (303) ──────────────────────────

#[tokio::test]
async fn restore_on_redirects_to_last_path_when_stored() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let (uid, cookie) = staff_cookie("rlp_on_user", "rlp_on@example.com").await;

    umbra_admin::models::set_last_path(uid, "/admin/rlp_on_item/")
        .await
        .expect("seed last_path");

    let req = Request::builder()
        .uri("/admin/")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();

    // axum's Redirect::to emits 303 See Other.
    assert!(
        resp.status() == StatusCode::SEE_OTHER || resp.status() == StatusCode::FOUND,
        "flag=true + stored last_path → /admin/ should redirect, got {}",
        resp.status()
    );
    let loc = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        loc, "/admin/rlp_on_item/",
        "redirect destination should be the stored last_path"
    );
}

// ── (b) ?dashboard=1 bypasses the redirect ───────────────────────────────────

#[tokio::test]
async fn restore_on_dashboard_escape_via_query_param() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let (uid, cookie) = staff_cookie("rlp_on_escape", "rlp_on_escape@example.com").await;

    // Ensure a last_path is stored so the redirect WOULD fire without ?dashboard=1.
    umbra_admin::models::set_last_path(uid, "/admin/rlp_on_item/")
        .await
        .expect("seed last_path");

    let req = Request::builder()
        .uri("/admin/?dashboard=1")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "?dashboard=1 should render the dashboard (200), not redirect"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("umbra-admin"),
        "expected dashboard HTML in response body"
    );
}

// ── (c) no stored last_path → /admin/ renders dashboard ──────────────────────

#[tokio::test]
async fn restore_on_no_redirect_when_no_last_path() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let (uid, cookie) =
        staff_cookie("rlp_on_nolast", "rlp_on_nolast@example.com").await;

    // Wipe any prefs so last_path is absent.
    let pool = umbra::db::pool();
    sqlx::query("DELETE FROM admin_user_pref WHERE user_id = ?")
        .bind(uid)
        .execute(&pool)
        .await
        .ok();

    let req = Request::builder()
        .uri("/admin/")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "flag=true + no stored last_path → /admin/ should render dashboard (200)"
    );
}

// ── (d) Home breadcrumb carries ?dashboard=1 ─────────────────────────────────

#[tokio::test]
async fn sidebar_home_link_carries_dashboard_escape_when_flag_on() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let (_uid, cookie) =
        staff_cookie("rlp_on_sidebar", "rlp_on_sidebar@example.com").await;

    // Force the dashboard via ?dashboard=1 to get HTML back rather than a redirect.
    let req = Request::builder()
        .uri("/admin/?dashboard=1")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);

    // The "Home" breadcrumb (in base.html) should link to `/admin/?dashboard=1`
    // so the user can reach the dashboard in one click when restore is active.
    assert!(
        html.contains("/admin/?dashboard=1"),
        "Home breadcrumb should carry ?dashboard=1 when restore_last_path=true"
    );
}
