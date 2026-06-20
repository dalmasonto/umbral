//! Sidebar permission-gate tests (gaps2 #83).
//!
//! Verifies that `sidebar_apps` (and the underlying `AdminRegistry::apps`)
//! filters models by the viewer's `view_<model>` permission when
//! `PermissionsPlugin` is installed:
//!
//! 1. A staff user WITHOUT `view_<model>` → model absent from the sidebar
//!    app list returned by `/admin/` (the dashboard HTML).
//! 2. A user WITH the permission → model present.
//! 3. Superuser bypasses the perm check → all models present.
//!
//! The "no PermissionsPlugin" baseline (all models visible regardless) is
//! covered in `sidebar_no_perm_gate.rs`, which boots a separate app without
//! the permissions plugin.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbra_admin::AdminPlugin;
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_permissions::PermissionsPlugin;
use umbra_sessions::SessionsPlugin;

// ---------------------------------------------------------------------------
// Test models
//
// Table names are bare (no underscores) so `table_app_label` returns "app",
// which matches the plugin name that `discover_models` assigns to models
// registered via `.model::<T>()`. The seeded codenames then agree with what
// `permcheck::codename` produces: "app.view_sbsecret" / "app.view_sbpublic".
// ---------------------------------------------------------------------------

/// A model the restricted user may NOT view.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "sbsecret")]
pub struct Secret {
    pub id: i64,
    pub data: String,
}

/// A model the restricted user MAY view.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "sbpublic")]
pub struct Public {
    pub id: i64,
    pub name: String,
}

// ---------------------------------------------------------------------------
// One shared boot per binary.
// ---------------------------------------------------------------------------

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sidebar_perm_gate.sqlite");
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
            .plugin(PermissionsPlugin)
            .plugin(AdminPlugin::default())
            .model::<Secret>()
            .model::<Public>()
            .build()
            .expect("App::build");

        let migration_dir = tempfile::tempdir().expect("migration dir");
        let migration_dir_path = migration_dir.path().to_path_buf();
        std::mem::forget(migration_dir);
        umbra::migrate::make_in(&migration_dir_path)
            .await
            .expect("make migrations");
        umbra::migrate::run_in(&migration_dir_path)
            .await
            .expect("run migrations");
        umbra_permissions::seed_standard_permissions_for_tests()
            .await
            .expect("seed permissions");

        let pool = umbra::db::pool();

        // `sb_restricted` — staff, NOT superuser, only has `view_sbpublic`.
        //
        // The sidebar filter checks `"<plugin>.view_<table>"` (same formula
        // as `permcheck::codename`). Both models land in the "app" plugin
        // (registered via `.model::<T>()`), so the required codenames are
        // "app.view_sbsecret" and "app.view_sbpublic".
        //
        // We insert ContentType + Permission rows manually (following
        // `phase3_action_permissions`'s pattern for custom perms) with
        // exactly those codenames so `permissions_userpermission` FK resolves.
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_contenttype (app_label, model) VALUES ('app', 'sbsecret')",
        )
        .execute(&pool)
        .await
        .expect("insert ct sbsecret");
        let ct_secret_id: i64 = sqlx::query_scalar(
            "SELECT id FROM permissions_contenttype WHERE app_label = 'app' AND model = 'sbsecret'",
        )
        .fetch_one(&pool)
        .await
        .expect("fetch ct_secret_id");

        sqlx::query(
            "INSERT OR IGNORE INTO permissions_contenttype (app_label, model) VALUES ('app', 'sbpublic')",
        )
        .execute(&pool)
        .await
        .expect("insert ct sbpublic");
        let ct_public_id: i64 = sqlx::query_scalar(
            "SELECT id FROM permissions_contenttype WHERE app_label = 'app' AND model = 'sbpublic'",
        )
        .fetch_one(&pool)
        .await
        .expect("fetch ct_public_id");

        // Insert the permission rows keyed on the exact codename permcheck uses.
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_permission (codename, content_type_id, name) \
             VALUES ('app.view_sbsecret', ?, 'Can view sbsecret')",
        )
        .bind(ct_secret_id)
        .execute(&pool)
        .await
        .expect("insert perm view_sbsecret");

        sqlx::query(
            "INSERT OR IGNORE INTO permissions_permission (codename, content_type_id, name) \
             VALUES ('app.view_sbpublic', ?, 'Can view sbpublic')",
        )
        .bind(ct_public_id)
        .execute(&pool)
        .await
        .expect("insert perm view_sbpublic");

        let restricted = create_user("sb_restricted", "sb_r@example.com", "pass123")
            .await
            .expect("create sb_restricted");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(restricted.id)
            .execute(&pool)
            .await
            .expect("set staff");
        // Grant only the public model's view permission.
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) \
             VALUES (?, 'app.view_sbpublic')",
        )
        .bind(restricted.id.to_string())
        .execute(&pool)
        .await
        .expect("grant view_sbpublic");

        // `sb_privileged` — staff, NOT superuser, has BOTH view permissions.
        let privileged = create_user("sb_privileged", "sb_p@example.com", "pass123")
            .await
            .expect("create sb_privileged");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(privileged.id)
            .execute(&pool)
            .await
            .expect("set privileged staff");
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) \
             VALUES (?, 'app.view_sbpublic')",
        )
        .bind(privileged.id.to_string())
        .execute(&pool)
        .await
        .expect("grant view_sbpublic to privileged");
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) \
             VALUES (?, 'app.view_sbsecret')",
        )
        .bind(privileged.id.to_string())
        .execute(&pool)
        .await
        .expect("grant view_sbsecret to privileged");

        // `sb_super` — superuser (no explicit perm rows; bypass is implicit).
        let superuser = create_user("sb_super", "sb_s@example.com", "pass123")
            .await
            .expect("create sb_super");
        sqlx::query("UPDATE auth_user SET is_staff = 1, is_superuser = 1 WHERE id = ?")
            .bind(superuser.id)
            .execute(&pool)
            .await
            .expect("set superuser");

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
                .header(header::COOKIE, format!("umbra_session={session}"))
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
                .header(header::COOKIE, format!("umbra_csrf_token={anon_cookie}"))
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
// Tests
// ---------------------------------------------------------------------------

