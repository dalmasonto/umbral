#![allow(dead_code, private_interfaces)]
//! gaps2 #36 — EasyMDE markdown-editor image upload endpoint.
//!
//! `POST /admin/upload-image` stores an image through the ambient storage
//! seam and returns `{ "url": ... }`. This suite boots an admin router and
//! exercises:
//!
//!   1. A staff POST of a small PNG → 200 + `{url}`, bytes retrievable.
//!   2. A non-staff / unauthenticated POST → redirect/403 (no upload).
//!   3. A non-image content-type → 415 rejected.
//!
//! The graceful no-backend case (409) lives in its own binary,
//! `image_upload_no_backend.rs`, because the ambient pool + storage globals
//! are set-once per process: a router built WITHOUT a storage backend can't
//! coexist with this one in the same test binary.
//!
//! The admin test harness does NOT mount SecurityPlugin, so CSRF is not
//! enforced here — the happy path is asserted directly; CSRF carriage is
//! wired in admin.js (the `imageUploadFunction` reads the cookie) and the
//! route inherits the app-wide CSRF middleware when SecurityPlugin IS
//! mounted in production.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral::plugin::Plugin;
use umbral::storage::{Storage, StorageError, StoredFile, set_storage};
use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

// =========================================================================
// Model + in-memory storage backend
// =========================================================================

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Note {
    id: i64,
    title: String,
}

/// In-memory `Storage` mirroring the one in `file_image_upload.rs`.
#[derive(Debug, Default)]
struct MemStorage {
    files: Mutex<HashMap<String, Vec<u8>>>,
}

#[umbral::storage::async_trait]
impl Storage for MemStorage {
    async fn store(
        &self,
        filename: &str,
        _content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        let key = format!("uploads/{filename}");
        self.files
            .lock()
            .unwrap()
            .insert(key.clone(), bytes.to_vec());
        let url = format!("https://cdn.test/{key}");
        Ok(StoredFile {
            key,
            url,
            size: bytes.len() as u64,
        })
    }
    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.files
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(StorageError::NotFound)
    }
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.files.lock().unwrap().remove(key);
        Ok(())
    }
    fn url(&self, key: &str) -> String {
        format!("https://cdn.test/{key}")
    }
}

static BACKEND: OnceCell<Arc<MemStorage>> = OnceCell::const_new();

/// Registers the in-memory backend in `on_ready`, like a real MediaPlugin.
struct MemMediaPlugin;

impl Plugin for MemMediaPlugin {
    fn name(&self) -> &'static str {
        "mem_media"
    }
    fn provides_storage(&self) -> bool {
        true
    }
    fn on_ready(
        &self,
        _ctx: &umbral::plugin::AppContext,
    ) -> Result<(), umbral::plugin::PluginError> {
        let backend = BACKEND
            .get()
            .cloned()
            .expect("BACKEND set before App::build");
        set_storage(backend);
        Ok(())
    }
}

// =========================================================================
// Boot — one router WITH a storage backend, shared across tests.
// =========================================================================

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let backend = Arc::new(MemStorage::default());
        BACKEND.set(backend).expect("set backend once");

        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("image_upload.sqlite");
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

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(MemMediaPlugin)
            .plugin(AdminPlugin::default().register(AdminModel::new("note")))
            .model::<Note>()
            .build()
            .expect("build");

        bootstrap_tables_and_user().await;
        app.into_router()
    })
    .await
}

