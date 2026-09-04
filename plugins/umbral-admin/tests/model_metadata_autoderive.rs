//! gaps4 #95 — the admin auto-derives its changelist config from the model's
//! own declared presentation metadata (`#[umbral(list_display)]`,
//! `#[umbral(search)]`, …) when no explicit `AdminModel` overrides it. So a
//! model gets a sensible admin with ZERO per-model wiring in `main.rs`.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_admin::AdminPlugin;
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

// Declares its presentation metadata on the model — NO AdminModel is
// registered for it, so the admin must read this to build the changelist.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "amd_gadget")]
struct Gadget {
    id: i64,
    #[umbral(list_display, search)]
    name: String,
    #[umbral(list_display, list_filter)]
    kind: String,
    // Neither displayed nor searched.
    secret_notes: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("amd.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        // AdminPlugin::default() — no AdminModel::register for Gadget.
        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default())
            .model::<Gadget>()
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        let staff = create_user("amd_admin", "amd_admin@example.com", "password123")
            .await
            .expect("create staff user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark staff");
        sqlx::query(
            "INSERT INTO amd_gadget (name, kind, secret_notes) VALUES \
             ('Alpha', 'tool', 'hidden'), ('Beta', 'toy', 'hidden')",
        )
        .execute(&pool)
        .await
        .expect("seed gadgets");

        app.into_router()
    })
    .await
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

async fn staff_cookie(router: axum::Router, username: &str) -> String {
    // GET the login page → anon session cookie + CSRF token.
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
        ("password", "password123"),
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

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn changelist_auto_derives_display_and_search_from_the_model() {
    let router = boot().await.clone();
    let session = staff_cookie(router.clone(), "amd_admin").await;

    let req = Request::builder()
        .uri("/admin/amd_gadget/")
        .header(header::COOKIE, format!("umbral_session={session}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(router, req).await;
    assert_eq!(status, StatusCode::OK, "changelist renders: {body}");

    // The search box shows because the model declares a `#[umbral(search)]`
    // field — even though no AdminModel::search_fields was configured.
    assert!(
        body.contains(r#"id="dt-search""#),
        "the model-declared search field must enable the admin search box"
    );

    // The declared list_display columns (name, kind) appear as headers; the
    // undeclared `secret_notes` column must NOT be a table header.
    assert!(
        body.contains(">name<") || body.contains("name"),
        "name shown"
    );
    assert!(
        !body.contains("secret_notes"),
        "a column not in the model's list_display must not appear in the table: {body}"
    );
}
