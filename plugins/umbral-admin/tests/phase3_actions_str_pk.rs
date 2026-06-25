#![allow(dead_code, private_interfaces)]
//! gaps2 #83 — bulk actions on non-i64 PKs.
//!
//! A String-PK model's selected ids were silently dropped by the old i64
//! parse: `filter_map(|v| v.parse::<i64>().ok())` discarded every string-slug
//! id, so `delete_selected` toasted "Deleted N" without touching any rows.
//!
//! Fix: ids flow through as `Vec<String>` and reach `filter_in_strings` which
//! dispatches on the column's SqlType — no forced i64 conversion.
//!
//! This file runs in its own test binary (separate from phase3_actions.rs)
//! to avoid the `Settings::init once` constraint that forbids two `App::build`
//! calls in the same process.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{Action, AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

/// A model whose PK is a String (slug-style) — gaps2 #83 regression target.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct Tag {
    pub id: String,
    pub label: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase3_str_pk.sqlite");
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

        let tag_config = AdminModel::new("tag")
            .list_display(&["id", "label"])
            .actions(vec![Action::delete_selected()]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(tag_config))
            .model::<Tag>()
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
                last_login TEXT\
            )",
        )
        .execute(&pool)
        .await
        .expect("auth_user");

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
        .expect("session");

        sqlx::query(
            "CREATE TABLE tag (\
                id TEXT PRIMARY KEY,\
                label TEXT NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .expect("tag");

        let staff = create_user("strpk_admin", "strpk@example.com", "pass123")
            .await
            .expect("user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("set staff");

        sqlx::query(
            "INSERT INTO tag (id, label) VALUES ('rust', 'Rust'), ('python', 'Python'), ('go', 'Go')",
        )
        .execute(&pool)
        .await
        .expect("seed tags");

        app.into_router()
    })
    .await
}

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker).unwrap_or(0);
    let window = &html[pos..(pos + 200).min(html.len())];
    let val = r#"value=""#;
    let vpos = window.find(val).unwrap_or(0);
    let after = &window[vpos + val.len()..];
    let end = after.find('"').unwrap_or(0);
    after[..end].to_string()
}

fn extract_cookie(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .and_then(|p| p.split_once('=').map(|(_, v)| v.to_string()))
        .unwrap_or_default()
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
        .expect("get login");
    let anon_raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let anon_cookie = extract_cookie(&anon_raw);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let csrf = extract_csrf(&html);
    let form = serde_urlencoded::to_string([
        ("username", "strpk_admin"),
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
                .header(header::COOKIE, format!("umbral_csrf_token={anon_cookie}"))
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .expect("post login");
    resp2
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(extract_cookie)
        .unwrap_or(anon_cookie)
}

/// gaps2 #83: bulk delete on a String-PK model via the HTMX dispatch path
/// (`POST /admin/{table}/actions/delete_selected` with JSON `{"ids":[...]}`).
///
/// Before the fix: string ids were silently dropped by
/// `filter_map(|x| x.as_i64())` / `v.parse::<i64>().ok()`, so zero rows
/// were deleted and the toast incorrectly reported "Deleted 2 row(s)."
///
/// After the fix: ids flow as `Vec<String>` to `filter_in_strings` which
/// dispatches on the column's SqlType (Text), and the toast count reflects
/// the actual `u64` returned by `DynQuerySet::delete()`.
#[tokio::test]
async fn test_delete_selected_string_pk_deletes_rows_and_reports_correct_count() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let pool = umbral::db::pool();

    // Rows must exist before the action runs.
    let before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tag WHERE id IN ('rust', 'python')")
            .fetch_one(&pool)
            .await
            .expect("count before");
    assert_eq!(before, 2, "rows must exist before delete");

    // Fire delete_selected with two String PKs.
    let (status, headers, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/tag/actions/delete_selected")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ids":["rust","python"]}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete_selected should return 200");

    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        trigger.contains("showToast"),
        "should return a toast trigger: {trigger}"
    );
    // The toast must report the actual rows affected (2), not a stale count
    // derived from the number of submitted ids.
    assert!(
        trigger.contains("Deleted 2"),
        "toast should report 2 deleted rows: {trigger}"
    );

    // Both selected rows must be gone.
    let after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tag WHERE id IN ('rust', 'python')")
            .fetch_one(&pool)
            .await
            .expect("count after");
    assert_eq!(after, 0, "both selected rows should be deleted");

    // The unselected row must survive.
    let untouched: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tag WHERE id = 'go'")
        .fetch_one(&pool)
        .await
        .expect("count untouched");
    assert_eq!(untouched, 1, "'go' row must not be deleted");
}
