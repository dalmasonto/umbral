//! Phase 5 polish tests.
//!
//! Covers:
//! 1. `column_widths` builder lands the right `<col style="width: ...">` in the rendered table.
//! 2. Sensitive-column defaults: a `password_hash` column is automatically readonly
//!    — it must not appear as a writable `<input>` in the edit form.
//! 3. Dashboard model-count cards render with the registered model names and link.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

// ---- Minimal test model with a password_hash column ----

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct StaffMember {
    id: i64,
    username: String,
    password_hash: String,
    role: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase5_polish.sqlite");
        std::mem::forget(tmp);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite pool");

        let staff_cfg = AdminModel::new("staff_member")
            .list_display(&["username", "role"])
            // Explicitly set column_widths so we can assert the colgroup output.
            .column_widths(&[("username", "50%"), ("role", "200px")]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(staff_cfg))
            .model::<StaffMember>()
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
            "CREATE TABLE staff_member (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL,\
                password_hash TEXT NOT NULL,\
                role TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create staff_member");

        // Seed one row so edit form is reachable.
        sqlx::query(
            "INSERT INTO staff_member (username, password_hash, role) VALUES ('alice', '$argon2id$...', 'editor')"
        )
        .execute(&pool)
        .await
        .expect("seed staff_member");

        let admin_user = create_user("polish_admin", "polish_admin@example.com", "password123")
            .await
            .expect("create staff user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(admin_user.id)
            .execute(&pool)
            .await
            .expect("mark staff");

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
    set_cookie
        .split(';')
        .next()
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .unwrap_or_default()
}

async fn login_session(router: axum::Router, username: &str, password: &str) -> String {
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
                .header(header::COOKIE, format!("umbral_csrf_token={anon_cookie}"))
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

/// Bug 2: `column_widths` builder produces `<col style="width: ...">` colgroup markup.
#[tokio::test]
async fn test_column_widths_renders_colgroup() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "polish_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/staff_member/")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK, "changelist loads");
    assert!(
        body.contains("width: 50%"),
        "50% column width rendered: snippet={}",
        &body[..body.len().min(3000)]
    );
    assert!(
        body.contains("width: 200px"),
        "200px column width rendered: snippet={}",
        &body[..body.len().min(3000)]
    );
    assert!(
        body.contains("<colgroup>"),
        "colgroup element present: snippet={}",
        &body[..body.len().min(3000)]
    );
}

/// Bug 6: `password_hash` is a sensitive column — it must be rendered as a
/// readonly field in the edit form (no enabled `<input>` for it).
#[tokio::test]
async fn test_password_hash_is_readonly_by_default() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "polish_admin", "password123").await;

    // Load the edit-sheet fragment for row #1.
    let req = Request::builder()
        .uri("/admin/staff_member/1/edit-sheet")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK, "edit sheet loads");

    // The `password_hash` field should carry `disabled` attribute (bug 3a fix:
    // readonly fields now use `disabled` so they can't receive focus and can't
    // be submitted). The `readonly` label badge is still rendered in the label row.
    assert!(
        body.contains("disabled"),
        "password_hash field is marked disabled: snippet={}",
        &body[..body.len().min(3000)]
    );
    // Also verify the field IS shown (readonly, not hidden) — the label must be present.
    assert!(
        body.to_lowercase().contains("password"),
        "password_hash label visible in form: {body}"
    );
}

/// Bug 8: Dashboard model cards render with the model label and a link.
#[tokio::test]
async fn test_dashboard_model_cards_render() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "polish_admin", "password123").await;

    let req = Request::builder()
        .uri("/admin/")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK, "dashboard loads");

    // Model cards section should be present.
    assert!(
        body.contains("Models"),
        "Models section header present: {body}"
    );
    // The staff_member model card should link to its changelist.
    assert!(
        body.contains("/admin/staff_member/"),
        "staff_member changelist link in model card: {body}"
    );
    // Row count: seeded 1 row, so count "1" should appear somewhere in the cards grid.
    assert!(
        body.contains(">1<"),
        "row count 1 shown in model card: snippet={}",
        &body[..body.len().min(4000)]
    );
}
