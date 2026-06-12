#![allow(dead_code, private_interfaces)]
//! Wave 4 — file/image upload widgets + multipart form handling in the
//! admin.
//!
//! Boots an admin router for a model carrying an `ImageField`, wires an
//! in-memory `Storage` backend (via a `provides_storage()` plugin so the
//! boot system check passes), and exercises the consumer wiring end to
//! end:
//!
//!   1. POST a `multipart/form-data` create with a file part → the row
//!      lands with the column = the stored key, and the bytes are
//!      retrievable through the ambient Storage.
//!   2. GET the change form → the rendered HTML carries
//!      `enctype="multipart/form-data"`, a `type="file"` input, and (for
//!      the image field) an `<img>` preview whose `src` is the resolved
//!      URL.
//!   3. POST an update WITHOUT a new file (empty file part) → the
//!      existing key is preserved, never nulled.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbra::orm::ImageField;
use umbra::plugin::Plugin;
use umbra::storage::{Storage, StorageError, StoredFile, set_storage};
use umbra_admin::{AdminModel, AdminPlugin};
use umbra_auth::{AuthPlugin, AuthUser, create_user};
use umbra_sessions::SessionsPlugin;

// =========================================================================
// Model + in-memory storage backend
// =========================================================================

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Product {
    id: i64,
    name: String,
    cover: ImageField,
}

/// In-memory `Storage`: `store` keys files by filename and keeps the
/// bytes so the test can read them back; `url` returns a stable public
/// URL the template can render.
#[derive(Debug, Default)]
struct MemStorage {
    files: Mutex<HashMap<String, Vec<u8>>>,
}

#[umbra::storage::async_trait]
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
        Ok(StoredFile { key, url })
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

/// Shared handle so the test body can `retrieve` what the upload stored.
static BACKEND: OnceCell<Arc<MemStorage>> = OnceCell::const_new();

/// Reports `provides_storage()` so the boot system check passes for the
/// `cover: ImageField` column, and registers the in-memory backend in
/// `on_ready` (the production posture — backends register there).
struct MemMediaPlugin;

impl Plugin for MemMediaPlugin {
    fn name(&self) -> &'static str {
        "mem_media"
    }
    fn provides_storage(&self) -> bool {
        true
    }
    fn on_ready(&self, _ctx: &umbra::plugin::AppContext) -> Result<(), umbra::plugin::PluginError> {
        let backend = BACKEND
            .get()
            .cloned()
            .expect("BACKEND set before App::build");
        set_storage(backend);
        Ok(())
    }
}

// =========================================================================
// Boot
// =========================================================================

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let backend = Arc::new(MemStorage::default());
        BACKEND.set(backend).expect("set backend once");

        let settings = umbra::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("file_image_upload.sqlite");
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

        let product_config = AdminModel::new("product").list_display(&["name", "cover"]);

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(MemMediaPlugin)
            .plugin(AdminPlugin::default().register(product_config))
            .model::<Product>()
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
            "CREATE TABLE product (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL,\
                cover TEXT NOT NULL DEFAULT ''\
            )",
        )
        .execute(&pool)
        .await
        .expect("product");

        let staff = create_user("media_admin", "media@example.com", "pass123")
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

// =========================================================================
// Helpers
// =========================================================================

const BOUNDARY: &str = "X-UMBRA-ADMIN-UPLOAD";

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

/// Log in `media_admin` and return the session cookie value.
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
    let csrf_cookie = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|s| {
            let first = s.split(';').next()?;
            let (k, v) = first.split_once('=')?;
            if k.trim() == "umbra_csrf_token" {
                Some(v.to_string())
            } else {
                None
            }
        })
        .expect("login sets umbra_csrf_token cookie");
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let csrf = extract_csrf(&String::from_utf8_lossy(&bytes));
    let form = serde_urlencoded::to_string([
        ("username", "media_admin"),
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
                .header(header::COOKIE, format!("umbra_csrf_token={csrf_cookie}"))
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
            if k.trim() == "umbra_session" {
                Some(v.to_string())
            } else {
                None
            }
        })
        .expect("login sets umbra_session cookie")
}

// =========================================================================
// Tests
// =========================================================================

/// Multipart create stores the file via Storage and writes the returned
/// key to the column; the bytes are retrievable from the backend.
#[tokio::test]
async fn multipart_create_stores_file_and_writes_key() {
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let png = b"\x89PNG\r\n\x1a\nFAKEIMAGE";
    let body = multipart_body(&[
        ("name", None, None, b"Widget"),
        ("cover", Some("hero.png"), Some("image/png"), png),
    ]);

    let (status, _h, _b) = send(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri("/admin/product/new")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header(header::COOKIE, format!("umbra_session={session}"))
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    // Full-page create redirects to the changelist on success.
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "create should succeed, got {status}"
    );

    // The row exists with cover = the stored key.
    let pool = umbra::db::pool();
    let key: String = sqlx::query_scalar("SELECT cover FROM product WHERE name = 'Widget'")
        .fetch_one(&pool)
        .await
        .expect("row created");
    assert_eq!(key, "uploads/hero.png", "cover column holds the stored key");

    // And the bytes are retrievable through the ambient Storage.
    let backend = BACKEND.get().cloned().unwrap();
    let got = backend.retrieve(&key).await.expect("file stored");
    assert_eq!(got, png, "stored bytes match the upload");
}

