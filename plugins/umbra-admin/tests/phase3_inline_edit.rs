#![allow(dead_code, private_interfaces)]
//! Phase 3 inline cell edit tests.
//!
//! Covers:
//! 1. GET /admin/cell_note/1/cell/title/edit returns field editor fragment.
//! 2. POST /admin/cell_note/1/cell/title with valid body updates the row, returns read-only cell.
//! 3. Read-only field returns 403.
//! 4. POST for a nonexistent row returns OK or 404 (UPDATE affects 0 rows but no server error).

#![allow(dead_code)]

use axum::body::Body;

use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbra_admin::{AdminModel, AdminPlugin};
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct CellNote {
    id: i64,
    title: String,
    body: String,
    published: bool,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase3_inline.sqlite");
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

        let note_config = AdminModel::new("cell_note")
            .list_display(&["title", "published"])
            .readonly_fields(&["body"])
            .inline_edit_fields(&["title", "published"]);

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(note_config))
            .model::<CellNote>()
            .build()
            .expect("build");

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
        .expect("auth_user");

        sqlx::query(
            "CREATE TABLE session (\
                id TEXT PRIMARY KEY,\
                user_id INTEGER,\
                data TEXT NOT NULL DEFAULT '{}',\
                created_at TEXT NOT NULL,\
                expires_at TEXT NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .expect("session");

        sqlx::query(
            "CREATE TABLE cell_note (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL DEFAULT '',\
                published INTEGER NOT NULL DEFAULT 0\
            )",
        )
        .execute(&pool)
        .await
        .expect("cell_note");

        sqlx::query(
            "INSERT INTO cell_note (title, body, published) VALUES ('Original Title', 'Body text', 0)",
        )
        .execute(&pool)
        .await
        .expect("seed");

        let staff = create_user("cell_admin", "cell@example.com", "pass123")
            .await
            .expect("user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("set staff");

        app.into_router()
    })
    .await
}

async fn send(
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

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker).unwrap_or(0);
    let window = &html[pos..(pos + 200).min(html.len())];
    let val = r#"value=""#;
    let vpos = window.find(val).unwrap_or(0);
    let after = &window[vpos + val.len()..];
    after[..after.find('"').unwrap_or(0)].to_string()
}

fn extract_cookie(s: &str) -> String {
    s.split(';')
        .next()
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .unwrap_or_default()
}

async fn login(router: axum::Router) -> String {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("get");
    let anon_raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let anon = extract_cookie(&anon_raw);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let csrf = extract_csrf(&String::from_utf8_lossy(&bytes));
    let form = serde_urlencoded::to_string([
        ("username", "cell_admin"),
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
                .header(header::COOKIE, format!("umbra_session={anon}"))
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .expect("post");
    resp2
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(extract_cookie)
        .unwrap_or(anon)
}

#[tokio::test]
async fn test_cell_edit_get_returns_editor_fragment() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, body) = send(
        router,
        Request::builder()
            .uri("/admin/cell_note/1/cell/title/edit")
            .header(header::COOKIE, format!("umbra_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cell edit GET ok: {body}");
    assert!(
        body.contains("<form") || body.contains("<input"),
        "editor fragment: {body}"
    );
    assert!(
        body.contains("title") || body.contains("Original"),
        "field name or value in fragment: {body}"
    );
    assert!(!body.contains("<!doctype"), "not full page: {body}");
}

#[tokio::test]
async fn test_cell_edit_post_updates_row() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/cell_note/1/cell/title")
            .header(header::COOKIE, format!("umbra_session={session}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("title=Updated+Cell+Title"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cell save ok: {body}");
    assert!(
        body.contains("Updated Cell Title"),
        "new value in response: {body}"
    );
    // Verify DB updated.
    let pool = umbra::db::pool();
    let title: String = sqlx::query_scalar("SELECT title FROM cell_note WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(title, "Updated Cell Title");
}

#[tokio::test]
async fn test_cell_edit_readonly_field_returns_403() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, _body) = send(
        router,
        Request::builder()
            .uri("/admin/cell_note/1/cell/body/edit")
            .header(header::COOKIE, format!("umbra_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "readonly field blocked");
}

#[tokio::test]
async fn test_cell_edit_post_nonexistent_row_returns_ok_or_404() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/cell_note/9999/cell/title")
            .header(header::COOKIE, format!("umbra_session={session}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("title=X"))
            .unwrap(),
    )
    .await;
    // Row doesn't exist — UPDATE affects 0 rows but doesn't error.
    assert!(
        status == StatusCode::OK || status == StatusCode::NOT_FOUND,
        "status: {status}"
    );
}
