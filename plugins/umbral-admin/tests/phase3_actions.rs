#![allow(dead_code, private_interfaces)]
//! Phase 3 action tests.
//!
//! Covers:
//! 1. Custom action declared in AdminModel appears in list page markup.
//! 2. POST /admin/{table}/actions/{key} with row id invokes handler, returns HX-Trigger showToast.
//! 3. Bulk action with multiple ids hits handler with ids.len() > 1.
//! 4. Unknown action key returns 404.
//! 5. delete_selected action deletes the row and returns Toast trigger.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::{
    Action, ActionInvocation, ActionResult, ActionScope, AdminModel, AdminPlugin, ToastLevel,
};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Article {
    id: i64,
    title: String,
    published: bool,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase3_actions.sqlite");
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

        let publish_action = Action::new(
            "publish",
            "Publish",
            "send",
            |inv: ActionInvocation| async move {
                Ok(ActionResult::Toast {
                    message: format!("Published {} item(s).", inv.ids.len()),
                    level: ToastLevel::Success,
                })
            },
        )
        .scope(ActionScope::Both);

        let article_config = AdminModel::new("article")
            .list_display(&["title", "published"])
            .actions(vec![Action::delete_selected(), publish_action]);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(article_config))
            .model::<Article>()
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();



        let staff = create_user("act_admin", "act@example.com", "pass123")
            .await
            .expect("user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("set staff");

        sqlx::query(
            "INSERT INTO article (title, published) VALUES ('Article 1', 0), ('Article 2', 0), ('Article 3', 0)",
        )
        .execute(&pool)
        .await
        .expect("seed");

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
        ("username", "act_admin"),
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

#[tokio::test]
async fn test_custom_action_appears_in_changelist() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _headers, body) = send(
        router,
        Request::builder()
            .uri("/admin/article/")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("publish") || body.contains("Publish"),
        "publish action in page: snippet={}",
        &body[..body.len().min(2000)]
    );
}

#[tokio::test]
async fn test_row_action_dispatch_returns_toast_trigger() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, headers, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/article/actions/publish")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ids":[1]}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dispatch returns 200");
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        trigger.contains("showToast"),
        "HX-Trigger showToast present: {trigger}"
    );
    assert!(trigger.contains("success"), "level=success: {trigger}");
}

#[tokio::test]
async fn test_bulk_action_receives_multiple_ids() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, headers, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/article/actions/publish")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ids":[1,2,3]}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Handler message says "Published 3 item(s)."
    assert!(trigger.contains("3"), "bulk count in toast: {trigger}");
}

#[tokio::test]
async fn test_unknown_action_returns_404() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;
    let (status, _headers, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/article/actions/nonexistent")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ids":[1]}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_selected_action_deletes_row() {
    let _g = LOCK.lock().await;
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    // Insert a throwaway row.
    let pool = umbral::db::pool();
    sqlx::query("INSERT INTO article (title, published) VALUES ('ToDelete', 0)")
        .execute(&pool)
        .await
        .expect("insert");
    let id: i64 = sqlx::query_scalar("SELECT id FROM article WHERE title = 'ToDelete'")
        .fetch_one(&pool)
        .await
        .expect("get id");

    let (status, headers, _body) = send(
        router,
        Request::builder()
            .method("POST")
            .uri("/admin/article/actions/delete_selected")
            .header(header::COOKIE, format!("umbral_session={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(r#"{{"ids":[{id}]}}"#)))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let trigger = headers
        .get("hx-trigger")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(trigger.contains("showToast"), "toast on delete: {trigger}");
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM article WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(remaining, 0, "row was deleted");
}
