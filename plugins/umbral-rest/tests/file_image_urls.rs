//! REST file/image fields resolve to their public Storage URL, not the
//! raw storage key.
//!
//! A `FileField`/`ImageField` stores an opaque storage KEY in a TEXT
//! column. The REST response should surface the resolved public URL
//! (`storage().url(key)`), so a consumer can fetch the asset directly.
//! This boots an App with a model carrying an `ImageField` (+ a nullable
//! `FileField`), wires an in-memory `Storage` whose `url(key)` returns
//! `/media/<key>`, and asserts:
//!
//!   1. a non-empty image key comes back as `/media/<key>` (list + detail);
//!   2. a null nullable file field stays `null`;
//!   3. an empty-string key is NOT turned into a bare `/media/`.
//!
//! Dedicated test binary so the storage / settings OnceLocks aren't
//! shared with the other rest test binaries.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral::orm::{FileField, ImageField};
use umbral::plugin::Plugin;
use umbral::storage::{Storage, StorageError, StoredFile, set_storage};
use umbral_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Brand {
    id: i64,
    name: String,
    // Non-null image: a stored key resolves to a URL.
    logo: ImageField,
    // Nullable file: `None` stays null, a key resolves to a URL.
    spec_sheet: Option<FileField>,
}

/// In-memory storage whose `url(key)` returns `/media/<key>` — the exact
/// public-URL shape the fix is meant to surface.
#[derive(Debug, Default)]
struct MediaUrlStorage;

#[umbral::storage::async_trait]
impl Storage for MediaUrlStorage {
    async fn store(
        &self,
        filename: &str,
        _content_type: &str,
        _bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        let key = filename.to_string();
        let url = format!("/media/{key}");
        Ok(StoredFile {
            key,
            url,
            size: _bytes.len() as u64,
        })
    }
    async fn retrieve(&self, _key: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::NotFound)
    }
    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Ok(())
    }
    fn url(&self, key: &str) -> String {
        format!("/media/{key}")
    }
}

/// Reports `provides_storage()` so the boot system check passes for the
/// file/image columns, and registers the backend in `on_ready` (the
/// production posture — backends register there).
struct MediaPlugin;

impl Plugin for MediaPlugin {
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
        set_storage(Arc::new(MediaUrlStorage));
        Ok(())
    }
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("rest_file_urls.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Brand>()
            .plugin(MediaPlugin)
            .plugin(RestPlugin::default())
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE brand (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL,\
                logo TEXT NOT NULL DEFAULT '',\
                spec_sheet TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create brand table");

        // Row 1: image key + file key, both non-empty.
        // Row 2: null spec_sheet (nullable) + empty-string logo.
        sqlx::query(
            "INSERT INTO brand (name, logo, spec_sheet) VALUES \
             ('apple', 'c457-apple-touch-icon.png', 'datasheet.pdf'), \
             ('blank', '', NULL)",
        )
        .execute(&pool)
        .await
        .expect("seed");

        app.into_router()
    })
    .await
}

async fn get_json(router: axum::Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).expect("valid json");
    (status, parsed)
}

#[tokio::test]
async fn detail_resolves_image_and_file_keys_to_urls() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/brand/1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["logo"],
        Value::String("/media/c457-apple-touch-icon.png".into()),
        "image field resolves to the public URL, not the raw key: {body}"
    );
    assert_eq!(
        body["spec_sheet"],
        Value::String("/media/datasheet.pdf".into()),
        "nullable file field with a key resolves to the public URL: {body}"
    );
    assert_eq!(
        body["name"],
        Value::String("apple".into()),
        "name untouched"
    );
}

#[tokio::test]
async fn list_resolves_image_and_file_keys_to_urls() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/brand/").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let results = body["results"].as_array().expect("results array");
    let apple = results
        .iter()
        .find(|r| r["name"] == "apple")
        .expect("apple row");
    assert_eq!(
        apple["logo"],
        Value::String("/media/c457-apple-touch-icon.png".into()),
        "list image field resolves to URL: {apple}"
    );
}

#[tokio::test]
async fn null_file_field_stays_null_empty_key_stays_empty() {
    let router = boot().await.clone();
    let (status, body) = get_json(router, "/api/brand/2").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    // Nullable file field with no upload stays null — never `/media/`.
    assert_eq!(
        body["spec_sheet"],
        Value::Null,
        "null nullable file field stays null: {body}"
    );
    // An empty-string key is NOT turned into a bare `/media/`.
    assert_eq!(
        body["logo"],
        Value::String(String::new()),
        "empty-string key stays empty, never a bare /media/: {body}"
    );
}
