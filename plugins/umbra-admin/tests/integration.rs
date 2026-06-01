//! End-to-end coverage for umbra-admin. Boot the App once with
//! AuthPlugin + AdminPlugin registered, seed a staff user via the
//! umbra-auth helpers, then drive every admin route through axum's
//! `ServiceExt::oneshot` without a TCP listener.
//!
//! Covers the full Django-shape flow:
//!
//! - GET /admin without auth → 401 + WWW-Authenticate prompt
//! - GET /admin with a non-staff user → 403
//! - GET /admin as staff → 200 with the registered-models index
//! - POST /admin/<table>/new (create) → 303 → row appears
//! - GET /admin/<table>/ (list) → 200 with the new row visible
//! - GET /admin/<table>/<id> (detail) → 200 with field values
//! - POST /admin/<table>/<id>/edit (update) → 303 → row reflects edit
//! - POST /admin/<table>/<id>/delete (delete) → 303 → row gone

// The local `Note` model is private but `#[derive(Model)]` emits a
// `pub const` column module that references it. Same lint dodge the
// migrate / type_catalogue / backup tests use.
#![allow(dead_code, private_interfaces)]

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbra_admin::AdminPlugin;
use umbra_auth::{AuthPlugin, AuthUser, create_user};

/// A second model in addition to AuthUser so the admin's
/// list-models index shows >1 entry.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Note {
    id: i64,
    title: String,
    body: String,
    published_at: Option<DateTime<Utc>>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");

        // Tempfile-backed sqlite so every pool connection sees the
        // same database (the same pattern umbra-auth's integration
        // tests use).
        let tmp = tempfile::tempdir().expect("tempdir for the test DB");
        let path = tmp.path().join("admin_integration.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(AdminPlugin::default())
            .model::<Note>()
            .build()
            .expect("App::build with AuthPlugin + AdminPlugin");

        // Schema: every registered model + the umbra-auth user.
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
        .expect("create auth_user");
        sqlx::query(
            "CREATE TABLE note (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL,\
                published_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create note");

        // Seed two users: one staff, one not.
        let staff = create_user("alice", "alice@example.com", "hunter2")
            .await
            .expect("create staff user");
        sqlx::query("UPDATE auth_user SET is_staff = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("mark alice as staff");
        let _: AuthUser = create_user("bob", "bob@example.com", "secret")
            .await
            .expect("create regular user");

        app.into_router()
    })
    .await
}

fn basic_auth(username: &str, password: &str) -> String {
    let creds = format!("{username}:{password}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(creds);
    format!("Basic {encoded}")
}

async fn send(router: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes).into_owned();
    (status, body)
}

#[tokio::test]
async fn admin_index_without_auth_returns_401() {
    let router = boot().await.clone();
    let (status, _) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_with_non_staff_user_returns_403() {
    let router = boot().await.clone();
    let (status, _) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .header(header::AUTHORIZATION, basic_auth("bob", "secret"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_with_wrong_password_returns_401() {
    let router = boot().await.clone();
    let (status, _) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .header(header::AUTHORIZATION, basic_auth("alice", "wrong"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_index_as_staff_lists_registered_models() {
    let router = boot().await.clone();
    let (status, body) = send(
        router,
        Request::builder()
            .uri("/admin/")
            .header(header::AUTHORIZATION, basic_auth("alice", "hunter2"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Registered models"), "got body:\n{body}");
    // Both registered models should show up.
    assert!(body.contains("auth_user"), "auth_user missing:\n{body}");
    assert!(body.contains("note"), "note missing:\n{body}");
}

#[tokio::test]
async fn full_crud_flow_against_note_model() {
    let router = boot().await.clone();
    let auth = basic_auth("alice", "hunter2");

    // 1. Create via POST /admin/note/new
    let create_body = serde_urlencoded::to_string([
        ("title", "first note"),
        ("body", "hello from the admin"),
        ("published_at", ""),
    ])
    .unwrap();
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/note/new")
                .header(header::AUTHORIZATION, &auth)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(create_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "POST new should 303");
    let location = resp
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(location, "/admin/note/");

    // 2. The list view shows the new row.
    let (status, body) = send(
        router.clone(),
        Request::builder()
            .uri("/admin/note/")
            .header(header::AUTHORIZATION, &auth)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("first note"),
        "list missing seeded note:\n{body}"
    );

    // 3. Detail view by id (we just created the only note, so id=1).
    let (status, body) = send(
        router.clone(),
        Request::builder()
            .uri("/admin/note/1")
            .header(header::AUTHORIZATION, &auth)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("hello from the admin"),
        "detail body:\n{body}"
    );

    // 4. Edit via POST /admin/note/1/edit
    let edit_body = serde_urlencoded::to_string([
        ("title", "edited note"),
        ("body", "after edit"),
        ("published_at", ""),
    ])
    .unwrap();
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/note/1/edit")
                .header(header::AUTHORIZATION, &auth)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(edit_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "POST edit should 303");

    // Detail after edit reflects the change.
    let (_, body) = send(
        router.clone(),
        Request::builder()
            .uri("/admin/note/1")
            .header(header::AUTHORIZATION, &auth)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(body.contains("edited note"), "edit didn't take:\n{body}");
    assert!(
        body.contains("after edit"),
        "edit body didn't take:\n{body}"
    );

    // 5. Delete via POST /admin/note/1/delete
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/note/1/delete")
                .header(header::AUTHORIZATION, &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "POST delete should 303"
    );

    // The note is gone.
    let (status, _) = send(
        router.clone(),
        Request::builder()
            .uri("/admin/note/1")
            .header(header::AUTHORIZATION, &auth)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "post-delete detail should 404"
    );
}

// Quiet a probable unused-import warning for `PathBuf` if Rust ever
// reshuffles which test references it.
#[allow(dead_code)]
fn _unused_pathbuf_marker() -> Option<PathBuf> {
    None
}
