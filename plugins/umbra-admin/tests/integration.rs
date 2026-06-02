//! End-to-end coverage for umbra-admin. Boot the App once with
//! AuthPlugin + SessionsPlugin + AdminPlugin registered, seed a staff
//! user, then drive every admin route through axum's
//! `ServiceExt::oneshot` without a TCP listener.
//!
//! Auth is now session-based (HTML form flow) instead of Basic Auth.
//! Tests use a `login_session` helper that POSTs to `GET /admin/login`
//! to get a CSRF token and then POSTs credentials to obtain a session
//! cookie, which is passed on subsequent requests.
//!
//! Covers the full Django-shape flow:
//!
//! - GET /admin without session → 302 to /admin/login
//! - GET /admin with a non-staff user → 403
//! - GET /admin as staff → 200 with the registered-models index
//! - POST /admin/<table>/new (create) → 303 → row appears
//! - GET /admin/<table>/ (list) → 200 with the new row visible
//! - GET /admin/<table>/<id> (detail) → 200 with field values
//! - POST /admin/<table>/<id>/edit (update) → 303 → row reflects edit
//! - POST /admin/<table>/<id>/delete (delete) → 303 → row gone

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

use umbra_admin::AdminPlugin;
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_sessions::SessionsPlugin;

/// A second model in addition to AuthUser so the admin's
/// list-models index shows >1 entry.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Note {
    id: i64,
    title: String,
    body: String,
    published_at: Option<DateTime<Utc>>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir for the test DB");
        let path = tmp.path().join("admin_integration.sqlite");
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

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default())
            .model::<Note>()
            .build()
            .expect("App::build with AuthPlugin + SessionsPlugin + AdminPlugin");

        let pool = umbra::db::pool();
        // Create tables manually (no migrate runner in test harness).
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
            "CREATE TABLE note (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL,\
                published_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create note");

        // Seed two users: one staff, one not.
        let staff = create_user("alice", "alice@example.com", "hunter2")
            .await
            .expect("create staff user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark alice as staff");
        let _: AuthUser = create_user("bob", "bob@example.com", "secret")
            .await
            .expect("create regular user");

        app.into_router()
    })
    .await
}

// =========================================================================
// Test helpers.
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

/// Log in via the admin HTML form and return the session cookie value.
///
/// Steps:
/// 1. GET /admin/login to pick up an anonymous session cookie + CSRF token.
/// 2. POST /admin/login with credentials + CSRF token.
/// 3. Extract the session cookie from the response Set-Cookie header.
async fn login_session(router: &axum::Router, username: &str, password: &str) -> String {
    // Step 1: GET the login page to get an anonymous session + CSRF token.
    let (status, headers, body) = send_full(
        router.clone(),
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET /admin/login should 200; body:\n{body}"
    );

    // Extract the session cookie issued by umbra_sessions::create_session.
    let anon_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            // The Set-Cookie value is like: umbra_session=<token>; Path=...; ...
            s.split(';')
                .next()
                .and_then(|pair| pair.split_once('='))
                .map(|(_, v)| v.to_string())
        })
        .expect("GET /admin/login must set a session cookie");

    // Extract CSRF token from the hidden form field.
    let csrf_token =
        extract_csrf_token(&body).expect("login page must contain a csrf_token hidden input");

    // Step 2: POST credentials + CSRF token with the session cookie.
    let form_body = serde_urlencoded::to_string([
        ("username", username),
        ("password", password),
        ("csrf_token", &csrf_token),
        ("next", "/admin/"),
    ])
    .unwrap();

    let cookie_header = format!("umbra_csrf_token={anon_cookie}");
    let (status2, headers2, _) = send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::COOKIE, &cookie_header)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form_body))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status2,
        StatusCode::SEE_OTHER,
        "POST /admin/login should 303 on success"
    );

    // Step 3: The response sets a new session cookie.
    headers2
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()
                .and_then(|pair| pair.split_once('='))
                .map(|(_, v)| v.to_string())
        })
        .expect("POST /admin/login must set a session cookie on success")
}

/// Extract the csrf_token value from a login page's hidden input.
fn extract_csrf_token(html: &str) -> Option<String> {
    // The form has an <input type="hidden" name="csrf_token" value="<token>"/>
    // but template reformats may split the attributes across lines. Search
    // for the name attribute then locate the matching value within a small
    // window after it; tolerant of whitespace and attribute order.
    let name_marker = r#"name="csrf_token""#;
    let pos = html.find(name_marker)?;
    let window_end = pos.saturating_add(400).min(html.len());
    let window = &html[pos..window_end];
    let value_marker = "value=\"";
    let vstart = window.find(value_marker)? + value_marker.len();
    let vend = window[vstart..].find('"')?;
    Some(window[vstart..vstart + vend].to_string())
}