/// (a) Staff user WITHOUT `view_sbsecret` → `sbsecret` is absent from the
/// sidebar HTML on the dashboard.
///
/// This is the primary regression: before the fix, the model appeared in the
/// sidebar even though the user could not view the changelist (403 on click).
#[tokio::test]
async fn sidebar_hides_model_when_view_perm_absent() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone(), "sb_restricted").await;
    let (_status, html) = send_get(router, "/admin/", &session).await;

    assert!(
        !html.contains("sbsecret"),
        "model without view perm must NOT appear in sidebar; \
         found 'sbsecret' in dashboard HTML"
    );
    // The model they CAN view should still be present.
    assert!(
        html.contains("sbpublic"),
        "model with view perm must appear in sidebar"
    );
}

/// (b) A user WITH both view permissions sees both models in the sidebar.
#[tokio::test]
async fn sidebar_shows_model_when_view_perm_present() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone(), "sb_privileged").await;
    let (_status, html) = send_get(router, "/admin/", &session).await;

    assert!(
        html.contains("sbsecret"),
        "privileged user must see sbsecret"
    );
    assert!(
        html.contains("sbpublic"),
        "privileged user must see sbpublic"
    );
}

/// (c) Superuser bypasses the perm check and sees every model.
#[tokio::test]
async fn sidebar_shows_all_models_for_superuser() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone(), "sb_super").await;
    let (_status, html) = send_get(router, "/admin/", &session).await;

    assert!(
        html.contains("sbsecret"),
        "superuser must see sbsecret"
    );
    assert!(
        html.contains("sbpublic"),
        "superuser must see sbpublic"
    );
}
