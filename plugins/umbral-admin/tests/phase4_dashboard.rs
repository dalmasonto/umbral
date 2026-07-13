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

use umbral_admin::{
    AdminPlugin, BarPayload, ChartPoint, HeatmapPayload, KpiPayload, ProgressPayload,
    RadialPayload, Series, Span, Widget, WidgetDataFn, WidgetFilter, WidgetKind, WidgetPayload,
};
use umbral_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbral_sessions::SessionsPlugin;

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase4_dashboard.sqlite");
        std::mem::forget(tmp);
        let pool_obj = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
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
            default_period: None,
            filters: Vec::new(),
            data: WidgetDataFn::new(|_user| async move {
                WidgetPayload::Kpi(KpiPayload {
                    value: "99".to_string(),
                    unit: Some("items".to_string()),
                    delta: Some(5.2),
                    sparkline: None,
                })
            }),
        };

        // The newer widget kinds — each must render its HTML fragment
        // through the macro registered in engine.rs (regression guard
        // for "macro template never registered" -> import error ->
        // blank widget cell).
        let radial_widget = Widget {
            key: "test_radial",
            title: "Test Radial".to_string(),
            kind: WidgetKind::Radial,
            default_span: Span { cols: 3, rows: 2 },
            permission: None,
            default_period: None,
            filters: Vec::new(),
            data: WidgetDataFn::new(|_user| async move {
                WidgetPayload::Radial(RadialPayload::single("Done", 73.0))
            }),
        };
        let heatmap_widget = Widget {
            key: "test_heatmap",
            title: "Test Heatmap".to_string(),
            kind: WidgetKind::Heatmap,
            default_span: Span { cols: 6, rows: 3 },
            permission: None,
            default_period: None,
            filters: Vec::new(),
            data: WidgetDataFn::new(|_user| async move {
                WidgetPayload::Heatmap(HeatmapPayload::from_grid(
                    ["R1"],
                    ["a", "b"],
                    vec![vec![1.0, 2.0]],
                ))
            }),
        };
        // A widget carrying declarative filters. The data closure echoes the
        // resolved filter values straight back into the payload, so a test can
        // assert what the CLOSURE actually received rather than merely that some
        // HTML contains a `<select>` — a control that renders but never reaches
        // the query is the exact bug worth catching.
        let filtered_widget = Widget::new(
            "test_filtered",
            "Filtered",
            WidgetKind::Bar,
            WidgetDataFn::with_params(|_user, params| async move {
                let status = params.choice("status").unwrap_or("none").to_string();
                let period = params.period.clone().unwrap_or_else(|| "none".into());
                let range = params
                    .date_range()
                    .map(|(s, e)| format!("{s}..{e}"))
                    .unwrap_or_else(|| "none".into());
                WidgetPayload::Bar(BarPayload {
                    series: vec![Series {
                        name: format!("status={status};period={period};range={range}"),
                        points: vec![ChartPoint {
                            x: "x".to_string(),
                            y: 1.0,
                        }],
                    }],
                    x_type: "t".to_string(),
                })
            }),
        )
        .filter(WidgetFilter::period_default())
        .filter(WidgetFilter::date_range())
        .filter(WidgetFilter::choice(
            "status",
            "Status",
            [("open", "Open"), ("paid", "Paid")],
        ));

        let progress_widget = Widget {
            key: "test_progress",
            title: "Test Progress".to_string(),
            kind: WidgetKind::Progress,
            default_span: Span { cols: 3, rows: 3 },
            permission: None,
            default_period: None,
            filters: Vec::new(),
            data: WidgetDataFn::new(|_user| async move {
                WidgetPayload::Progress(ProgressPayload::from_pairs([("A", 10.0), ("B", 5.0)]))
            }),
        };

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool_obj)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            // Builtins are now opt-in (used to auto-prepend). The
            // test exercises the catalog endpoint with all three
            // shapes registered: both builtins + a custom widget.
            .plugin(
                AdminPlugin::default()
                    .register_widget(umbral_admin::builtin_total_models_widget())
                    .register_widget(umbral_admin::builtin_recent_users_widget())
                    .register_widget(custom_widget)
                    .register_widget(radial_widget)
                    .register_widget(heatmap_widget)
                    .register_widget(progress_widget)
                    .register_widget(filtered_widget),
            )
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

        umbral_admin::models::ensure_tables_for_tests(&pool)
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
            let pool = umbral::db::pool();
            sqlx::query_as::<_, umbral_auth::AuthUser>(
                "SELECT id, username, email, password_hash, is_active, is_staff, is_superuser, date_joined, last_login, email_verified_at \
                 FROM auth_user WHERE username = 'dash_user'",
            )
            .fetch_one(&pool)
            .await
            .expect("lookup dash_user")
        }
    };
    let tok = umbral_sessions::create_session(Some(user.id.to_string()), None)
        .await
        .expect("session");
    format!("umbral_session={tok}")
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
        keys.contains(&"umbral_total_models"),
        "missing umbral_total_models"
    );
    assert!(
        keys.contains(&"umbral_recent_users"),
        "missing umbral_recent_users"
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

/// Every widget kind must render its HTML fragment through the macro
/// registered in `engine.rs`. Regression guard: the radial/heatmap/
/// progress macro templates were initially added to `widget_data.html`
/// but NOT registered with the minijinja env, so the `{% from ... %}`
/// import failed at render time and the widget cell stayed blank.
#[tokio::test]
async fn new_widget_kinds_render_html_fragments() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    for (key, marker) in [
        ("test_radial", "data-umbral-chart=\"radial\""),
        ("test_heatmap", "data-umbral-chart=\"heatmap\""),
        ("test_progress", "progress-widget"),
    ] {
        let req = Request::builder()
            .uri(format!("/admin/api/dashboard/widgets/{key}/data"))
            .header(header::COOKIE, cookie.clone())
            // HTML path (the dashboard cell's hx-get), not the JSON API.
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{key} should render 200");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8_lossy(&body);
        assert!(
            html.contains(marker),
            "{key} fragment must contain `{marker}` (macro not registered?); got: {html}"
        );
    }
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

/// gaps2 #4 — the admin runtime JS is served as a single external
/// asset (now on the unified `/static/admin/admin.js` URL) rather than
/// ~1080 lines of inline `<script>` blocks in wrapper.html. Four pins:
///
/// 1. The unified `/static/admin/admin.js` endpoint serves a valid
///    `application/javascript` response whose body is the EMBEDDED bytes
///    (proves the re-point onto `/static/admin/…` resolves through the
///    real Phase-5.4 specific route, beating the pipeline fallback —
///    zero-config single-binary serving preserved on the new URL).
/// 2. wrapper.html references the external file (via `static()`) + sets
///    the `umbralAdminBase` bootstrap.
/// 3. The pre-fix inline IIFE marker (`// Sheet stack state machine.`,
///    the comment at the top of old Block 5) no longer appears in
///    the served wrapper HTML — would catch a revert that re-inlines
///    the JS without updating the gap status.
#[tokio::test]
async fn admin_js_served_as_external_asset_not_inline() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    // 1. Unified static-pipeline asset endpoint. The specific Phase-5.4
    //    route serves the embedded bytes, winning over the nested pipeline
    //    fallback at `static_url`.
    let asset_req = Request::builder()
        .uri("/static/admin/admin.js")
        .body(Body::empty())
        .unwrap();
    let asset_resp = router.clone().oneshot(asset_req).await.unwrap();
    assert_eq!(asset_resp.status(), StatusCode::OK);
    let ct = asset_resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("application/javascript"),
        "admin.js Content-Type should be application/javascript, got `{ct}`"
    );
    let body = asset_resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        body.len() > 1000,
        "admin.js body should be non-trivial, got {} bytes",
        body.len()
    );
    // The served bytes ARE the embedded include_bytes! content — proves the
    // re-point serves the in-binary asset end to end, not a disk file.
    assert_eq!(
        body.as_ref(),
        include_bytes!("../src/assets/admin.js").as_slice(),
        "/static/admin/admin.js should serve the embedded admin.js bytes"
    );

    // 2. wrapper.html references the external file
    let page_req = Request::builder()
        .uri("/admin/?dashboard=1")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let page_resp = router.clone().oneshot(page_req).await.unwrap();
    let html = String::from_utf8_lossy(&page_resp.into_body().collect().await.unwrap().to_bytes())
        .into_owned();
    assert!(
        html.contains("var umbralAdminBase = '/admin'"),
        "umbralAdminBase bootstrap should be inline (read by admin.js)"
    );
    assert!(
        html.contains("/static/admin/admin.js"),
        "wrapper.html should reference the external admin.js on the unified static URL"
    );

    // 3. The old inline IIFE comments must be gone — if they reappear
    //    in wrapper.html, the gap got reverted without updating tests.
    assert!(
        !html.contains("// Sheet stack state machine."),
        "old inline block-5 IIFE comment should be gone from wrapper.html"
    );
    assert!(
        !html.contains("// Extend the early-declared window.umbral stub"),
        "old inline block-3 IIFE comment should be gone from wrapper.html"
    );
}

