//! `StoragePlugin::save` end-to-end: persists the bytes through the
//! storage backend AND inserts a `media_file` row. Moved from umbral-media.

use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::storage::Storage;
use umbral_storage::{FsStorage, MediaFile, MediaTracking, StoragePlugin};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
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

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE media_file (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                key TEXT NOT NULL,\
                filename TEXT NOT NULL,\
                content_type TEXT NOT NULL,\
                size INTEGER NOT NULL,\
                uploaded_at TEXT NOT NULL,\
                status TEXT NOT NULL DEFAULT 'ready'\
             )",
        )
        .execute(&pool)
        .await
        .expect("create media_file");

        let ambient_dir = tempfile::tempdir().expect("ambient media dir");
        let path = ambient_dir.path().to_path_buf();
        std::mem::forget(ambient_dir);
        let fs: Arc<dyn Storage> = Arc::new(FsStorage::new("/media", path));
        umbral::storage::set_storage(Arc::new(MediaTracking::new(fs)));
    })
    .await;
}

#[tokio::test]
async fn save_writes_bytes_and_inserts_row() {
    boot().await;
    let media_dir = tempfile::tempdir().expect("media dir");

    let fs = Arc::new(FsStorage::new("/media", media_dir.path()));
    let plugin = StoragePlugin::new().media_with_storage("/media", fs.clone());

    let bytes = b"the quick brown fox";
    let outcome = plugin
        .save("report.txt", "text/plain", bytes)
        .await
        .expect("save should succeed");

    let rows_for_key = MediaFile::objects()
        .filter(umbral_storage::media_file::KEY.eq(&outcome.file.key))
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
    assert!(outcome.file.id > 0, "saved row must have a real primary key");

    assert_eq!(outcome.url, fs.url(&outcome.file.key));
    assert!(outcome.url.starts_with("/media/"));

    let got = fs
        .retrieve(&outcome.file.key)
        .await
        .expect("stored bytes must be retrievable");
    assert_eq!(got, bytes);
}

#[tokio::test]
async fn ambient_upload_tracks_exactly_one_row() {
    boot().await;

    let ambient = umbral::storage::storage();

    let bytes = b"ambient upload bytes";
    let stored = ambient
        .store("photo.jpg", "image/jpeg", bytes)
        .await
        .expect("ambient store should succeed");

    let rows_for_key = MediaFile::objects()
        .filter(umbral_storage::media_file::KEY.eq(&stored.key))
        .count()
        .await
        .expect("count by key");
    assert_eq!(
        rows_for_key, 1,
        "an ambient upload must insert exactly one media_file row for its key"
    );

    let row = MediaFile::objects()
        .filter(umbral_storage::media_file::KEY.eq(&stored.key))
        .first()
        .await
        .expect("query row")
        .expect("tracking row must exist");
    assert_eq!(row.filename, "photo.jpg");
    assert_eq!(row.content_type, "image/jpeg");
    assert_eq!(row.size, bytes.len() as i64);

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

    let plugin = StoragePlugin::new().media("/media", media_dir.path()).max_size(4);

    let err = plugin
        .save("big.bin", "application/octet-stream", b"too many bytes")
        .await
        .expect_err("oversized upload should be rejected");

    assert!(
        matches!(err, umbral_storage::MediaError::TooLarge { .. }),
        "expected TooLarge, got {err:?}"
    );
}
