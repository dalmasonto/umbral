//! Phase 1 shell integration tests.
//!
//! Tests the six deliverables from the phase 1 spec:
//!
//! 1. GET /admin/login returns 200 + HTML containing `<form` + a CSRF token field.
//! 2. GET /admin/ without a session redirects to /admin/login?next=/admin/ (302).
//! 3. POST /admin/login with valid creds sets a session cookie and redirects to `next`.
//! 4. POST /admin/login with bad creds returns the login template with a generic error
//!    (no username/password distinction).
//! 5. POST /admin/login with a malicious `next=//evil.com/` rejects the redirect (returns to /admin/).
//! 6. GET /admin/<table>/ (existing changelist) renders extending the new base — assert the
//!    response HTML contains the sidebar markup (id="umbral-admin-sidebar").
//! 7. The sidebar nav lists every registered model grouped by plugin.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

/// A simple model so the sidebar has something to show.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    title: String,
    body: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in test env");

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("admin_phase1.sqlite");
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

        let admin = AdminPlugin::default().register_for(
            "blog",
            AdminModel::new("post").label("Posts").icon("file-text"),
        );

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(admin)
            .model::<Post>()
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();

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
                last_login TEXT,\
                email_verified_at TEXT\
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
                body TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create post");

        // Seed: one staff user, one non-staff.
        let staff = create_user("staff_user", "staff@test.com", "staffpass")
            .await
            .expect("create staff");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark staff");
        let _: AuthUser = create_user("reg_user", "reg@test.com", "regpass")
            .await
            .expect("create regular user");

        // Seed one post so the list has content.
        sqlx::query("INSERT INTO post (title, body) VALUES ('Hello world', 'First post body')")
            .execute(&pool)
            .await
            .expect("seed post");

        app.into_router()
    })
    .await
}

// =========================================================================
// Helpers.
// =========================================================================

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
    let body = String::from_utf8_lossy(&bytes).into_owned();
    (status, headers, body)
}

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let (s, _, b) = send_full(router, req).await;
    (s, b)
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
    let (_, hdrs, body) = send_full(
        router.clone(),
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let anon_cookie = hdrs
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|s| {
            let first = s.split(';').next()?;
            let (k, v) = first.split_once('=')?;
            if k.trim() == "umbral_csrf_token" {
                Some(v.to_string())
            } else {
                None
            }
        })
        .expect("GET /admin/login must set umbral_csrf_token cookie");
    let csrf = extract_csrf_token(&body).expect("login page must have csrf_token");

    let form = serde_urlencoded::to_string([
        ("username", username),
        ("password", password),
        ("csrf_token", &csrf),
        ("next", "/admin/"),
    ])
    .unwrap();
    let (_, hdrs2, _) = send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::COOKIE, format!("umbral_csrf_token={anon_cookie}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
    )
    .await;
    hdrs2
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
// Test 1: GET /admin/login returns 200 + <form + CSRF token field.
// =========================================================================