/// gaps2 #3 — change-password dialog lives as an HTML `<template>`
/// in wrapper.html rather than JS-string concatenation. The opener
/// (`umbral._openChangePasswordDialog`) clones the template and
/// patches `hx-post` to the target URL.
///
/// Regression pin: ensure both the template element AND the form
/// selector hook (`data-change-pw-form`) are present, and that the
/// old JS-built shape is fully gone.
#[tokio::test]
async fn change_password_dialog_uses_html_template_not_js_concat() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/?dashboard=1")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);

    assert!(
        html.contains("<template id=\"umbral-change-password-dialog-template\">"),
        "expected change-password <template> block in wrapper.html"
    );
    assert!(
        html.contains("data-change-pw-form"),
        "expected `data-change-pw-form` selector hook on the form"
    );
    // Negative pin: the old JS-string-concat shape's literal
    // `'/' + id + '/change-password"' + ...` should be gone. If
    // someone reverts to the pre-fix builder, this test catches it.
    assert!(
        !html.contains("change-password\"' +"),
        "old JS-string-concat change-password builder should be removed"
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

/// docs/decisions/2026-06-10-automatic-csrf.md: every htmx request the
/// admin makes must carry the ambient CSRF token. `hx-headers` on
/// `<body>` is inherited by all descendant hx-* requests, so one
/// attribute covers sheet create/edit, inline edit, delete, and actions.
#[test]
fn wrapper_body_carries_csrf_hx_headers() {
    let wrapper = include_str!("../templates/wrapper.html");
    let body_line = wrapper
        .lines()
        .find(|l| l.trim_start().starts_with("<body"))
        .expect("wrapper.html must have a <body> tag");
    assert!(
        body_line.contains("hx-headers"),
        "missing hx-headers: {body_line}"
    );
    assert!(
        body_line.contains("X-CSRF-Token"),
        "missing X-CSRF-Token: {body_line}"
    );
    assert!(
        body_line.contains("{{ csrf_token }}"),
        "must use the ambient token: {body_line}"
    );
}

/// Raw fetch() writes in admin.js (the PUT /api/prefs persistence calls)
/// bypass htmx's hx-headers inheritance, so each one must spread the
/// csrfHeaders() helper that reads the (deliberately non-HttpOnly) cookie.
#[test]
fn admin_js_fetches_send_csrf_header() {
    let js = include_str!("../src/assets/admin.js");
    assert!(
        js.contains("function csrfHeaders()"),
        "admin.js needs the csrfHeaders helper"
    );
    let writes = js.matches("method: 'PUT'").count() + js.matches("method: 'POST'").count();
    let wired = js.matches("csrfHeaders()").count();
    assert!(
        writes > 0 && wired >= writes,
        "every write fetch must spread csrfHeaders(): {wired} uses for {writes} writes"
    );
}

/// features.md #4: the admin lazy-mounts a markdown editor (EasyMDE)
/// and an RTE (Quill) onto the `data-widget` textareas the field
/// editor renders, on every form-render path (page load, htmx swap,
/// sheet open).
#[test]
fn admin_js_mounts_widget_editors() {
    let js = include_str!("../src/assets/admin.js");
    assert!(js.contains("initWidgetEditors"), "no widget-editor init");
    assert!(
        js.contains("new EasyMDE"),
        "markdown editor (EasyMDE) not mounted"
    );
    assert!(js.contains("new Quill"), "rte editor (Quill) not mounted");
    assert!(
        js.contains("CodeMirror.fromTextArea"),
        "code editor (CodeMirror) not mounted"
    );
    // Claims the textareas the field editor emits for each widget
    // (the selector is built dynamically from these names).
    assert!(
        js.contains("claim(root, 'markdown')"),
        "markdown widget not claimed"
    );
    assert!(js.contains("claim(root, 'rte')"), "rte widget not claimed");
    assert!(
        js.contains("claim(root, 'code')"),
        "code widget not claimed"
    );
    // Previews are sandboxed through DOMPurify (EasyMDE preview render +
    // Quill initial load), never rendering authored HTML raw.
    assert!(
        js.contains("DOMPurify"),
        "previews not sandboxed via DOMPurify"
    );
    assert!(
        js.contains("sanitizerFunction"),
        "EasyMDE preview not routed through the sanitizer"
    );
    assert!(
        js.contains(r#"data-widget="' + selector + '"#),
        "dynamic widget selector missing"
    );
    // Lazy-loaded, not eagerly bundled, and idempotent across re-scans.
    assert!(
        js.contains("loadScript"),
        "editors should be lazy-loaded from CDN"
    );
    assert!(js.contains("data-widget-mounted"), "no idempotency marker");
    // Mounted on the sheet's innerHTML path too (not just htmx swaps).
    assert!(
        js.matches("umbral.initWidgetEditors").count() >= 3,
        "must mount on DOMContentLoaded, htmx:afterSwap, AND the sheet path"
    );
}

/// Task 3 (visual refresh): every card surface on the dashboard uses the
/// shared card recipe (bg-surface + hairline border + rounded-xl + shadow-card).
/// This pins the elevation token so the dashboard reads as one card system.
#[tokio::test]
async fn test_dashboard_cards_use_shared_card_recipe() {
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
        html.contains("shadow-card"),
        "dashboard cards must use the shared shadow-card recipe"
    );
}

/// The editor libraries ship light themes; the wrapper re-skins them
/// with the admin design tokens so they track the dark/light toggle.
#[test]
fn wrapper_themes_the_editors() {
    let wrapper = include_str!("../templates/wrapper.html");
    assert!(
        wrapper.contains("umbral-editor-theme"),
        "no editor theme block"
    );
    assert!(wrapper.contains(".EasyMDEContainer"), "EasyMDE not themed");
    assert!(wrapper.contains(".umbral-rte .ql-"), "Quill not themed");
    assert!(
        wrapper.contains("var(--surface-container-low)"),
        "editor theme must use admin design tokens, not hardcoded colors"
    );
}

/// Task 3 (widget-grid macro): the dashboard widget grid is now rendered
/// via the shared `_macros/widget_grid.html` macro. Regression guard:
/// the dashboard still renders widget cells with HTMX self-load after
/// the inline block was moved into the macro.
#[tokio::test]
async fn test_dashboard_widget_grid_renders_via_macro() {
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
    // A widget cell still self-loads from the data endpoint after the macro extraction.
    assert!(
        html.contains("/api/dashboard/widgets/") && html.contains("hx-trigger=\"load\""),
        "dashboard still renders widget cells via the shared grid macro"
    );
}

// =========================================================================
// Declarative widget filters
// =========================================================================

/// Fetch the filtered widget's data endpoint and return the rendered HTML.
async fn filtered_html(router: &axum::Router, cookie: &str, query: &str) -> String {
    let uri = if query.is_empty() {
        "/admin/api/dashboard/widgets/test_filtered/data".to_string()
    } else {
        format!("/admin/api/dashboard/widgets/test_filtered/data?{query}")
    };
    let req = Request::builder()
        .uri(uri)
        .header(header::COOKIE, cookie)
        .header("HX-Request", "true")
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&body).to_string()
}

