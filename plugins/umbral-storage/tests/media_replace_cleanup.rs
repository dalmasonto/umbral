//! Replace-cleanup (gaps2 #92): updating a row's `FileField` key to a new
//! value deletes the OLD blob. Moved from umbral-media.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::orm::FileField;
use umbral::storage::{Storage, StorageError};
use umbral_storage::{FsStorage, StoragePlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "replace_doc")]
pub struct ReplaceDoc {
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
        let db_path = tmp.path().join("replace.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
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
            .model::<ReplaceDoc>()
            .plugin(
                StoragePlugin::new()
                    .media_with_storage("/media", fs)
                    .cleanup_on_delete::<ReplaceDoc>(),
            )
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE replace_doc (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                attachment TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create replace_doc");
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
async fn replacing_file_deletes_old_blob_keeps_new() {
    boot().await;

    let key_a = store_blob("a.pdf", b"old bytes A").await;
    let key_b = store_blob("b.pdf", b"new bytes B").await;
    assert!(blob_exists(&key_a).await);
    assert!(blob_exists(&key_b).await);

    let doc = ReplaceDoc::objects()
        .create(ReplaceDoc {
            id: 0,
            title: "doc".into(),
            attachment: FileField::from(key_a.clone()),
        })
        .await
        .expect("create row");

    let mut updated = doc.clone();
    updated.attachment = FileField::from(key_b.clone());
    ReplaceDoc::objects()
        .save(updated)
        .await
        .expect("save (update) row");

    assert!(
        !blob_exists(&key_a).await,
        "old blob A should be deleted on file replace"
    );
    assert!(
        blob_exists(&key_b).await,
        "new blob B should remain after replace"
    );
}

#[tokio::test]
async fn same_key_update_deletes_nothing() {
    boot().await;

    let key = store_blob("same.pdf", b"unchanged").await;

    let doc = ReplaceDoc::objects()
        .create(ReplaceDoc {
            id: 0,
            title: "before".into(),
            attachment: FileField::from(key.clone()),
        })
        .await
        .expect("create row");

    let mut updated = doc.clone();
    updated.title = "after".into();
    ReplaceDoc::objects()
        .save(updated)
        .await
        .expect("save (update) row");

    assert!(
        blob_exists(&key).await,
        "same-key update must NOT delete the blob"
    );
}

#[tokio::test]
async fn non_file_update_deletes_nothing() {
    boot().await;

    let key = store_blob("keep.pdf", b"keep").await;

    let doc = ReplaceDoc::objects()
        .create(ReplaceDoc {
            id: 0,
            title: "t1".into(),
            attachment: FileField::from(key.clone()),
        })
        .await
        .expect("create row");

    let mut updated = doc.clone();
    updated.title = "t2".into();
    ReplaceDoc::objects()
        .save(updated)
        .await
        .expect("save (update) row");

    assert!(blob_exists(&key).await, "non-file update keeps the blob");
}

#[tokio::test]
async fn insert_deletes_nothing() {
    boot().await;

    let key = store_blob("fresh.pdf", b"fresh").await;

    let _doc = ReplaceDoc::objects()
        .create(ReplaceDoc {
            id: 0,
            title: "new".into(),
            attachment: FileField::from(key.clone()),
        })
        .await
        .expect("create row");

    assert!(blob_exists(&key).await, "INSERT must not delete any blob");
}
