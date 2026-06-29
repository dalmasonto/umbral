//! Phase 4 audit log tests.
//!
//! 1. Creating a row via POST /admin/{table}/new writes an audit entry.
//! 2. Deleting a row via DELETE writes an audit entry.
//! 3. GET /admin/{table}/{id}/history renders the timeline.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbral_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Note {
    id: i64,
    content: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase4_audit.sqlite");
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
            .plugin(
                AdminPlugin::default().register(
                    AdminModel::new("note")
                        .list_display(&["id", "content"])
                        .search_fields(&["content"]),
                ),
            )
            .model::<Note>()
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

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS note (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                content TEXT NOT NULL\
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
    // Create or reuse the staff user (unique constraint may fire on second call).
    let user = match create_user_with_flags(
        "audit_user",
        "audit@example.com",
        "pass123",
        true,
        false,
    )
    .await
    {
        Ok(u) => u,
        Err(_) => {
            // User already exists — look it up directly.
            let pool = umbral::db::pool();
            sqlx::query_as::<_, umbral_auth::AuthUser>(
                "SELECT id, username, email, password_hash, is_active, is_staff, is_superuser, date_joined, last_login, email_verified_at \
                 FROM auth_user WHERE username = 'audit_user'",
            )
            .fetch_one(&pool)
            .await
            .expect("lookup existing audit_user")
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
async fn create_writes_audit_row() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let req = Request::builder()
        .method("POST")
        .uri("/admin/note/new")
        .header(header::COOKIE, cookie.clone())
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("content=hello+world"))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    // Either 303 redirect (success) or 200 form re-render
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "unexpected status: {status}"
    );

    let pool = umbral::db::pool();
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log WHERE action = 'create' AND model = 'note'",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or(0);
    assert!(count >= 1, "expected ≥1 create audit row, got {count}");
}

#[tokio::test]
async fn delete_writes_audit_row() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;
    let pool = umbral::db::pool();

    let row_id: i64 =
        sqlx::query_scalar("INSERT INTO note (content) VALUES ('to delete') RETURNING id")
            .fetch_one(&pool)
            .await
            .expect("insert note");

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/admin/note/{row_id}"))
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert!(
        resp.status() == StatusCode::OK || resp.status() == StatusCode::SEE_OTHER,
        "unexpected: {}",
        resp.status()
    );

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log WHERE action = 'delete' AND model = 'note'",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or(0);
    assert!(count >= 1, "expected ≥1 delete audit row, got {count}");
}

#[tokio::test]
async fn history_endpoint_renders_timeline() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;
    let pool = umbral::db::pool();

    let row_id: i64 =
        sqlx::query_scalar("INSERT INTO note (content) VALUES ('history test') RETURNING id")
            .fetch_one(&pool)
            .await
            .expect("insert note");

    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO admin_audit_log
            (actor_user_id, action, model, object_id, diff_summary, created_at)
         VALUES (1, 'create', 'note', ?, 'created Note', ?)",
    )
    .bind(row_id)
    .bind(&now)
    .execute(&pool)
    .await
    .expect("insert audit");

    let req = Request::builder()
        .uri(format!("/admin/note/{row_id}/history"))
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("History"), "missing History header");
    assert!(html.contains("created Note"), "missing audit entry");
}
