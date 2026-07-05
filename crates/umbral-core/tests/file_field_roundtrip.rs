//! Wave 2 — a `FileField` column round-trips through a real SQLite TEXT
//! column: write a model with a `FileField`, read it back, the key
//! matches.
//!
//! Boots one App (settings OnceLock is per-process) with a storage-
//! providing test plugin so the `field.storage_backend` boot check
//! passes, then exercises the typed `create` / `get` path.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use serde::{Deserialize, Serialize};
use umbral::migrate::ModelMeta;
use umbral::orm::{FileField, ImageField};
use umbral::plugin::Plugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "file_doc")]
pub struct FileDoc {
    pub id: i64,
    pub attachment: FileField,
    pub cover: ImageField,
    pub thumbnail: Option<FileField>,
}

/// A do-nothing plugin that reports `provides_storage() == true` so the
/// boot check is satisfied without pulling in umbral-storage (a separate
/// crate umbral-core can't depend on).
struct FakeStoragePlugin;

impl Plugin for FakeStoragePlugin {
    fn name(&self) -> &'static str {
        "fake_storage"
    }
    fn provides_storage(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn file_field_round_trips_through_sqlite_text_column() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("file_roundtrip.sqlite");
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

    let _app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<FileDoc>()
        .plugin(FakeStoragePlugin)
        .build()
        .expect("App::build should succeed with a storage-providing plugin");

    // Sanity: the model registered and carries the expected meta.
    let _ = ModelMeta::for_::<FileDoc>();

    let pool = umbral::db::pool();
    sqlx::query(
        "CREATE TABLE file_doc (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            attachment TEXT NOT NULL,\
            cover TEXT NOT NULL,\
            thumbnail TEXT\
         )",
    )
    .execute(&pool)
    .await
    .expect("create file_doc table");

    let row = FileDoc {
        id: 0,
        attachment: FileField::from("ab12-report.pdf"),
        cover: ImageField::from("cd34-hero.png"),
        thumbnail: Some(FileField::from("ef56-thumb.png")),
    };
    let saved = FileDoc::objects().create(row).await.expect("create");

    // Read it back through the ORM and confirm the keys survived the
    // TEXT round-trip.
    let fetched = FileDoc::objects()
        .filter(file_doc::ID.eq(saved.id))
        .get()
        .await
        .expect("get");

    assert_eq!(fetched.attachment.key(), "ab12-report.pdf");
    assert_eq!(fetched.cover.key(), "cd34-hero.png");
    assert_eq!(
        fetched.thumbnail.as_ref().map(|t| t.key().to_string()),
        Some("ef56-thumb.png".to_string()),
    );

    // A NULL thumbnail round-trips to None.
    let row2 = FileDoc {
        id: 0,
        attachment: FileField::from("g.bin"),
        cover: ImageField::from("h.png"),
        thumbnail: None,
    };
    let saved2 = FileDoc::objects().create(row2).await.expect("create2");
    let fetched2 = FileDoc::objects()
        .filter(file_doc::ID.eq(saved2.id))
        .get()
        .await
        .expect("get2");
    assert!(fetched2.thumbnail.is_none(), "NULL thumbnail -> None");
}
