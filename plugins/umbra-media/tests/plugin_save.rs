//! `MediaPlugin::save` end-to-end: persists the bytes through the
//! storage backend AND inserts a `media_file` row, returning the correct
//! public URL. Booted once against a real (tempfile) SQLite pool.

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbra::storage::Storage;
use umbra_media::{FsStorage, MediaFile, MediaPlugin, MediaTracking};

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

        // Register the ambient storage as a `MediaTracking` wrapping an
        // `FsStorage` (matching what `MediaPlugin::on_ready` does), so the
        // admin/form path test below can store through
        // `umbra::storage::storage()` and observe a tracking row. Boot
        // happens once, so this set_storage wins the OnceLock.
        let ambient_dir = tempfile::tempdir().expect("ambient media dir");
        let path = ambient_dir.path().to_path_buf();
        std::mem::forget(ambient_dir);
        let fs: Arc<dyn Storage> = Arc::new(FsStorage::new("/media", path));
        umbra::storage::set_storage(Arc::new(MediaTracking::new(fs)));
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

    let bytes = b"the quick brown fox";
    let outcome = plugin
        .save("report.txt", "text/plain", bytes)
        .await
        .expect("save should succeed");

    // Exactly one row exists for THIS upload's key. Counting by the
    // generated key (not the whole table) keeps the assertion race-free
    // against any other test sharing the process-global pool, while still
    // proving `save` inserts exactly one — never zero, never two.
    let rows_for_key = MediaFile::objects()
        .filter(umbra_media::media_file::KEY.eq(&outcome.file.key))
        .count()
        .await
        .expect("count by key");
    assert_eq!(
        rows_for_key, 1,
        "save must insert exactly one media_file row for the stored key"
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

/// An upload through the AMBIENT storage (the `MediaTracking` decorator
/// registered at boot, the path the admin/form upload takes via
/// `parse_and_store_multipart`) inserts exactly one `media_file` row with
/// the right metadata, and the bytes are retrievable.
#[tokio::test]
async fn ambient_upload_tracks_exactly_one_row() {
    boot().await;

    let ambient = umbra::storage::storage();

    let bytes = b"ambient upload bytes";
    let stored = ambient
        .store("photo.jpg", "image/jpeg", bytes)
        .await
        .expect("ambient store should succeed");

    // Exactly one tracking row exists for this upload's key — counting by
    // key (not the whole table) is race-free against the concurrent
    // `save_writes_bytes_and_inserts_row` test sharing the global pool.
    let rows_for_key = MediaFile::objects()
        .filter(umbra_media::media_file::KEY.eq(&stored.key))
        .count()
        .await
        .expect("count by key");
    assert_eq!(
        rows_for_key, 1,
        "an ambient upload must insert exactly one media_file row for its key"
    );

    // The tracking row carries the right metadata.
    let row = MediaFile::objects()
        .filter(umbra_media::media_file::KEY.eq(&stored.key))
        .first()
        .await
        .expect("query row")
        .expect("tracking row must exist");
    assert_eq!(row.filename, "photo.jpg");
    assert_eq!(row.content_type, "image/jpeg");
    assert_eq!(row.size, bytes.len() as i64);

    // The bytes round-trip through the ambient backend.
    let got = ambient
        .retrieve(&stored.key)
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
