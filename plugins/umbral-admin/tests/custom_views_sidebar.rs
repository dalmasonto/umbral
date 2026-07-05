//! Sidebar integration for custom admin views (Task 6).
//!
//! Verifies:
//! - A registered non-hidden view appears as a sidebar link on the dashboard.
//! - A `.hide()`-d view's href is absent from the sidebar.
//! - A `.with_permission(codename)` view is filtered out of the sidebar for a
//!   staff user who lacks that codename, and shown for one who holds it
//!   (the security-relevant path).
//!
//! `PermissionsPlugin` is installed so `permissions_installed()` is true and
//! `has_codename` actually checks the DB — without it, the graceful no-op
//! returns true and the permission-filter assertions would be vacuous.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{AdminPlugin, AdminView};
use umbral_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbral_permissions::PermissionsPlugin;
use umbral_sessions::SessionsPlugin;

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

/// The custom-view permission codename gated test 3/4 turn on. It is a
/// free-form codename (not model-bound), exactly the shape a real app passes
/// to `AdminView::with_permission("reports.view_secret")`.
const SECRET_CODENAME: &str = "reports.view_secret";

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("custom_views_sidebar.sqlite");
        std::mem::forget(tmp);
        let pool_obj = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool_obj)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            // Installed so `has_codename` actually checks the DB — the
            // permission-filter assertions would be vacuous without it.
            .plugin(PermissionsPlugin)
            .plugin(
                AdminPlugin::default()
                    // Visible, non-gated view — must appear in sidebar for any staff.
                    .view(AdminView::new("reports/sales", "Sales report").with_icon("bar-chart"))
                    // Hidden view — must NOT appear in sidebar.
                    .view(AdminView::new("internal/debug", "Debug panel").hide())
                    // Permission-gated view — only staff holding SECRET_CODENAME see it.
                    .view(
                        AdminView::new("reports/secret", "Secret report")
                            .with_permission(SECRET_CODENAME),
                    ),
            )
            .build()
            .expect("App::build");

        // Create every registered plugin table (auth, sessions, permissions,
        // admin) through the real migration engine — no hand-rolled DDL.
        let migration_dir = tempfile::tempdir().expect("migration dir");
        let migration_dir_path = migration_dir.path().to_path_buf();
        std::mem::forget(migration_dir);
        umbral::migrate::make_in(&migration_dir_path)
            .await
            .expect("make migrations");
        umbral::migrate::run_in(&migration_dir_path)
            .await
            .expect("run migrations");
        umbral_permissions::seed_standard_permissions_for_tests()
            .await
            .expect("seed permissions");

        let pool = umbral::db::pool();

        // Seed the custom codename so it can be granted to the privileged user.
        // `permissions_userpermission.permission_id` FK resolves against this
        // row (codename IS the FK value post-gap-#60). Mirrors the pattern in
        // sidebar_perm_gate.rs for non-standard permissions.
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_contenttype (app_label, model) \
             VALUES ('reports', 'secret')",
        )
        .execute(&pool)
        .await
        .expect("insert ct reports/secret");
        let ct_id: i64 = sqlx::query_scalar(
            "SELECT id FROM permissions_contenttype WHERE app_label = 'reports' AND model = 'secret'",
        )
        .fetch_one(&pool)
        .await
        .expect("fetch ct_id");
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_permission (codename, content_type_id, name) \
             VALUES (?, ?, 'Can view secret report')",
        )
        .bind(SECRET_CODENAME)
        .bind(ct_id)
        .execute(&pool)
        .await
        .expect("insert perm reports.view_secret");

        // `sidebar_staff` — plain staff, NOT superuser, holds NO custom codename.
        let denied = create_user_with_flags(
            "sidebar_staff",
            "sidebar@example.com",
            "pass123",
            true,
            false,
        )
        .await
        .expect("create sidebar_staff");
        let _ = denied;

        // `sidebar_priv` — staff, NOT superuser, granted SECRET_CODENAME.
        let privileged = create_user_with_flags(
            "sidebar_priv",
            "sidebar_priv@example.com",
            "pass123",
            true,
            false,
        )
        .await
        .expect("create sidebar_priv");
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) \
             VALUES (?, ?)",
        )
        .bind(privileged.id.to_string())
        .bind(SECRET_CODENAME)
        .execute(&pool)
        .await
        .expect("grant reports.view_secret");

        app.into_router()
    })
    .await
}

/// Resolve a username already created in `boot()` to a session cookie.
async fn cookie_for(username: &str) -> String {
    let pool = umbral::db::pool();
    let user = sqlx::query_as::<_, umbral_auth::AuthUser>(
        "SELECT id, username, email, password_hash, is_active, is_staff, is_superuser, \
         date_joined, last_login, email_verified_at \
         FROM auth_user WHERE username = ?",
    )
    .bind(username)
    .fetch_one(&pool)
    .await
    .unwrap_or_else(|_| panic!("lookup {username}"));
    let tok = umbral_sessions::create_session(Some(user.id.to_string()), None)
        .await
        .expect("session");
    format!("umbral_session={tok}")
}

async fn dashboard_html(router: &axum::Router, cookie: String) -> String {
    let req = Request::builder()
        .uri("/admin/?dashboard=1")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&body).into_owned()
}

// =========================================================================
// Tests
// =========================================================================

/// A non-hidden registered view appears as a sidebar link on the dashboard.
#[tokio::test]
async fn visible_view_appears_in_sidebar() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let html = dashboard_html(router, cookie_for("sidebar_staff").await).await;

    assert!(
        html.contains("href=\"/admin/custom-views/reports/sales/\""),
        "sidebar must contain href for visible view"
    );
    assert!(
        html.contains("data-lucide=\"bar-chart\""),
        "sidebar must render the view's icon"
    );
    assert!(
        html.contains("Sales report"),
        "sidebar must show the view title"
    );
}

/// A `.hide()`-d view must NOT appear in the sidebar.
#[tokio::test]
async fn hidden_view_absent_from_sidebar() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let html = dashboard_html(router, cookie_for("sidebar_staff").await).await;

    assert!(
        !html.contains("href=\"/admin/custom-views/internal/debug/\""),
        "hidden view must not appear in the sidebar"
    );
}

/// Security path: a `.with_permission(codename)` view is filtered out of the
/// sidebar for a staff user who lacks that codename — while a non-gated view
/// stays visible in the SAME response (the filter is selective, not blanket).
///
/// Regression guard: inverting the `!` in `if !has_codename(...) { continue }`
/// would make the gated view appear here, failing this test. The two existing
/// hidden/visible tests do not exercise the permission branch at all.
#[tokio::test]
async fn gated_view_hidden_for_user_without_codename() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let html = dashboard_html(router, cookie_for("sidebar_staff").await).await;

    assert!(
        !html.contains("href=\"/admin/custom-views/reports/secret/\""),
        "permission-gated view must be filtered out for a staff user without the codename"
    );
    // Selective, not blanket: the non-gated view is still present.
    assert!(
        html.contains("href=\"/admin/custom-views/reports/sales/\""),
        "non-gated view must remain visible in the same response (filter is selective)"
    );
}

/// Counterpart to the negative path: a staff user who HOLDS the codename DOES
/// see the gated view. Proves the filter keys on the codename (not a constant
/// false from an errored permission lookup) and catches the inverse `!`
/// regression from the other side.
#[tokio::test]
async fn gated_view_shown_for_user_with_codename() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let html = dashboard_html(router, cookie_for("sidebar_priv").await).await;

    assert!(
        html.contains("href=\"/admin/custom-views/reports/secret/\""),
        "staff user holding the codename must see the gated view"
    );
}