/// A filter is only real if its value reaches the data closure. The closure
/// echoes what it received into the series name, so this asserts the plumbing
/// end to end rather than just that a `<select>` appears somewhere.
#[tokio::test]
async fn declared_filters_reach_the_data_closure() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let html = filtered_html(router, &cookie, "status=paid&period=7d").await;
    assert!(
        html.contains("status=paid"),
        "the choice filter's value must reach the closure; got: {html}"
    );
    assert!(
        html.contains("period=7d"),
        "the period must reach the closure; got: {html}"
    );
}

/// Every widget kind gets controls now — this one is a Bar, which before this
/// change could not be filtered at all (the chip strip was hardcoded inside
/// line.html).
#[tokio::test]
async fn a_non_line_widget_renders_its_declared_controls() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let html = filtered_html(router, &cookie, "").await;
    assert!(
        html.contains("<select") && html.contains("value=\"paid\""),
        "the choice filter renders a select with its options on a BAR widget"
    );
    assert!(
        html.contains("type=\"date\""),
        "the date-range filter renders two date inputs"
    );
    assert!(
        html.contains("aria-pressed"),
        "the period filter renders its chip strip"
    );
}

/// Picking a filter sticks: a later request with a bare URL still sees it.
/// Without this the dashboard resets every reload and the controls are a toy.
#[tokio::test]
async fn a_chosen_filter_is_sticky_across_requests() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    // Pick it once...
    let _ = filtered_html(router, &cookie, "status=paid").await;
    // ...then ask with NO query at all.
    let html = filtered_html(router, &cookie, "").await;

    assert!(
        html.contains("status=paid"),
        "the previously chosen status must survive a bare request; got: {html}"
    );
    assert!(
        html.contains(r#"<option value="paid" selected"#),
        "and the select must render it as the selected option"
    );
}

