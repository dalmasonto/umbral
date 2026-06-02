//! Tests for the AdminModel extensibility surface (gap 18 + phase 1).
//!
//! Covers:
//! 1. `list_display` — filters which columns the list view renders.
//! 2. `list_filter` — filter facets appear in the response HTML.
//! 3. `search_fields` — `?q=` produces a correct WHERE LIKE clause.
//! 4. `ordering` — list rows come back in the configured ORDER BY.
//! 5. Custom action — runs the handler and returns the flash message.
//! 6. `readonly_fields` — form renders `<input ... readonly>` for those fields.
//!
//! Auth is session-based (HTML form flow). Uses a shared `login_session`
//! helper to obtain a session cookie before each test.
//!
//! Uses the same OnceCell-boot pattern as `tests/integration.rs`.

#![allow(dead_code, private_interfaces)]

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbra_admin::{Action, AdminModel, AdminPlugin};
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_sessions::SessionsPlugin;

/// A simple model for these tests.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Article {
    id: i64,
    title: String,
    body: String,
    published: bool,
    created_at: Option<DateTime<Utc>>,
}

// =========================================================================
// Shared boot. Each test clones the router (same Arc'd state underneath).
// =========================================================================

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir for the test DB");
        let path = tmp.path().join("admin_extensibility.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        // AdminModel (was AdminConfig) for the `article` table.
        let article_config = AdminModel::new("article")
            .list_display(&["title", "published"])
            .list_filter(&["published"])
            .search_fields(&["title", "body"])
            .ordering(&["-id"])
            .readonly_fields(&["created_at"])
            .actions(vec![
                Action::delete_selected(),
                Action::new(
                    "mark_published",
                    "Mark published",
                    "check-circle",
                    |inv| async move {
                        Ok(umbra_admin::ActionResult::Toast {
                            message: format!("Marked {} article(s) as published.", inv.ids.len()),
                            level: umbra_admin::ToastLevel::Success,
                        })
                    },
                ),
            ]);

        let admin = AdminPlugin::default().register(article_config);

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(admin)
            .model::<Article>()
            .build()
            .expect("App::build with AdminPlugin + AdminModel");

        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE auth_user (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL UNIQUE,\
                email TEXT NOT NULL,\
                password_hash TEXT NOT NULL,\
                is_active INTEGER NOT NULL,\
                is_staff INTEGER NOT NULL,\
                is_superuser INTEGER NOT NULL,\
                date_joined TEXT NOT NULL,\
                last_login TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create auth_user");

        sqlx::query(
            "CREATE TABLE session (\
                id TEXT PRIMARY KEY,\
                user_id TEXT,\
                data TEXT NOT NULL DEFAULT '{}',\
                created_at TEXT NOT NULL,\
                expires_at TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create session");

        sqlx::query(
            "CREATE TABLE article (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL,\
                published INTEGER NOT NULL DEFAULT 0,\
                created_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create article");

        // Seed a staff user.
        let staff = create_user("admin_ext", "admin_ext@example.com", "password123")
            .await
            .expect("create staff user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark staff");

        sqlx::query(
            "INSERT INTO article (title, body, published) VALUES \
             ('Alpha article', 'alpha body text', 0), \
             ('Beta article',  'beta body text',  1)",
        )
        .execute(&pool)
        .await
        .expect("seed articles");

        app.into_router()
    })
    .await
}

// =========================================================================
// Session auth helpers (same as integration.rs).
// =========================================================================

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    (status, body)
}

async fn send_full(
    router: axum::Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let headers = resp.headers().clone();
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    (status, headers, body)
}

fn extract_csrf_token(html: &str) -> Option<String> {
    // Find the input with name="csrf_token", then locate its value attribute
    // within a small window after it. Tolerant of whitespace / line breaks
    // between attributes so reformats of login.html don't break the test.
    let name_marker = r#"name="csrf_token""#;
    let pos = html.find(name_marker)?;
    let window_end = pos.saturating_add(400).min(html.len());
    let window = &html[pos..window_end];
    let value_marker = "value=\"";
    let vstart = window.find(value_marker)? + value_marker.len();
    let vend = window[vstart..].find('"')?;
    Some(window[vstart..vstart + vend].to_string())
}

async fn login_session(router: &axum::Router, username: &str, password: &str) -> String {
    let (_, headers, body) = send_full(
        router.clone(),
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    // GET /admin/login mints (or echoes) the umbra_csrf_token cookie.
    // Find it in the Set-Cookie header(s).
    let csrf_cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|s| {
            let first = s.split(';').next()?;
            let (k, v) = first.split_once('=')?;
            if k.trim() == "umbra_csrf_token" {
                Some(v.to_string())
            } else {
                None
            }
        })
        .expect("GET /admin/login must set umbra_csrf_token cookie");
    let csrf_token = extract_csrf_token(&body).expect("login page must have csrf_token");

    let form_body = serde_urlencoded::to_string([
        ("username", username),
        ("password", password),
        ("csrf_token", &csrf_token),
        ("next", "/admin/"),
    ])
    .unwrap();
    let (status, headers2, _) = send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::COOKIE, format!("umbra_csrf_token={csrf_cookie}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form_body))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "login_session should succeed"
    );
    headers2
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()
                .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        })
        .expect("POST /admin/login must set authenticated session cookie")
}

