//! File-lifecycle cleanup (gaps2 #82): deleting a row that holds a
//! `FileField` key deletes the stored blob from the `Storage` backend.
//! Moved from umbral-media.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::orm::FileField;
use umbral::storage::{Storage, StorageError};
use umbral_storage::{FsStorage, StoragePlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "lifecycle_doc")]
pub struct LifecycleDoc {
    pub id: i64,
    pub title: String,
    pub attachment: FileField,
}

static AMBIENT_DIR: OnceCell<PathBuf> = OnceCell::const_new();
static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");

        let tmp = tempfile::tempdir().expect("db tempdir");
        let db_path = tmp.path().join("lifecycle.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().to_path_buf();
        std::mem::forget(media_dir);
        AMBIENT_DIR
            .set(media_path.clone())
            .expect("ambient dir set once");

        let fs: Arc<dyn Storage> = Arc::new(FsStorage::new("/media", media_path));

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<LifecycleDoc>()
            .plugin(
                StoragePlugin::new()
                    .media_with_storage("/media", fs)
                    .cleanup_on_delete::<LifecycleDoc>(),
            )
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

async fn store_blob(filename: &str, bytes: &[u8]) -> String {
    let storage = umbral::storage::storage();
    storage
        .store(filename, "application/octet-stream", bytes)
        .await
        .expect("store blob")
        .key
}

async fn blob_exists(key: &str) -> bool {
    match umbral::storage::storage().retrieve(key).await {
        Ok(_) => true,
        Err(StorageError::NotFound) => false,
        Err(e) => panic!("unexpected retrieve error for {key}: {e}"),
    }
}

#[tokio::test]
async fn deleting_row_deletes_its_blob() {
    boot().await;

    let key = store_blob("report.pdf", b"%PDF lifecycle bytes").await;
    assert!(blob_exists(&key).await, "blob should exist after store");

    let doc = LifecycleDoc::objects()
        .create(LifecycleDoc {
            id: 0,
            title: "Q3 report".into(),
            attachment: FileField::from(key.clone()),
        })
        .await
        .expect("create row");

    LifecycleDoc::objects()
        .delete_instance(&doc)
        .await
        .expect("delete row");

    assert!(
        !blob_exists(&key).await,
        "blob should be gone after the owning row was deleted"
    );
}

#[tokio::test]
async fn deleting_row_with_empty_file_is_fine() {
    boot().await;

    let doc = LifecycleDoc::objects()
        .create(LifecycleDoc {
            id: 0,
            title: "no attachment".into(),
            attachment: FileField::default(),
        })
        .await
        .expect("create row");

    let affected = LifecycleDoc::objects()
        .delete_instance(&doc)
        .await
        .expect("delete row with empty file must not error");
    assert_eq!(affected, 1, "the row should still have been deleted");
}

#[tokio::test]
async fn deleting_row_with_missing_blob_does_not_fail() {
    boot().await;

    let key = store_blob("gone.bin", b"transient").await;
    umbral::storage::storage()
        .delete(&key)
        .await
        .expect("pre-delete the blob");
    assert!(!blob_exists(&key).await);

    let doc = LifecycleDoc::objects()
        .create(LifecycleDoc {
            id: 0,
            title: "stale ref".into(),
            attachment: FileField::from(key.clone()),
        })
        .await
        .expect("create row");

    let affected = LifecycleDoc::objects()
        .delete_instance(&doc)
        .await
        .expect("delete row with missing blob must not error");
    assert_eq!(affected, 1, "row delete should still report 1 row");
}

#[tokio::test]
async fn cleanup_is_scoped_to_the_deleted_rows_keys() {
    boot().await;

    let keep = store_blob("keep.dat", b"keep me").await;
    let drop = store_blob("drop.dat", b"drop me").await;

    let kept_doc = LifecycleDoc::objects()
        .create(LifecycleDoc {
            id: 0,
            title: "keeper".into(),
            attachment: FileField::from(keep.clone()),
        })
        .await
        .expect("create keeper");
    let _ = &kept_doc;

    let dropped_doc = LifecycleDoc::objects()
        .create(LifecycleDoc {
            id: 0,
            title: "doomed".into(),
            attachment: FileField::from(drop.clone()),
        })
        .await
        .expect("create doomed");

    LifecycleDoc::objects()
        .delete_instance(&dropped_doc)
        .await
        .expect("delete doomed");

    assert!(!blob_exists(&drop).await, "deleted row's blob is gone");
    assert!(
        blob_exists(&keep).await,
        "unrelated row's blob must be untouched"
    );
}
