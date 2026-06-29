#![allow(dead_code, private_interfaces)]
//! gaps2 #35 — admin trash UI for soft-delete models.
//!
//! Boots the admin against a soft-delete model (`Note`, tagged
//! `#[umbral(soft_delete)]`) and a plain model (`Tag`), logs in a staff
//! user, and exercises the trash workflow end-to-end through the real
//! HTTP handlers + assertions on DB state:
//!
//! (a) the default changelist excludes a soft-deleted row;
//! (b) `?trash=1` shows it (and offers Restore / Delete-permanently);
//! (c) the Restore action clears `deleted_at` (row back in the active list);
//! (d) Delete-permanently hard-removes the row (gone even from with_deleted);
//! (e) the default `delete_selected` on a soft-delete model SOFT-deletes
//!     (the row moves to trash, still present via `deleted_at`);
//! (f) a non-soft-delete model shows no Trash toggle.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{Action, AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(soft_delete, table = "sdadmin_note")]
struct Note {
    id: i64,
    #[umbral(string)]
    title: String,
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "sdadmin_tag")]
struct Tag {
    id: i64,
    #[umbral(string)]
    name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soft_delete_admin.sqlite");
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

        let note_config = AdminModel::new("sdadmin_note")
            .list_display(&["title"])
            .actions(vec![Action::delete_selected()]);
        let tag_config = AdminModel::new("sdadmin_tag").list_display(&["name"]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(
                AdminPlugin::default()
                    .register(note_config)
                    .register(tag_config),
            )
            .model::<Note>()
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
                last_login TEXT,\
                email_verified_at TEXT\
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
            "CREATE TABLE sdadmin_note (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                deleted_at TEXT\
            )",
        )
        .execute(&pool)
        .await
        .expect("note");

        sqlx::query(
            "CREATE TABLE sdadmin_tag (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .expect("tag");

        let staff = create_user("sd_admin", "sd@example.com", "pass123")
            .await
            .expect("user");
        sqlx::query("UPDATE auth_user SET is_staff = 1, is_superuser = 1 WHERE id = ?")
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
        ("username", "sd_admin"),
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

/// Insert one note, return its id.
async fn insert_note(title: &str) -> i64 {
    let pool = umbral::db::pool();
    sqlx::query("INSERT INTO sdadmin_note (title) VALUES (?)")
        .bind(title)
        .execute(&pool)
        .await
        .expect("insert note");
    sqlx::query_scalar("SELECT id FROM sdadmin_note WHERE title = ? ORDER BY id DESC LIMIT 1")
        .bind(title)
        .fetch_one(&pool)
        .await
        .expect("note id")
}

/// `deleted_at` value for a note id (None when live / row absent).
async fn deleted_at(id: i64) -> Option<Option<String>> {
    let pool = umbral::db::pool();
    sqlx::query_scalar::<_, Option<String>>("SELECT deleted_at FROM sdadmin_note WHERE id = ?")
        .bind(id)
        .fetch_optional(&pool)
        .await
        .expect("query deleted_at")
}

async fn dispatch(router: axum::Router, session: &str, uri: &str, ids: &[i64]) -> StatusCode {
    let ids_json: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    let body = format!(r#"{{"ids":[{}]}}"#, ids_json.join(","));
    let (status, _h, _b) = send(
        router,
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    status
}

async fn changelist(router: axum::Router, session: &str, uri: &str) -> String {
    let (status, _h, body) = send(
        router,
        Request::builder()
            .uri(uri)
            .header(header::COOKIE, format!("umbral_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "changelist {uri} 200");
    body
}

// (a)+(b): default changelist excludes a soft-deleted row; `?trash=1`
// shows it. We soft-delete via the admin's default delete action, then
// read both views.
#[tokio::test]
async fn trash_filter_hides_then_shows_soft_deleted_row() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let id = insert_note("Alpha note").await;

    // Soft-delete it through the admin's default delete action.
    let st = dispatch(
        router.clone(),
        &session,
        "/admin/sdadmin_note/actions/delete_selected",
        &[id],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Active list: title gone.
    let active = changelist(router.clone(), &session, "/admin/sdadmin_note/").await;
    assert!(
        !active.contains("Alpha note"),
        "soft-deleted row hidden from active list"
    );

    // Trash list: title present + the trash-only affordances render.
    let trash = changelist(router.clone(), &session, "/admin/sdadmin_note/?trash=1").await;
    assert!(trash.contains("Alpha note"), "row shows in ?trash=1 view");
    assert!(
        trash.contains("restore_selected"),
        "Restore action offered in trash view"
    );
    assert!(
        trash.contains("delete_permanently"),
        "Delete-permanently offered in trash view"
    );
}

// (e): default delete on a soft-delete model SOFT-deletes — the row is
// still in the table, just stamped with deleted_at.
#[tokio::test]
async fn default_delete_soft_deletes_to_trash() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let id = insert_note("Beta note").await;
    assert_eq!(deleted_at(id).await, Some(None), "starts live");

    let st = dispatch(
        router.clone(),
        &session,
        "/admin/sdadmin_note/actions/delete_selected",
        &[id],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Row still present (not hard-deleted) and deleted_at is populated.
    match deleted_at(id).await {
        Some(Some(_)) => {} // soft-deleted: row exists, deleted_at set
        other => panic!("expected row present with deleted_at set, got {other:?}"),
    }
}

// (c): Restore clears deleted_at — row returns to the active list.
#[tokio::test]
async fn restore_action_clears_deleted_at() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let id = insert_note("Gamma note").await;
    // Soft-delete then restore.
    dispatch(
        router.clone(),
        &session,
        "/admin/sdadmin_note/actions/delete_selected",
        &[id],
    )
    .await;
    assert!(matches!(deleted_at(id).await, Some(Some(_))), "trashed");

    let st = dispatch(
        router.clone(),
        &session,
        "/admin/sdadmin_note/actions/restore_selected",
        &[id],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    assert_eq!(deleted_at(id).await, Some(None), "deleted_at cleared");

    // Back in the active changelist.
    let active = changelist(router.clone(), &session, "/admin/sdadmin_note/").await;
    assert!(active.contains("Gamma note"), "restored row in active list");
}

// (d): Delete-permanently hard-removes the row — gone even from with_deleted.
#[tokio::test]
async fn delete_permanently_hard_removes_row() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let id = insert_note("Delta note").await;
    dispatch(
        router.clone(),
        &session,
        "/admin/sdadmin_note/actions/delete_selected",
        &[id],
    )
    .await;
    assert!(matches!(deleted_at(id).await, Some(Some(_))), "in trash");

    let st = dispatch(
        router.clone(),
        &session,
        "/admin/sdadmin_note/actions/delete_permanently",
        &[id],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Row absent entirely (None from fetch_optional means no row).
    assert_eq!(deleted_at(id).await, None, "row hard-deleted from table");
}

// (f): a non-soft-delete model shows no Trash toggle and ignores ?trash=1.
#[tokio::test]
async fn non_soft_delete_model_has_no_trash_toggle() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let pool = umbral::db::pool();
    sqlx::query("INSERT INTO sdadmin_tag (name) VALUES ('rust')")
        .execute(&pool)
        .await
        .expect("seed tag");

    let body = changelist(router.clone(), &session, "/admin/sdadmin_tag/").await;
    // The trash toggle link `?trash=1` only renders for soft-delete models.
    assert!(
        !body.contains("?trash=1"),
        "non-soft-delete model has no trash toggle"
    );
    assert!(
        !body.contains("restore_selected"),
        "non-soft-delete model offers no restore action"
    );
}