/// A control must carry its neighbours' values, or clicking one silently
/// resets the others — the strip would then lie about what you are looking at.
#[tokio::test]
async fn a_control_carries_the_other_filters_values() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let html = filtered_html(router, &cookie, "status=open&period=90d").await;
    // The period chips must preserve `status=open` in their own hx-get URLs.
    assert!(
        html.contains("period=7d&status=open") || html.contains("period=7d&amp;status=open"),
        "a period chip must carry the active status along; got: {html}"
    );
}

// =========================================================================
// CSV export
// =========================================================================

/// The export must be computed from the filters currently in force — a CSV that
/// silently ignored them would disagree with the chart it came from.
#[tokio::test]
async fn export_honours_the_active_filters() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/api/dashboard/widgets/test_filtered/export.csv?status=paid&period=7d")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let ctype = resp
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let disp = resp
        .headers()
        .get("Content-Disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(ctype.starts_with("text/csv"), "served as CSV, got {ctype}");
    assert!(
        disp.contains("attachment") && disp.contains("test_filtered.csv"),
        "downloads as a named file, got {disp}"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let csv = String::from_utf8_lossy(&body);
    assert!(csv.starts_with("series,x,y\n"), "has a header row: {csv}");
    assert!(
        csv.contains("status=paid") && csv.contains("period=7d"),
        "the export ran the closure with the ACTIVE filters: {csv}"
    );
}

/// A KPI is a single number, not a table. Exporting it would hand back a
/// one-cell file, so the endpoint says so instead of pretending.
#[tokio::test]
async fn export_refuses_a_shape_with_no_rows() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .uri("/admin/api/dashboard/widgets/test_kpi/export.csv")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Anonymous callers cannot export. The export shares `gate_widget` with the
/// data endpoint precisely so it can never become the softer way in.
#[tokio::test]
async fn export_requires_staff() {
    let _guard = LOCK.lock().await;
    let router = boot().await;

    let req = Request::builder()
        .uri("/admin/api/dashboard/widgets/test_filtered/export.csv")
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "an anonymous export must not succeed"
    );
}

