#![allow(dead_code, private_interfaces)]
//! Regression tests: admin fragments must honour a custom base path.
//!
//! When the admin is mounted at a path other than `/admin` (e.g.
//! `/backoffice`), every URL emitted into HTML fragments — HTMX
//! `hx-post`/`hx-get` attributes, pagination `href`s, etc. — must
//! use the configured base path, not the literal string `/admin`.
//!
//! Covers:
//! 1. inline-edit GET fragment: `hx-post` uses `/backoffice/...`, not `/admin/...`.
//! 2. fk-picker pagination fragment: `hx-get` uses `/backoffice/...`, not `/admin/...`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

// ── Models ───────────────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct BpNote {
    id: i64,
    title: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct BpTag {
    id: i64,
    name: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct BpPost {
    id: i64,
    title: String,
    // Field named `bp_tag_id` so the FK target resolves to `bp_tag`
    // (the handler strips `_id` from the column name to find the related table).
    bp_tag_id: i64,
}

// ── Boot ─────────────────────────────────────────────────────────────────────

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

/// Boot an admin app mounted at `/backoffice` — the non-default base path
/// that exercises the base-path-in-fragments bug.
async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("base_path_fragments.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let note_config = AdminModel::new("bp_note")
            .list_display(&["title"])
            .inline_edit_fields(&["title"]);
        let tag_config = AdminModel::new("bp_tag").search_fields(&["name"]);
        let post_config = AdminModel::new("bp_post")
            .list_display(&["title", "bp_tag_id"])
            .search_fields(&["title"]);

        // Mount admin at /backoffice (not the default /admin).
        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(
                AdminPlugin::default()
                    .at("/backoffice")
                    .register(note_config)
                    .register(tag_config)
                    .register(post_config),
            )
            .model::<BpNote>()
            .model::<BpTag>()
            .model::<BpPost>()
            .build()
            .expect("build");

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
            "CREATE TABLE bp_note (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .expect("bp_note");

        sqlx::query(
            "CREATE TABLE bp_tag (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("bp_tag");

        sqlx::query(
            "CREATE TABLE bp_post (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                bp_tag_id INTEGER NOT NULL REFERENCES bp_tag(id)\
            )",
        )
        .execute(&pool)
        .await
        .expect("bp_post");

        sqlx::query("INSERT INTO bp_note (title) VALUES ('Backoffice Note')")
            .execute(&pool)
            .await
            .expect("seed note");

        // Seed 25 tags so total > page_size=20 → pagination appears.
        for i in 1..=25i64 {
            sqlx::query("INSERT INTO bp_tag (name) VALUES (?)")
                .bind(format!("bp-tag-{i}"))
                .execute(&pool)
                .await
                .expect("seed tag");
        }

        sqlx::query("INSERT INTO bp_post (title, bp_tag_id) VALUES ('Post A', 1)")
            .execute(&pool)
            .await
            .expect("seed post");

        let staff = create_user("bp_admin", "bp@example.com", "Xq7vBramble42x")
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

// ── Helpers ───────────────────────────────────────────────────────────────────

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

async fn login(router: axum::Router) -> String {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/backoffice/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("get login");

    let csrf_cookie = resp
        .headers()
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
        .expect("GET /backoffice/login must set umbral_csrf_token cookie");

    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let csrf = extract_csrf(&String::from_utf8_lossy(&bytes));

    let form = serde_urlencoded::to_string([
        ("username", "bp_admin"),
        ("password", "Xq7vBramble42x"),
        ("csrf_token", csrf.as_str()),
        ("next", "/backoffice/"),
    ])
    .unwrap();

    let resp2 = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/backoffice/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, format!("umbral_csrf_token={csrf_cookie}"))
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .expect("post login");

    resp2
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|s| {
            let first = s.split(';').next()?;
            let (k, v) = first.split_once('=')?;
            if k.trim() == "umbral_session" {
                Some(v.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The inline-edit GET fragment's `hx-post` must point at the
/// configured base path, not the literal `/admin/...`.
#[tokio::test]
async fn test_inline_edit_fragment_uses_base_path() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let (status, _h, body) = send(
        router,
        Request::builder()
            .uri("/backoffice/bp_note/1/cell/title/edit")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "inline-edit GET ok: {body}");
    assert!(
        body.contains("<form") || body.contains("<input"),
        "editor fragment returned: {body}"
    );
    assert!(
        body.contains("/backoffice/"),
        "fragment must contain /backoffice/ in hx-post: {body}"
    );
    assert!(
        !body.contains("\"/admin/"),
        "fragment must NOT contain hardcoded /admin/: {body}"
    );
}

/// The FK-picker pagination links (`hx-get`) must point at the
/// configured base path, not the literal `/admin/...`.
#[tokio::test]
async fn test_fk_picker_pagination_uses_base_path() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    // 25 tags, page_size=20 → has_more=true → pagination rendered.
    // Send with HX-Request: true to get the HTML fragment (not JSON).
    let (status, _h, body) = send(
        router,
        Request::builder()
            .uri("/backoffice/api/bp_post/bp_tag_id/options?page_size=20")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "fk-picker GET ok: {body}");
    assert!(
        body.contains("Previous") || body.contains("Next"),
        "pagination buttons present: {body}"
    );
    assert!(
        body.contains("/backoffice/"),
        "pagination hx-get must contain /backoffice/: {body}"
    );
    assert!(
        !body.contains("\"/admin/"),
        "pagination must NOT contain hardcoded /admin/: {body}"
    );
}
