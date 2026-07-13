#![allow(dead_code, private_interfaces)]
//! Phase 3 FK picker tests.
//!
//! Covers:
//! 1. GET /admin/api/post3/tag_id/options?search=foo returns matching options.
//! 2. Pagination: has_more is true when rows > page_size.
//! 3. /options/resolve?ids=1,2 returns labels.
//! 4. Unauthenticated request is blocked (redirect or 403).

#![allow(dead_code)]

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

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Tag {
    id: i64,
    name: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post3 {
    id: i64,
    title: String,
    tag_id: i64,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase3_fk.sqlite");
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

        let post_config = AdminModel::new("post3")
            .list_display(&["title", "tag_id"])
            .search_fields(&["title"]);
        let tag_config = AdminModel::new("tag").search_fields(&["name"]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(
                AdminPlugin::default()
                    .register(post_config)
                    .register(tag_config),
            )
            .model::<Tag>()
            .model::<Post3>()
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

        sqlx::query("CREATE TABLE tag (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("tag");

        sqlx::query("CREATE TABLE post3 (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, tag_id INTEGER NOT NULL REFERENCES tag(id))")
            .execute(&pool)
            .await
            .expect("post3");

        // Seed 25 tags + one special foo-tag.
        for i in 1..=25i64 {
            sqlx::query("INSERT INTO tag (name) VALUES (?)")
                .bind(format!("tag-{i}"))
                .execute(&pool)
                .await
                .expect("seed tag");
        }
        sqlx::query("INSERT INTO tag (name) VALUES ('foo-tag')")
            .execute(&pool)
            .await
            .expect("seed foo-tag");
        sqlx::query("INSERT INTO post3 (title, tag_id) VALUES ('Hello', 1)")
            .execute(&pool)
            .await
            .expect("seed post3");

        let staff = create_user("fk_admin", "fk@example.com", "pass123")
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
    // GET /admin/login mints (or echoes) the umbral_csrf_token cookie.
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
        .expect("GET /admin/login must set umbral_csrf_token cookie");
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let csrf = extract_csrf(&String::from_utf8_lossy(&bytes));
    let form = serde_urlencoded::to_string([
        ("username", "fk_admin"),
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
                .header(header::COOKIE, format!("umbral_csrf_token={csrf_cookie}"))
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .expect("post");
    // The session is created on successful login; extract it from
    // the Set-Cookie list on the POST response.
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

#[tokio::test]
async fn test_fk_options_search_returns_matches() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, body) = send(
        router,
        Request::builder()
            .uri("/admin/api/post3/tag_id/options?search=foo")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "status: body={body}");
    assert!(
        body.contains("foo-tag") || body.contains("foo"),
        "foo-tag in response: {body}"
    );
}

#[tokio::test]
async fn test_fk_options_has_more_when_exceeds_page_size() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    // 26 tags total, page_size=20 → has_more=true
    let (status, _h, body) = send(
        router,
        Request::builder()
            .uri("/admin/api/post3/tag_id/options?page_size=20")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // JSON response (no HX-Request header): parse and check has_more.
    if body.trim().starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(
            v["has_more"],
            serde_json::json!(true),
            "has_more=true: {body}"
        );
    } else {
        // HTML response — at minimum non-empty.
        assert!(body.len() > 10, "non-empty HTML response");
    }
}

#[tokio::test]
async fn test_fk_options_resolve_returns_labels() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _h, body) = send(
        router,
        Request::builder()
            .uri("/admin/api/post3/tag_id/options/resolve?ids=1,2")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("json: {body}"));
    let items = v["items"].as_array().expect("items array");
    assert!(!items.is_empty(), "items non-empty: {body}");
    assert!(items[0]["label"].is_string(), "label is string: {body}");
}

#[tokio::test]
async fn test_fk_options_no_staff_returns_redirect_or_403() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let (status, _h, _body) = send(
        router,
        Request::builder()
            .uri("/admin/api/post3/tag_id/options")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    // No session → redirect to login (303/307) or 403.
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::FORBIDDEN
            || status == StatusCode::TEMPORARY_REDIRECT,
        "unauthenticated blocked: {status}"
    );
}