/// The change form renders multipart enctype, a file input, and an image
/// preview whose src is the resolved URL.
#[tokio::test]
async fn change_form_renders_multipart_and_image_preview() {
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    // Seed a row directly with a known key.
    let pool = umbra::db::pool();
    sqlx::query("INSERT INTO product (name, cover) VALUES ('Seeded', 'uploads/seed.png')")
        .execute(&pool)
        .await
        .expect("seed");
    let id: i64 = sqlx::query_scalar("SELECT id FROM product WHERE name = 'Seeded'")
        .fetch_one(&pool)
        .await
        .expect("id");

    let (status, _h, html) = send(
        router.clone(),
        Request::builder()
            .method("GET")
            .uri(format!("/admin/product/{id}/edit"))
            .header(header::COOKIE, format!("umbra_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "edit form loads");

    assert!(
        html.contains(r#"enctype="multipart/form-data""#),
        "form carries multipart enctype when a file field is present"
    );
    assert!(
        html.contains(r#"type="file""#) && html.contains(r#"name="cover""#),
        "file input rendered for the cover field"
    );
    // Image preview <img> with the resolved URL (not the raw key).
    assert!(
        html.contains("<img "),
        "an <img> thumbnail is rendered for the current image value"
    );
    // The preview src is the storage-resolved URL (not the raw key).
    // minijinja entity-escapes the `/` in attribute values, so compare
    // against the un-escaped HTML to keep the assertion about the URL,
    // not the escaping scheme.
    let unescaped = html.replace("&#x2f;", "/").replace("&#47;", "/");
    assert!(
        unescaped.contains("src=\"https://cdn.test/uploads/seed.png\""),
        "image preview src is the resolved storage URL"
    );
    assert!(
        !html.contains("src=\"uploads/seed.png\"")
            && !unescaped.contains("src=\"uploads/seed.png\""),
        "preview must use the resolved URL, never the raw key"
    );
}

/// An update with an EMPTY file part leaves the existing key untouched.
#[tokio::test]
async fn update_without_new_file_preserves_existing_key() {
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let pool = umbra::db::pool();
    sqlx::query("INSERT INTO product (name, cover) VALUES ('Keepme', 'uploads/keep.png')")
        .execute(&pool)
        .await
        .expect("seed");
    let id: i64 = sqlx::query_scalar("SELECT id FROM product WHERE name = 'Keepme'")
        .fetch_one(&pool)
        .await
        .expect("id");

    // Submit the edit form changing only the name, with an EMPTY file
    // part for cover (what a browser sends when no new file is chosen).
    let body = multipart_body(&[
        ("name", None, None, b"Keepme Renamed"),
        ("cover", Some(""), Some("application/octet-stream"), b""),
    ]);
    let (status, _h, _b) = send(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(format!("/admin/product/{id}/edit"))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header(header::COOKIE, format!("umbra_session={session}"))
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "update should succeed, got {status}"
    );

    let (name, cover): (String, String) =
        sqlx::query_as("SELECT name, cover FROM product WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("row");
    assert_eq!(name, "Keepme Renamed", "name updated");
    assert_eq!(
        cover, "uploads/keep.png",
        "existing cover key preserved when no new file uploaded"
    );
}

/// The CHANGELIST renders an image column as an `<img>` thumbnail whose
/// `src` is the resolved storage URL — not the raw key printed as text.
#[tokio::test]
async fn changelist_renders_image_thumbnail_not_raw_key() {
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    // Seed a row with a known image key.
    let pool = umbra::db::pool();
    sqlx::query("INSERT INTO product (name, cover) VALUES ('Listed', 'uploads/list.png')")
        .execute(&pool)
        .await
        .expect("seed");

    let (status, _h, html) = send(
        router.clone(),
        Request::builder()
            .method("GET")
            .uri("/admin/product/")
            .header(header::COOKIE, format!("umbra_session={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "changelist loads");

    // minijinja entity-escapes `/` in attribute values; compare against
    // the un-escaped HTML so the assertion is about the URL not escaping.
    let unescaped = html.replace("&#x2f;", "/").replace("&#47;", "/");
    assert!(
        unescaped.contains("<img src=\"https://cdn.test/uploads/list.png\""),
        "changelist renders an <img> thumbnail with the resolved URL"
    );
    // The raw key must NOT appear as bare cell text (it's only inside the
    // resolved src/href URL).
    assert!(
        !unescaped.contains(">uploads/list.png<"),
        "raw storage key must not be printed as cell text"
    );
}

/// The PREVIEW sheet renders an image column as an `<img>` whose `src`
/// is the resolved storage URL.
#[tokio::test]
async fn preview_sheet_renders_image_not_raw_key() {
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let pool = umbra::db::pool();
    sqlx::query("INSERT INTO product (name, cover) VALUES ('Previewed', 'uploads/prev.png')")
        .execute(&pool)
        .await
        .expect("seed");
    let id: i64 = sqlx::query_scalar("SELECT id FROM product WHERE name = 'Previewed'")
        .fetch_one(&pool)
        .await
        .expect("id");

    let (status, _h, html) = send(
        router.clone(),
        Request::builder()
            .method("GET")
            .uri(format!("/admin/product/{id}/sheet"))
            .header(header::COOKIE, format!("umbra_session={session}"))
            // The preview sheet only renders the fragment for HTMX
            // requests; otherwise it redirects to the changelist.
            .header("HX-Request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "preview sheet loads");

    let unescaped = html.replace("&#x2f;", "/").replace("&#47;", "/");
    assert!(
        unescaped.contains("<img src=\"https://cdn.test/uploads/prev.png\""),
        "preview sheet renders an <img> with the resolved URL, not the raw key"
    );
    assert!(
        !unescaped.contains("src=\"uploads/prev.png\""),
        "preview must use the resolved URL, never the raw key"
    );
}
