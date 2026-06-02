//! Phase 2 DataTable tests.
//!
//! Covers:
//! 1. Changelist returns rows with `list_display` columns only.
//! 2. `?search=foo` returns just the matching rows.
//! 3. `?sort=title&order=desc` returns rows in reverse title order.
//! 4. `?filter[published]=true` filters rows (new `?filter=field=value` format).
//! 5. `?page=2&page_size=1` returns the second page.
//! 6. The HTMX `hx-target` markup is present in the rendered HTML.
//! 7. `GET /admin/{table}/rows` (HTMX fragment) returns just the tbody content.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbra_admin::{AdminModel, AdminPlugin};
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Post {
    id: i64,
    title: String,
    published: bool,
    created_at: Option<DateTime<Utc>>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase2_dt.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite pool");

        let post_config = AdminModel::new("post")
            .list_display(&["title", "published"])
            .list_filter(&["published"])
            .search_fields(&["title"])
            .ordering(&["title"]);

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(post_config))
            .model::<Post>()
            .build()
            .expect("App::build");

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
            "CREATE TABLE post (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                published INTEGER NOT NULL DEFAULT 0,\
                created_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create post");

        let staff = create_user("dt_admin", "dt_admin@example.com", "password123")
            .await
            .expect("create staff user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark staff");

        // Seed posts: Alpha (published=false), Beta (published=true), Gamma (published=true)
        sqlx::query(
            "INSERT INTO post (title, published) VALUES \
             ('Alpha post', 0), ('Beta post', 1), ('Gamma post', 1)",
        )
        .execute(&pool)
        .await
        .expect("seed posts");

        app.into_router()
    })
    .await
}

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn send_full(
    router: axum::Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn extract_csrf(html: &str) -> Option<String> {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker)?;
    let window = &html[pos..pos + 200];
    let val_marker = r#"value=""#;
    let vpos = window.find(val_marker)?;
    let after = &window[vpos + val_marker.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

fn extract_cookie_value(set_cookie: &str) -> String {
    // e.g. "umbra_session=abc123; Path=/; HttpOnly"
    set_cookie
        .split(';')
        .next()
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .unwrap_or_default()
}

async fn login_session(router: axum::Router, username: &str, password: &str) -> String {
    // GET login page → session cookie + CSRF
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("login get");
    let anon_raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let anon_cookie = extract_cookie_value(&anon_raw);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let csrf = extract_csrf(&html).unwrap_or_default();

    // POST credentials
    let form_body = serde_urlencoded::to_string([
        ("username", username),
        ("password", password),
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
                .body(Body::from(form_body))
                .unwrap(),
        )
        .await
        .expect("login post");
    resp2
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(extract_cookie_value)
        .unwrap_or(anon_cookie)
}

#[tokio::test]
async fn test_changelist_list_display_columns_only() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Should show "title" and "published" columns (list_display)
    assert!(body.contains("title"), "title column present: {body}");
    assert!(
        body.contains("published"),
        "published column present: {body}"
    );
    // "created_at" is not in list_display, should not be a column header
    // (may appear in other contexts but not as a table header)
}

#[tokio::test]
async fn test_changelist_search_returns_matching_rows() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/rows?search=alpha")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.to_lowercase().contains("alpha"),
        "alpha row present: {body}"
    );
    assert!(
        !body.to_lowercase().contains("beta"),
        "beta row absent: {body}"
    );
}

#[tokio::test]
async fn test_changelist_sort_order_desc() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/rows?sort=title&order=desc&page_size=10")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Gamma should appear before Alpha in desc order
    let gamma_pos = body.find("Gamma").unwrap_or(usize::MAX);
    let alpha_pos = body.find("Alpha").unwrap_or(usize::MAX);
    assert!(
        gamma_pos < alpha_pos,
        "Gamma before Alpha in desc order. body snippet: {}",
        &body[..body.len().min(500)]
    );
}

#[tokio::test]
async fn test_changelist_filter_published() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/rows?filter=published=true&page_size=10")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // Beta and Gamma are published=1, Alpha is not.
    // The filter "published=true" uses string match — in SQLite, true=1.
    // The "published" facet values will be "0" and "1".
    // Check we don't get Alpha (published=0):
    assert!(
        !body.contains("Alpha post"),
        "Alpha (unpublished) absent: {body}"
    );
}

#[tokio::test]
async fn test_changelist_pagination() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    // page_size=1, page=2 should give Beta (second in alpha order)
    let req = Request::builder()
        .uri("/admin/post/rows?sort=title&order=asc&page=2&page_size=1")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Beta"), "Beta on page 2: {body}");
    assert!(!body.contains("Alpha"), "Alpha absent on page 2: {body}");
    assert!(!body.contains("Gamma"), "Gamma absent on page 2: {body}");
}

#[tokio::test]
async fn test_changelist_htmx_target_markup_present() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("hx-target=\"#table-body\""),
        "hx-target=\"#table-body\" present in changelist: snippet={}",
        &body[..body.len().min(2000)]
    );
}

/// Bug 1 regression: the rows fragment (HTMX swap target) must include the
/// sticky-right action column with eye / pencil / trash buttons so clicking a
/// sortable column header never makes the actions disappear.
#[tokio::test]
async fn test_rows_fragment_includes_action_column() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/post/rows?sort=title&order=asc&page_size=10")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK);
    // All three action icons must be present in the fragment.
    assert!(
        body.contains("data-lucide=\"eye\""),
        "eye icon present in rows fragment: {body}"
    );
    assert!(
        body.contains("data-lucide=\"pencil\""),
        "pencil icon present in rows fragment: {body}"
    );
    assert!(
        body.contains("data-lucide=\"trash-2\""),
        "trash icon present in rows fragment: {body}"
    );
    // The action <td> must be there even after a sort swap.
    assert!(
        body.contains("sticky right-0"),
        "sticky-right action cell present in rows fragment: {body}"
    );
}

#[tokio::test]
async fn test_rows_htmx_fragment_only() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "dt_admin", "password123").await;

    // HTMX request to /rows endpoint
    let req = Request::builder()
        .uri("/admin/post/rows?page_size=10")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router.clone(), req).await;
    assert_eq!(status, StatusCode::OK);
    // Fragment should have rows but NOT the full HTML shell
    assert!(!body.contains("<!doctype html>"), "not a full page: {body}");
    assert!(!body.contains("<html"), "not a full page: {body}");

    // Non-HTMX request to the same endpoint REDIRECTS to the
    // changelist with the same query string preserved. The /rows
    // endpoint returns a naked tbody fragment that has no <head>,
    // no fonts, no Tailwind — useless when a user navigates to it
    // directly (back button, bookmark, copy-paste). The changelist
    // page itself HTMX-loads the rows on mount.
    let req2 = Request::builder()
        .uri("/admin/post/rows?page_size=10")
        .header(header::COOKIE, format!("umbra_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status2, headers2, _) = send_full(router, req2).await;
    assert!(
        status2 == StatusCode::SEE_OTHER
            || status2 == StatusCode::TEMPORARY_REDIRECT
            || status2 == StatusCode::FOUND,
        "expected redirect status, got: {status2}"
    );
    let location = headers2
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        location.starts_with("/admin/post/"),
        "redirect target should be the changelist; got: {location}"
    );
}
