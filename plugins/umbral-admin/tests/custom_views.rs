//! Behavioral integration tests for custom admin views — page-level coverage.
//!
//! Design spec: docs/superpowers/specs/2026-07-01-admin-custom-views-design.md
//!
//! Coverage in this file (page-level; sidebar-level is in custom_views_sidebar.rs):
//! 1. A registered view renders 200 inside the admin chrome with its title,
//!    widget cell (id="widget-{key}"), and the HTMX data-endpoint URL.
//! 2. A custom view's widget is reachable via the global data endpoint
//!    (`GET /admin/api/dashboard/widgets/{key}/data` → 200), proving the
//!    view's widgets are flattened into the global catalog on `App::build`.
//! 3. The view's page handler enforces the codename gate when
//!    `PermissionsPlugin` is installed: a staff user WITHOUT the codename
//!    gets 403; the same user WITH it gets 200.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{
    AdminPlugin, AdminView, KpiPayload, Span, Widget, WidgetDataFn, WidgetKind, WidgetPayload,
    WidgetSection,
};
use umbral_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbral_permissions::PermissionsPlugin;
use umbral_sessions::SessionsPlugin;

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

/// The codename the permission-gated view requires.
const SECRET_CODENAME: &str = "reports.view_secret";
/// Widget key inside the gated view — used by the widget-data gate tests.
const SECRET_WIDGET_KEY: &str = "rpt_secret";

// =========================================================================
// Tiny widget helper
// =========================================================================

fn tiny_kpi(key: &'static str) -> Widget {
    Widget {
        key,
        title: format!("KPI {key}"),
        kind: WidgetKind::Kpi,
        default_span: Span { cols: 3, rows: 1 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            WidgetPayload::Kpi(KpiPayload {
                value: "42".to_string(),
                unit: Some("units".to_string()),
                delta: None,
                sparkline: None,
            })
        }),
    }
}

// =========================================================================
// App boot
// =========================================================================

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("custom_views.sqlite");
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

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool_obj)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            // PermissionsPlugin installed so `has_codename` actually checks
            // the DB — the 403 gate test would be vacuous without it.
            .plugin(PermissionsPlugin)
            .plugin(
                AdminPlugin::default()
                    // Visible view with a "rpt_total" KPI widget — covers
                    // page render + widget-data reachability tests.
                    .view(
                        AdminView::new("reports/sales", "Sales report")
                            .with_icon("bar-chart")
                            .section(
                                WidgetSection::new("This month").widget(tiny_kpi("rpt_total")),
                            ),
                    )
                    // Permission-gated view — covers the 403/200 handler gate
                    // AND the widget-data endpoint gate (the security fix).
                    .view(
                        AdminView::new("reports/secret", "Secret report")
                            .with_permission(SECRET_CODENAME)
                            .section(
                                WidgetSection::new("Secret data")
                                    .widget(tiny_kpi(SECRET_WIDGET_KEY)),
                            ),
                    ),
            )
            .build()
            .expect("App::build");

        // Run real migrations for auth, sessions, permissions, admin.
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

        // Seed the custom codename row so it can be granted to a user.
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_contenttype \
             (app_label, model) VALUES ('reports', 'secret')",
        )
        .execute(&pool)
        .await
        .expect("insert ct reports/secret");
        let ct_id: i64 = sqlx::query_scalar(
            "SELECT id FROM permissions_contenttype \
             WHERE app_label = 'reports' AND model = 'secret'",
        )
        .fetch_one(&pool)
        .await
        .expect("fetch ct_id");
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_permission \
             (codename, content_type_id, name) VALUES (?, ?, 'Can view secret report')",
        )
        .bind(SECRET_CODENAME)
        .bind(ct_id)
        .execute(&pool)
        .await
        .expect("insert perm reports.view_secret");

        // `cv_staff` — plain staff, no custom codename.
        create_user_with_flags("cv_staff", "cv_staff@example.com", "pass123", true, false)
            .await
            .expect("create cv_staff");

        // `cv_priv` — staff, granted SECRET_CODENAME.
        let privileged =
            create_user_with_flags("cv_priv", "cv_priv@example.com", "pass123", true, false)
                .await
                .expect("create cv_priv");
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_userpermission \
             (user_id, permission_id) VALUES (?, ?)",
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

// =========================================================================
// Session helpers
// =========================================================================

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

// =========================================================================
// Tests
// =========================================================================

