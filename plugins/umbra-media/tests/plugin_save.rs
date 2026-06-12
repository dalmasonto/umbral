//! `MediaPlugin::save` end-to-end: persists the bytes through the
//! storage backend AND inserts a `media_file` row, returning the correct
//! public URL. Booted once against a real (tempfile) SQLite pool.

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbra::storage::Storage;
use umbra_media::{FsStorage, MediaFile, MediaPlugin};

/// Boot a one-process App with a tempfile SQLite DB and create the
/// `media_file` table. `App::build` sets the process-global pool
/// OnceLock, so boot must happen exactly once per binary — guarded by a
/// `tokio::sync::OnceCell`, the same pattern the umbra-sessions
/// integration test uses.
static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("media.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .build()
            .expect("App::build");

        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE media_file (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                key TEXT NOT NULL,\
                filename TEXT NOT NULL,\
                content_type TEXT NOT NULL,\
                size INTEGER NOT NULL,\
                uploaded_at TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create media_file");
    })
    .await;
}

#[tokio::test]
async fn save_writes_bytes_and_inserts_row() {
    boot().await;
    let media_dir = tempfile::tempdir().expect("media dir");

    // Use a backend we hold a handle to so we can verify the bytes landed
    // through the same storage the plugin saved them with.
    let fs = Arc::new(FsStorage::new("/media", media_dir.path()));
    let plugin = MediaPlugin::with_storage("/media", fs.clone());

    let before = MediaFile::objects().count().await.expect("count before");

    let bytes = b"the quick brown fox";
    let outcome = plugin
        .save("report.txt", "text/plain", bytes)
        .await
        .expect("save should succeed");

    // A row was inserted with the right metadata.
    let after = MediaFile::objects().count().await.expect("count after");
    assert_eq!(
        after,
        before + 1,
        "save must insert exactly one media_file row"
    );
    assert_eq!(outcome.file.filename, "report.txt");
    assert_eq!(outcome.file.content_type, "text/plain");
    assert_eq!(outcome.file.size, bytes.len() as i64);
    assert!(
        outcome.file.id > 0,
        "saved row must have a real primary key"
    );

    // The url is the backend's public url for the stored key.
    assert_eq!(outcome.url, fs.url(&outcome.file.key));
    assert!(outcome.url.starts_with("/media/"));

    // The bytes are retrievable via the storage backend.
    let got = fs
        .retrieve(&outcome.file.key)
        .await
        .expect("stored bytes must be retrievable");
    assert_eq!(got, bytes);
}

#[tokio::test]
async fn save_enforces_max_size() {
    boot().await;
    let media_dir = tempfile::tempdir().expect("media dir");

    let plugin = MediaPlugin::new("/media", media_dir.path()).max_size(4);

    let err = plugin
        .save("big.bin", "application/octet-stream", b"too many bytes")
        .await
        .expect_err("oversized upload should be rejected");

    assert!(
        matches!(err, umbra_media::MediaError::TooLarge { .. }),
        "expected TooLarge, got {err:?}"
    );
}