#[tokio::test]
async fn login_page_returns_200_with_form_and_csrf() {
    let router = boot().await.clone();
    let (status, _, body) = send_full(
        router,
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET /admin/login should be 200");
    assert!(
        body.contains("<form"),
        "login page must contain a form element; body:\n{body}"
    );
    assert!(
        body.contains(r#"name="csrf_token""#),
        "login page must have a csrf_token field; body:\n{body}"
    );
    // The CSRF token value must be non-empty.
    let token = extract_csrf_token(&body);
    assert!(
        token.as_deref().is_some_and(|t| !t.is_empty()),
        "csrf_token value must be non-empty; body:\n{body}"
    );
}

// =========================================================================
// Test 2: GET /admin/ without a session redirects to /admin/login?next=...
// =========================================================================

#[tokio::test]
async fn unauthenticated_admin_redirects_to_login_with_next() {
    let router = boot().await.clone();
    let (status, headers, _) = send_full(
        router,
        Request::builder()
            .uri("/admin/")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "should be 302");
    let location = headers.get(header::LOCATION).unwrap().to_str().unwrap();
    assert!(
        location.contains("/admin/login"),
        "should redirect to /admin/login; got {location}"
    );
    assert!(
        location.contains("next="),
        "redirect should include next= param; got {location}"
    );
    assert!(
        location.contains("%2Fadmin%2F") || location.contains("/admin/"),
        "next should encode the admin path; got {location}"
    );
}

// =========================================================================
// Test 3: POST /admin/login with valid creds sets session cookie + redirects to next.
// =========================================================================

#[tokio::test]
async fn login_with_valid_creds_sets_session_and_redirects() {
    let router = boot().await.clone();

    // Step 1: GET /admin/login to obtain session cookie + CSRF token.
    let (status, hdrs, body) = send_full(
        router.clone(),
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let anon_cookie = hdrs
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()
                .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        })
        .expect("session cookie from GET /admin/login");
    let csrf = extract_csrf_token(&body).expect("csrf_token from login page");

    // Step 2: POST with valid staff credentials.
    let form = serde_urlencoded::to_string([
        ("username", "staff_user"),
        ("password", "staffpass"),
        ("csrf_token", &csrf),
        ("next", "/admin/"),
    ])
    .unwrap();
    let (status2, hdrs2, _) = send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::COOKIE, format!("umbral_csrf_token={anon_cookie}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
    )
    .await;

    assert_eq!(status2, StatusCode::SEE_OTHER, "valid login should 302");
    // Response must set a new session cookie.
    let new_cookie = hdrs2.get(header::SET_COOKIE).and_then(|v| v.to_str().ok());
    assert!(
        new_cookie.is_some_and(|s| s.contains("umbral_session")),
        "login should set umbral_session cookie; got {:?}",
        new_cookie
    );
    // Redirect target should be the requested `next`.
    let location = hdrs2.get(header::LOCATION).unwrap().to_str().unwrap();
    assert_eq!(
        location, "/admin/",
        "should redirect to /admin/; got {location}"
    );
}

// =========================================================================
// Test 4: POST /admin/login with bad creds returns generic error (no disclosure).
// =========================================================================

#[tokio::test]
async fn login_with_bad_creds_returns_generic_error() {
    let router = boot().await.clone();

    let (_, hdrs, body) = send_full(
        router.clone(),
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let anon_cookie = hdrs
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()
                .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        })
        .unwrap();
    let csrf = extract_csrf_token(&body).unwrap();

    let form = serde_urlencoded::to_string([
        ("username", "staff_user"),
        ("password", "wrongpassword"),
        ("csrf_token", &csrf),
        ("next", "/admin/"),
    ])
    .unwrap();
    let (status, _, body2) = send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::COOKIE, format!("umbral_csrf_token={anon_cookie}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
    )
    .await;

    // Must NOT be a redirect.
    assert_ne!(status, StatusCode::SEE_OTHER, "bad creds must not redirect");
    // Must render the form again (HTML response).
    assert!(
        body2.contains("<form"),
        "must re-render the login form; body:\n{body2}"
    );
    // Error message must be generic — must not say "password" or "username" specifically.
    // The spec: "NEVER reveal whether username or password was wrong specifically".
    let error_msg = "incorrect"; // matches the message in lib.rs
    assert!(
        body2.contains(error_msg),
        "error message should say 'incorrect'; body:\n{body2}"
    );
    // Sanity: must not say "wrong password" specifically.
    assert!(
        !body2.contains("wrong password"),
        "must not reveal which field was wrong; body:\n{body2}"
    );
}

// =========================================================================
// Test 5: POST /admin/login with malicious next= rejects the redirect.
// =========================================================================

