//! Sidebar no-PermissionsPlugin baseline test (gaps2 #83).
//!
//! Verifies that when `PermissionsPlugin` is NOT installed, the sidebar
//! permission gate is a no-op: every staff user sees all registered models,
//! preserving the behaviour that existed before the #83 fix.
//!
//! This lives in a separate test binary from `sidebar_perm_gate.rs` because
//! each binary initialises `umbral::App` once via a process-global `OnceLock`;
//! a single binary cannot boot two apps with different plugin sets.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_admin::AdminPlugin;
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

// ---------------------------------------------------------------------------
// Test models (table names distinct from sidebar_perm_gate to avoid clashes
// when all test binaries are built together).
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nopgsecret")]
pub struct NopgSecret {
    pub id: i64,
    pub data: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nopgpublic")]
pub struct NopgPublic {
    pub id: i64,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Shared boot — one App per binary.
// ---------------------------------------------------------------------------

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sidebar_no_perm_gate.sqlite");
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

        // No PermissionsPlugin — only auth + sessions + admin.
        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default())
            .model::<NopgSecret>()
            .model::<NopgPublic>()
            .build()
            .expect("App::build (no perms)");

        let migration_dir = tempfile::tempdir().expect("migration dir");
        let migration_dir_path = migration_dir.path().to_path_buf();
        std::mem::forget(migration_dir);
        umbral::migrate::make_in(&migration_dir_path)
            .await
            .expect("make migrations");
        umbral::migrate::run_in(&migration_dir_path)
            .await
            .expect("run migrations");

        let pool = umbral::db::pool();

        // One plain staff user — no permission tables exist.
        let staff = create_user("nopg_staff", "nopg@example.com", "pass123")
            .await
            .expect("create nopg_staff");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("set staff");

        app.into_router()
    })
    .await
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn send_get(router: axum::Router, uri: &str, session: &str) -> (StatusCode, String) {
    let resp = router
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, format!("umbral_session={session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker).unwrap_or(0);
    let window = &html[pos..(pos + 200).min(html.len())];
    let val = r#"value=""#;
    let vpos = window.find(val).unwrap_or(0);
    let after = &window[vpos + val.len()..];
    let end = after.find('"').unwrap_or(0);
    after[..end].to_string()
}

fn extract_cookie(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .unwrap_or_default()
}

async fn login(router: axum::Router, username: &str) -> String {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("get login");
    let anon_raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let anon_cookie = extract_cookie(&anon_raw);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let csrf = extract_csrf(&html);
    let form = serde_urlencoded::to_string([
        ("username", username),
        ("password", "pass123"),
        ("csrf_token", csrf.as_str()),
        ("next", "/admin/"),
    ])
    .unwrap();
    let resp2 = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, format!("umbral_csrf_token={anon_cookie}"))
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .expect("post login");
    resp2
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(extract_cookie)
        .unwrap_or(anon_cookie)
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

/// No `PermissionsPlugin` installed → all models appear in the sidebar for
/// any staff user (unchanged behaviour; permission check is a no-op).
///
/// This confirms the fix doesn't break apps that haven't opted into
/// the permissions plugin.
#[tokio::test]
async fn sidebar_shows_all_models_when_no_plugin() {
    let router = boot().await.clone();
    let session = login(router.clone(), "nopg_staff").await;
    let (_status, html) = send_get(router, "/admin/", &session).await;

    assert!(
        html.contains("nopgsecret"),
        "without PermissionsPlugin, nopgsecret must appear in sidebar"
    );
    assert!(
        html.contains("nopgpublic"),
        "without PermissionsPlugin, nopgpublic must appear in sidebar"
    );
}