/// A registered view renders a 200 response that includes:
///  - the page title ("Sales report")
///  - a widget cell with `id="widget-rpt_total"` (the template emits this
///    via the shared `widget_grid` macro for HTMX self-load)
///  - the widget data URL the cell will load from
///    (`/api/dashboard/widgets/rpt_total/data`)
#[tokio::test]
async fn test_custom_view_page_renders() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = cookie_for("cv_staff").await;

    let req = Request::builder()
        .uri("/admin/reports/sales")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "custom view page must return 200"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);

    assert!(
        html.contains("Sales report"),
        "page must contain the view title 'Sales report'"
    );
    assert!(
        html.contains("id=\"widget-rpt_total\""),
        "page must contain a widget cell with id=\"widget-rpt_total\" (widget_grid macro)"
    );
    assert!(
        html.contains("/api/dashboard/widgets/rpt_total/data"),
        "page must embed the HTMX data URL for the rpt_total widget"
    );
}

/// A widget registered inside a custom view section is flattened into the
/// global widget catalog on `App::build`, so the per-key data endpoint
/// resolves it exactly as it would for a dashboard widget.
///
/// Regression guard: if the flatten loop is removed or the key is silently
/// deduplicated to the wrong entry, this endpoint returns 404 instead of 200.
#[tokio::test]
async fn test_custom_view_widget_data_served() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = cookie_for("cv_staff").await;

    let req = Request::builder()
        .uri("/admin/api/dashboard/widgets/rpt_total/data")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "widget registered in a custom view's section must be reachable via the global data endpoint"
    );
}

/// The page handler enforces `require_codename` when `PermissionsPlugin` is
/// installed: a staff user WITHOUT the codename gets 403, not 200.
///
/// Security path: inverting the `!` in `require_codename` would let the
/// handler proceed, changing the 403 to 200 and failing this test. The
/// sidebar-filter assertions in custom_views_sidebar.rs do NOT exercise the
/// page handler gate — that path returns early before reaching the handler.
#[tokio::test]
async fn test_custom_view_page_permission_gate_403() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    // cv_staff holds NO custom codename → must be denied with 403.
    let cookie = cookie_for("cv_staff").await;

    let req = Request::builder()
        .uri("/admin/reports/secret")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "staff user without the codename must get 403 on the gated view's page"
    );
}

/// Counterpart to the 403 test: a staff user WHO HOLDS the codename reaches
/// the page (200). Proves the gate passes valid holders, not a constant-deny.
#[tokio::test]
async fn test_custom_view_page_permission_gate_200_with_codename() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    // cv_priv was granted SECRET_CODENAME in boot() → must succeed.
    let cookie = cookie_for("cv_priv").await;

    let req = Request::builder()
        .uri("/admin/reports/secret")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "staff user holding the codename must reach the gated view (200)"
    );
}

/// Security gate on the widget-data API: a staff user WITHOUT the view's
/// permission codename must receive 403 when fetching the widget-data
/// endpoint for a widget that lives inside a `.with_permission()`-gated view.
///
/// `.with_permission(codename)` guards the page and the sidebar. This test
/// proves it ALSO guards `GET /admin/api/dashboard/widgets/{key}/data` for
/// every widget registered in that view — closing the leak where a user who
/// can't see the page could still scrape the data by hitting the API directly.
#[tokio::test]
async fn test_custom_view_widget_data_gated_403() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    // cv_staff holds NO custom codename → widget data must be denied.
    let cookie = cookie_for("cv_staff").await;

    let req = Request::builder()
        .uri(&format!(
            "/admin/api/dashboard/widgets/{SECRET_WIDGET_KEY}/data"
        ))
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "staff user without the view codename must get 403 fetching the gated view's widget data"
    );
}

/// Counterpart to the widget-data 403 test: a staff user WHO HOLDS the
/// view's codename may fetch the widget-data endpoint (200). Proves the
/// gate passes valid holders, not a constant-deny.
#[tokio::test]
async fn test_custom_view_widget_data_gated_200_with_codename() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    // cv_priv holds SECRET_CODENAME → widget data must be served.
    let cookie = cookie_for("cv_priv").await;

    let req = Request::builder()
        .uri(&format!(
            "/admin/api/dashboard/widgets/{SECRET_WIDGET_KEY}/data"
        ))
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "staff user holding the view codename must be served widget data (200)"
    );
}