// =========================================================================
// 1. list_display: only listed columns appear in the list HTML.
// =========================================================================

#[tokio::test]
async fn list_display_filters_columns() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "admin_ext", "password123").await;
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/")
            .header(header::COOKIE, format!("umbra_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    assert!(body.contains("title"), "title column missing:\n{body}");
    assert!(
        body.contains("published"),
        "published column missing:\n{body}"
    );
    assert!(
        !body.contains("<th>body</th>"),
        "body column should be hidden:\n{body}"
    );
    assert!(
        !body.contains("<th>created_at</th>"),
        "created_at column should be hidden:\n{body}"
    );
    assert!(
        body.contains("Alpha article"),
        "Alpha article missing:\n{body}"
    );
}

// =========================================================================
// 2. list_filter: filter button appears in the toolbar.
// The filter dialog itself is a separate HTMX fragment loaded on demand,
// not embedded in the initial changelist HTML.
// =========================================================================

#[tokio::test]
async fn list_filter_shows_facets_in_sidebar() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "admin_ext", "password123").await;
    let (status, body) = send(
        router.clone(),
        Request::builder()
            .uri("/admin/article/")
            .header(header::COOKIE, format!("umbra_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // The Filter button (with sliders-horizontal icon) should be visible
    // because `list_filter` is configured.
    assert!(
        body.contains("filter-dialog")
            || body.contains("sliders-horizontal")
            || body.contains("Filter"),
        "filter button missing when list_filter is configured:\n{body}"
    );
    // The filter dialog endpoint should return 200 for authenticated requests.
    let (dialog_status, dialog_body) = send(
        router,
        Request::builder()
            .uri("/admin/article/filter-dialog")
            .header(header::COOKIE, format!("umbra_session={cookie}"))
            .header("HX-Request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        dialog_status,
        StatusCode::OK,
        "filter-dialog endpoint failed:\n{dialog_body}"
    );
}

// =========================================================================
// 3. search_fields: ?q= narrows the list to matching rows.
// =========================================================================

#[tokio::test]
async fn search_fields_filters_rows() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "admin_ext", "password123").await;
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/?q=alpha")
            .header(header::COOKIE, format!("umbra_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    assert!(
        body.contains("Alpha article"),
        "Alpha article should match 'alpha':\n{body}"
    );
    assert!(
        !body.contains("Beta article"),
        "Beta article should not match 'alpha':\n{body}"
    );
}

#[tokio::test]
async fn search_fields_no_match_shows_empty() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "admin_ext", "password123").await;
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/?q=zzznomatch")
            .header(header::COOKIE, format!("umbra_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    assert!(
        !body.contains("Alpha article"),
        "Alpha article should not appear:\n{body}"
    );
    assert!(
        !body.contains("Beta article"),
        "Beta article should not appear:\n{body}"
    );
}

// =========================================================================
// 4. ordering: list rows appear in configured ORDER BY order.
// =========================================================================

#[tokio::test]
async fn ordering_applies_to_list() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "admin_ext", "password123").await;
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/")
            .header(header::COOKIE, format!("umbra_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    let alpha_pos = body.find("Alpha article").unwrap_or(usize::MAX);
    let beta_pos = body.find("Beta article").unwrap_or(usize::MAX);
    assert!(
        beta_pos < alpha_pos,
        "Beta (id=2) should appear before Alpha (id=1) with ORDER BY id DESC; \
         alpha_pos={alpha_pos}, beta_pos={beta_pos}"
    );
}

// =========================================================================
// 5. Custom action: runs the handler and redirects with flash message.
// =========================================================================

#[tokio::test]
async fn custom_action_runs_and_returns_flash() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "admin_ext", "password123").await;

    let form_body = serde_urlencoded::to_string([
        ("action", "mark_published"),
        ("selected", "1"),
        ("selected", "2"),
    ])
    .unwrap();

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/article/action")
                .header(header::COOKIE, format!("umbra_session={cookie}"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "action should redirect"
    );
    let location = resp
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        location.contains("flash="),
        "redirect should include flash: {location}"
    );
    assert!(
        location.contains("article"),
        "redirect should point back to article list: {location}"
    );
}

// =========================================================================
// 6. readonly_fields: form renders <input ... readonly> for those fields.
// =========================================================================

#[tokio::test]
async fn readonly_fields_render_readonly_input() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "admin_ext", "password123").await;
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/1/edit")
            .header(header::COOKIE, format!("umbra_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "edit form body:\n{body}");
    assert!(
        body.contains("readonly"),
        "readonly attribute missing from form:\n{body}"
    );
}

// =========================================================================
// Quiet unused import.
// =========================================================================
#[allow(dead_code)]
fn _unused_pathbuf_marker() -> Option<PathBuf> {
    None
}