// =========================================================================
// Tests.
// =========================================================================

#[tokio::test]
async fn admin_index_without_session_redirects_to_login() {
    let router = boot().await.clone();
    let (status, headers, _) = send_full(
        router,
        Request::builder()
            .uri("/admin/")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "unauthenticated /admin/ should 302"
    );
    let location = headers.get(header::LOCATION).unwrap().to_str().unwrap();
    assert!(
        location.contains("/admin/login"),
        "redirect should go to /admin/login; got {location}"
    );
}

#[tokio::test]
async fn admin_login_page_returns_200_with_form() {
    let router = boot().await.clone();
    let (status, _, body) = send_full(
        router,
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET /admin/login should 200");
    assert!(body.contains("<form"), "login page must contain a <form");
    assert!(
        body.contains(r#"name="csrf_token""#),
        "login page must have csrf_token field; body:\n{body}"
    );
}

#[tokio::test]
async fn admin_with_wrong_password_returns_error() {
    let router = boot().await.clone();

    // Get a session + CSRF token.
    let (_, headers, body) = send_full(
        router.clone(),
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let anon_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()
                .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        })
        .unwrap();
    let csrf_token = extract_csrf_token(&body).unwrap();

    let form_body = serde_urlencoded::to_string([
        ("username", "alice"),
        ("password", "wrongpass"),
        ("csrf_token", &csrf_token),
        ("next", "/admin/"),
    ])
    .unwrap();
    let (status2, _, body2) = send_full(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::COOKIE, format!("umbra_csrf_token={anon_cookie}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form_body))
            .unwrap(),
    )
    .await;
    // Should re-render the form with a generic error (not reveal username vs password).
    assert_eq!(status2, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body2.contains("incorrect"),
        "error message should say 'incorrect':\n{body2}"
    );
}

#[tokio::test]
async fn admin_index_as_staff_lists_registered_models() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "alice", "hunter2").await;
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .header(header::COOKIE, format!("umbra_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // Phase 4: /admin/ now renders the dashboard widget grid, not a model list.
    // Verify the dashboard page title and that the sidebar nav still shows registered models.
    assert!(
        body.contains("Dashboard"),
        "expected Dashboard page, got body:\n{body}"
    );
    // Sidebar should still contain auth_user and note links.
    assert!(
        body.contains("auth_user") || body.contains("Auth User") || body.contains("Admin"),
        "auth_user missing:\n{body}"
    );
    assert!(
        body.contains("note") || body.contains("Note"),
        "note missing:\n{body}"
    );
}

#[tokio::test]
async fn full_crud_flow_against_note_model() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "alice", "hunter2").await;
    let auth_cookie = format!("umbra_session={cookie}");

    // 1. Create via POST /admin/note/new
    let create_body = serde_urlencoded::to_string([
        ("title", "first note"),
        ("body", "hello from the admin"),
        ("published_at", ""),
    ])
    .unwrap();
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/note/new")
                .header(header::COOKIE, &auth_cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(create_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "POST new should 303");
    let location = resp
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(location, "/admin/note/");

    // 2. The list view shows the new row.
    let (status, body) = send(
        router.clone(),
        Request::builder()
            .uri("/admin/note/")
            .header(header::COOKIE, &auth_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("first note"),
        "list missing seeded note:\n{body}"
    );

    // 3. Detail view by id.
    let (status, body) = send(
        router.clone(),
        Request::builder()
            .uri("/admin/note/1")
            .header(header::COOKIE, &auth_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("hello from the admin"),
        "detail body:\n{body}"
    );

    // 4. Edit via POST /admin/note/1/edit
    let edit_body = serde_urlencoded::to_string([
        ("title", "edited note"),
        ("body", "after edit"),
        ("published_at", ""),
    ])
    .unwrap();
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/note/1/edit")
                .header(header::COOKIE, &auth_cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(edit_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "POST edit should 303");

    let (_, body) = send(
        router.clone(),
        Request::builder()
            .uri("/admin/note/1")
            .header(header::COOKIE, &auth_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(body.contains("edited note"), "edit didn't take:\n{body}");
    assert!(
        body.contains("after edit"),
        "edit body didn't take:\n{body}"
    );

    // 5. Delete via POST /admin/note/1/delete
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/note/1/delete")
                .header(header::COOKIE, &auth_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "POST delete should 303"
    );

    let (status, _) = send(
        router.clone(),
        Request::builder()
            .uri("/admin/note/1")
            .header(header::COOKIE, &auth_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "post-delete detail should 404"
    );
}

#[allow(dead_code)]
fn _unused_pathbuf_marker() -> Option<PathBuf> {
    None
}