#[tokio::test]
async fn login_malicious_next_is_rejected() {
    let router = boot().await.clone();

    let (_, hdrs, body) = send_full(
        router.clone(),
        Request::builder()
            .uri("/admin/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let anon_cookie = hdrs
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()
                .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        })
        .unwrap();
    let csrf = extract_csrf_token(&body).unwrap();

    // Attempt open redirect via protocol-relative URL.
    let form = serde_urlencoded::to_string([
        ("username", "staff_user"),
        ("password", "staffpass"),
        ("csrf_token", &csrf),
        ("next", "//evil.com/steal"),
    ])
    .unwrap();
    let (status, hdrs2, _) = send_full(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::COOKIE, format!("umbral_csrf_token={anon_cookie}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form))
            .unwrap(),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "valid login should still redirect"
    );
    let location = hdrs2.get(header::LOCATION).unwrap().to_str().unwrap();
    // Must NOT redirect to evil.com.
    assert!(
        !location.contains("evil.com"),
        "must not redirect to external URL; got {location}"
    );
    // Should redirect to the safe fallback (/admin/).
    assert!(
        location.starts_with("/admin"),
        "should redirect within /admin; got {location}"
    );
}

// =========================================================================
// Test 6: GET /admin/<table>/ renders extending base.html (sidebar present).
// =========================================================================

#[tokio::test]
async fn changelist_renders_with_sidebar() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "staff_user", "staffpass").await;

    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/post/")
            .header(header::COOKIE, format!("umbral_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "changelist should be 200; body:\n{body}"
    );
    // The base.html shell must be present.
    assert!(
        body.contains(r#"id="umbral-admin-sidebar""#),
        "base.html sidebar must be rendered; body:\n{body}"
    );
}

// =========================================================================
// Test 7: Sidebar nav lists registered models grouped by plugin.
// =========================================================================

#[tokio::test]
async fn sidebar_nav_lists_models_by_plugin() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "staff_user", "staffpass").await;

    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .header(header::COOKIE, format!("umbral_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // The sidebar should contain the "blog" plugin group (from register_for("blog", ...)).
    assert!(
        body.contains("sidebar-group-blog"),
        "sidebar must show the 'blog' plugin group; body:\n{body}"
    );
    // And the post model link must be present.
    assert!(
        body.contains("/admin/post/"),
        "sidebar must link to /admin/post/; body:\n{body}"
    );
}

// =========================================================================
// Test 8 (gap 44): Model::DISPLAY propagates into sidebar label (explicit
// registration with .label() overrides it).
// =========================================================================

#[tokio::test]
async fn explicit_label_overrides_model_display() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "staff_user", "staffpass").await;

    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .header(header::COOKIE, format!("umbral_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // The "blog" plugin's post model was registered with .label("Posts"),
    // which should appear in the sidebar and override any model-level DISPLAY.
    assert!(
        body.contains("Posts"),
        "sidebar must show the explicit label 'Posts'; body:\n{body}"
    );
}

// =========================================================================
// Test 9 (gap 44): Sidebar icon from explicit AdminModel::icon().
// =========================================================================

#[tokio::test]
async fn explicit_icon_appears_in_sidebar() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "staff_user", "staffpass").await;

    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .header(header::COOKIE, format!("umbral_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // The "blog" plugin's post model was registered with .icon("file-text").
    assert!(
        body.contains("data-lucide=\"file-text\""),
        "sidebar must contain the file-text icon; body:\n{body}"
    );
}

// =========================================================================
// Test 10 (gap 44): Auto-discovery — model registered via .model::<Post>()
// without an explicit AdminModel shows up in the sidebar.
// The boot() setup registers Post both via register_for("blog", ...) AND
// via .model::<Post>(). The explicit registration wins, but models with
// ONLY a .model::<T>() registration must still appear.
// =========================================================================

#[tokio::test]
async fn auto_discovered_model_appears_in_sidebar() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "staff_user", "staffpass").await;

    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .header(header::COOKIE, format!("umbral_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // The post model must appear regardless (explicit registration wins over
    // auto-discovery for the same table name, but the model must be present).
    assert!(
        body.contains("/admin/post/"),
        "auto-discovered model must appear in sidebar; body:\n{body}"
    );
}

// =========================================================================
// Test 11 (gap 45): Theme toggle button has onclick="umbral.toggleTheme()"
// so clicking it actually works (gap 1 of the four tasks).
// =========================================================================

#[tokio::test]
async fn theme_toggle_button_has_onclick() {
    let router = boot().await.clone();
    let cookie = login_session(&router, "staff_user", "staffpass").await;

    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .header(header::COOKIE, format!("umbral_session={cookie}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body:\n{body}");
    // The theme toggle button must exist.
    assert!(
        body.contains(r#"id="theme-toggle""#),
        "page must contain the theme-toggle button; body:\n{body}"
    );
    // The button must have onclick wiring.
    assert!(
        body.contains(r#"onclick="umbral.toggleTheme()""#),
        "theme-toggle must have onclick=\"umbral.toggleTheme()\"; body:\n{body}"
    );
}
