//! Phase 4 dashboard widget system tests.
//!
//! 1. GET /admin/api/dashboard/catalog lists registered widgets (built-ins + custom).
//! 2. GET /admin/api/dashboard/widgets/{key}/data returns typed JSON payload.
//! 3. Unknown widget key returns 404.
//! 4. GET /admin/ renders with widget placeholder divs.
//! 5. PUT + GET /admin/api/dashboard/layout round-trips.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbra_admin::{AdminPlugin, KpiPayload, Span, Widget, WidgetDataFn, WidgetKind, WidgetPayload};
use umbra_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbra_sessions::SessionsPlugin;

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase4_dashboard.sqlite");
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

        let custom_widget = Widget {
            key: "test_kpi",
            title: "Test KPI".to_string(),
            kind: WidgetKind::Kpi,
            default_span: Span { cols: 3, rows: 1 },
            permission: None,
            data: WidgetDataFn::new(|_user| async move {
                WidgetPayload::Kpi(KpiPayload {
                    value: "99".to_string(),
                    unit: Some("items".to_string()),
                    delta: Some(5.2),
                    sparkline: None,
                })
            }),
        };

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool_obj)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register_widget(custom_widget))
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

        umbra_admin::models::ensure_tables_for_tests(&pool)
            .await
            .expect("ensure_tables");

        app.into_router()
    })
    .await
}

async fn staff_cookie() -> String {
    let user = match create_user_with_flags("dash_user", "dash@example.com", "pass123", true, false)
        .await
    {
        Ok(u) => u,
        Err(_) => {
            let pool = umbra::db::pool();
            sqlx::query_as::<_, umbra_auth::AuthUser>(
                "SELECT id, username, email, password_hash, is_active, is_staff, is_superuser, date_joined, last_login \
                 FROM auth_user WHERE username = 'dash_user'",
            )
            .fetch_one(&pool)
            .await
            .expect("lookup dash_user")
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
async fn catalog_lists_registered_widgets() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/api/dashboard/catalog")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let arr = json.as_array().unwrap();

    // Built-ins (2) + custom = at least 3
    assert!(arr.len() >= 3, "expected ≥3 widgets, got {}", arr.len());

    let keys: Vec<&str> = arr.iter().filter_map(|v| v["key"].as_str()).collect();
    assert!(
        keys.contains(&"umbra_total_models"),
        "missing umbra_total_models"
    );
    assert!(
        keys.contains(&"umbra_recent_users"),
        "missing umbra_recent_users"
    );
    assert!(keys.contains(&"test_kpi"), "missing test_kpi");
}

#[tokio::test]
async fn widget_data_returns_typed_payload() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/api/dashboard/widgets/test_kpi/data")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["key"].as_str().unwrap_or(""), "test_kpi");
    assert_eq!(json["kind"].as_str().unwrap_or(""), "kpi");
    assert_eq!(json["payload"]["value"].as_str().unwrap_or(""), "99");
    assert_eq!(json["payload"]["unit"].as_str().unwrap_or(""), "items");
}

#[tokio::test]
async fn unknown_widget_key_returns_404() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/api/dashboard/widgets/no_such_widget/data")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dashboard_page_renders_widget_placeholders() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("hx-get=\"/admin/api/dashboard/widgets/"),
        "expected HTMX widget placeholders in dashboard HTML"
    );
}

#[tokio::test]
async fn dashboard_layout_round_trips() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let layout = r#"[{"key":"test_kpi","span":{"cols":6,"rows":2}}]"#;

    let put_req = Request::builder()
        .method("PUT")
        .uri("/admin/api/dashboard/layout")
        .header(header::COOKIE, cookie.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(layout))
        .unwrap();
    let put_resp = router.clone().oneshot(put_req).await.unwrap();
    assert_eq!(put_resp.status(), StatusCode::OK);

    let get_req = Request::builder()
        .uri("/admin/api/dashboard/layout")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let get_resp = router.clone().oneshot(get_req).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);

    let body = get_resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body);
    assert!(body_str.contains("test_kpi"), "layout not saved/returned");
}