// =========================================================================
// Per-user widget reordering (features.md #8)
// =========================================================================

/// The layout endpoint has stored a layout for a long time; nothing READ it, so
/// dragging widgets appeared to work and silently reset on the next load. The
/// dashboard must now render in the saved order.
#[tokio::test]
async fn a_saved_layout_reorders_the_dashboard() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    // Put a widget that registers LAST at the front, with a custom span.
    let layout = r#"[
        {"key":"test_progress","span":{"cols":6,"rows":2}},
        {"key":"test_kpi","span":{"cols":3,"rows":1}}
    ]"#;
    let req = Request::builder()
        .method("PUT")
        .uri("/admin/api/dashboard/layout")
        .header(header::COOKIE, cookie.clone())
        .header("Content-Type", "application/json")
        .body(Body::from(layout))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let req = Request::builder()
        .uri("/admin/")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);

    let progress_at = html
        .find("id=\"widget-test_progress\"")
        .expect("progress widget renders");
    let kpi_at = html
        .find("id=\"widget-test_kpi\"")
        .expect("kpi widget renders");
    assert!(
        progress_at < kpi_at,
        "the saved layout must put test_progress before test_kpi; the dashboard \
         is still rendering in registration order"
    );

    // And the saved span must be applied, not the registration default (3x3).
    let cell = &html[progress_at.saturating_sub(400)..progress_at];
    assert!(
        cell.contains("span 6"),
        "the saved span must win over the registration default; got: {cell}"
    );
}

/// A layout is a preference, not a permission. An entry naming a widget that no
/// longer exists must be ignored rather than rendering a ghost cell.
#[tokio::test]
async fn a_layout_entry_for_an_unknown_widget_is_ignored() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let layout = r#"[{"key":"widget_that_does_not_exist","span":{"cols":12,"rows":4}}]"#;
    let req = Request::builder()
        .method("PUT")
        .uri("/admin/api/dashboard/layout")
        .header(header::COOKIE, cookie.clone())
        .header("Content-Type", "application/json")
        .body(Body::from(layout))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );

    let req = Request::builder()
        .uri("/admin/")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "the dashboard still renders");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(
        !html.contains("widget_that_does_not_exist"),
        "a stale layout entry must not render a ghost widget"
    );
    assert!(
        html.contains("id=\"widget-test_kpi\""),
        "and the real widgets still render"
    );
}