async fn bootstrap_tables_and_user() {
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

    sqlx::query("CREATE TABLE note (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("note");

    // Staff user for the happy path.
    let staff = create_user("md_admin", "md@example.com", "pass123")
        .await
        .expect("user");
    sqlx::query("UPDATE auth_user SET is_staff = 1, is_superuser = 1 WHERE id = ?")
        .bind(staff.id)
        .execute(&pool)
        .await
        .expect("set staff");

    // A non-staff user for the 403 path.
    create_user("plain", "plain@example.com", "pass123")
        .await
        .expect("plain user");
}

// =========================================================================
// Helpers
// =========================================================================

const BOUNDARY: &str = "X-UMBRAL-EDITOR-UPLOAD";

/// `(name, filename, content_type, value)`; `None` filename = text field.
type PartSpec<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

fn multipart_body(parts: &[PartSpec<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, filename, content_type, value) in parts {
        out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match filename {
            Some(fname) => {
                out.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\n"
                    )
                    .as_bytes(),
                );
                if let Some(ct) = content_type {
                    out.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                }
            }
            None => {
                out.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
                );
            }
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
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

/// Log in `username`/`password` and return the session cookie value.
async fn login(router: axum::Router, username: &str, password: &str) -> String {
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
        .expect("login sets umbral_csrf_token cookie");
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let csrf = extract_csrf(&String::from_utf8_lossy(&bytes));
    let form = serde_urlencoded::to_string([
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
        .expect("login sets umbral_session cookie")
}

fn upload_request(session: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/admin/upload-image")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        );
    if let Some(s) = session {
        b = b.header(header::COOKIE, format!("umbral_session={s}"));
    }
    b.body(Body::from(body)).unwrap()
}

// =========================================================================
// Tests
// =========================================================================

/// A staff POST of a small PNG → 200 + JSON `{url}`, and the bytes are
/// retrievable through the storage backend.
#[tokio::test]
async fn staff_png_upload_stores_and_returns_url() {
    let router = boot().await.clone();
    let session = login(router.clone(), "md_admin", "pass123").await;

    let png = b"\x89PNG\r\n\x1a\nEDITORIMAGE";
    let body = multipart_body(&[("image", Some("paste.png"), Some("image/png"), png)]);

    let (status, _h, body) = send(router.clone(), upload_request(Some(&session), body)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "staff upload should be 200; body={body}"
    );

    let json: serde_json::Value = serde_json::from_str(&body).expect("JSON response");
    let url = json["url"].as_str().expect("response carries a url");
    assert_eq!(
        url, "https://cdn.test/uploads/paste.png",
        "url is the resolved storage URL"
    );

    // Bytes are retrievable via the storage backend.
    let backend = BACKEND.get().cloned().unwrap();
    let got = backend
        .retrieve("uploads/paste.png")
        .await
        .expect("file stored");
    assert_eq!(got, png, "stored bytes match the upload");
}

/// An unauthenticated POST → redirect to login (no upload performed).
#[tokio::test]
async fn unauthenticated_upload_is_rejected() {
    let router = boot().await.clone();
    let png = b"\x89PNG\r\n\x1a\nNOPE";
    let body = multipart_body(&[("image", Some("x.png"), Some("image/png"), png)]);

    let (status, _h, _b) = send(router.clone(), upload_request(None, body)).await;
    // require_staff redirects unauthenticated requests to the login page.
    assert!(
        status.is_redirection() || status == StatusCode::UNAUTHORIZED,
        "unauthenticated upload must not proceed, got {status}"
    );
    assert!(
        BACKEND
            .get()
            .cloned()
            .unwrap()
            .retrieve("uploads/x.png")
            .await
            .is_err(),
        "no file should have been stored"
    );
}

/// A logged-in NON-staff user → 403 (no upload performed).
///
/// The admin login flow refuses non-staff users, so we can't obtain a
/// session through it. Instead we forge a session directly for the
/// non-staff `plain` user (the production session API), which is exactly
/// the "logged in but not staff" state `require_staff` must 403.
#[tokio::test]
async fn non_staff_upload_is_forbidden() {
    let router = boot().await.clone();
    let pool = umbral::db::pool();
    let uid: i64 = sqlx::query_scalar("SELECT id FROM auth_user WHERE username = 'plain'")
        .fetch_one(&pool)
        .await
        .expect("plain user id");
    let session = umbral_sessions::create_session(Some(uid.to_string()), None)
        .await
        .expect("forge non-staff session");
    let png = b"\x89PNG\r\n\x1a\nNOPE2";
    let body = multipart_body(&[("image", Some("y.png"), Some("image/png"), png)]);

    let (status, _h, _b) = send(router.clone(), upload_request(Some(&session), body)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "non-staff upload must be 403"
    );
    assert!(
        BACKEND
            .get()
            .cloned()
            .unwrap()
            .retrieve("uploads/y.png")
            .await
            .is_err(),
        "no file should have been stored"
    );
}

/// A non-image content-type → 415 rejected (no upload performed).
#[tokio::test]
async fn non_image_content_type_is_rejected() {
    let router = boot().await.clone();
    let session = login(router.clone(), "md_admin", "pass123").await;
    let body = multipart_body(&[(
        "image",
        Some("evil.exe"),
        Some("application/octet-stream"),
        b"MZ\x90\x00",
    )]);

    let (status, _h, body) = send(router.clone(), upload_request(Some(&session), body)).await;
    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "non-image upload must be 415; body={body}"
    );
}
