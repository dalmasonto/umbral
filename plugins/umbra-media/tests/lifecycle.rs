//! File-lifecycle cleanup (gaps2 #82): deleting a row that holds a
//! `FileField` / `ImageField` key deletes the stored blob from the
//! `Storage` backend, so the backend doesn't accumulate orphaned files
//! (Django's `FileField` cleanup).
//!
//! Boots a one-process App against a tempfile SQLite DB with a
//! `MediaPlugin` opted into cleanup for the test model, so the
//! `pre_delete:<table>` handler is wired exactly as production wires it.
//! All assertions go through the public `Storage` trait + the ORM
//! (`delete_instance`) — no raw blob-path pokes beyond reading back to
//! prove absence.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbra::orm::FileField;
use umbra::storage::{Storage, StorageError};
use umbra_media::{FsStorage, MediaPlugin};

/// A model with a single `FileField` (`attachment`) holding a storage key.
/// Deleting a row must cascade to deleting `attachment`'s blob.
///
/// `pub` (with `pub` fields) because `#[derive(Model)]` emits `pub`
/// per-field column consts (`lifecycle_doc::ID`, …); a private struct
/// behind them trips the `private_interfaces` lint.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "lifecycle_doc")]
pub struct LifecycleDoc {
    pub id: i64,
    pub title: String,
    pub attachment: FileField,
}

/// Holds the one backend the App registers as ambient (so the test can
/// `store` / `retrieve` through the same `Storage` the cleanup handler
/// deletes through). Set in `boot`, read by every test.
static AMBIENT_DIR: OnceCell<PathBuf> = OnceCell::const_new();
static BOOT: OnceCell<()> = OnceCell::const_new();

/// Boot once: tempfile SQLite pool + the `lifecycle_doc` table + a
/// `MediaPlugin` (backed by an `FsStorage` over a tempdir) opted into
/// cleanup for `LifecycleDoc`. `App::build` runs `on_ready`, which both
/// registers the ambient storage AND subscribes the `pre_delete`
/// cleanup handler — the exact production wiring.
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults load");

        let tmp = tempfile::tempdir().expect("db tempdir");
        let db_path = tmp.path().join("lifecycle.sqlite");
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

        // The media dir the cleanup handler will delete blobs from.
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().to_path_buf();
        std::mem::forget(media_dir);
        AMBIENT_DIR
            .set(media_path.clone())
            .expect("ambient dir set once");

        let fs: Arc<dyn Storage> = Arc::new(FsStorage::new("/media", media_path));

        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<LifecycleDoc>()
            .plugin(MediaPlugin::with_storage("/media", fs).cleanup_on_delete::<LifecycleDoc>())
            .build()
            .expect("App::build");

        // Create the table the ORM will write/delete through. Raw DDL is
        // the documented test-only exception (CLAUDE.md) — `build()`
        // doesn't auto-migrate.
        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE lifecycle_doc (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                attachment TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create lifecycle_doc");
    })
    .await;
}

/// Store bytes through the ambient backend, returning the key.
async fn store_blob(filename: &str, bytes: &[u8]) -> String {
    let storage = umbra::storage::storage();
    storage
        .store(filename, "application/octet-stream", bytes)
        .await
        .expect("store blob")
        .key
}

/// True when the ambient backend still holds `key`.
async fn blob_exists(key: &str) -> bool {
    match umbra::storage::storage().retrieve(key).await {
        Ok(_) => true,
        Err(StorageError::NotFound) => false,
        Err(e) => panic!("unexpected retrieve error for {key}: {e}"),
    }
}

/// Deleting a row deletes its file blob.
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

    // Per-row delete fires `pre_delete`, which the cleanup handler hooks.
    LifecycleDoc::objects()
        .delete_instance(&doc)
        .await
        .expect("delete row");

    assert!(
        !blob_exists(&key).await,
        "blob should be gone after the owning row was deleted"
    );
}

/// A row with an EMPTY file field deletes cleanly — no blob to remove,
/// no error.
#[tokio::test]
async fn deleting_row_with_empty_file_is_fine() {
    boot().await;

    let doc = LifecycleDoc::objects()
        .create(LifecycleDoc {
            id: 0,
            title: "no attachment".into(),
            attachment: FileField::default(), // empty key
        })
        .await
        .expect("create row");

    let affected = LifecycleDoc::objects()
        .delete_instance(&doc)
        .await
        .expect("delete row with empty file must not error");
    assert_eq!(affected, 1, "the row should still have been deleted");
}

/// Best-effort: deleting a row whose blob is already gone does not panic
/// and the row delete still succeeds.
#[tokio::test]
async fn deleting_row_with_missing_blob_does_not_fail() {
    boot().await;

    let key = store_blob("gone.bin", b"transient").await;
    // Remove the blob out from under the row, simulating an external
    // delete / a half-cleaned orphan.
    umbra::storage::storage()
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

    // The cleanup handler will try to delete an already-absent blob; that
    // must be swallowed as success, never fail the row delete.
    let affected = LifecycleDoc::objects()
        .delete_instance(&doc)
        .await
        .expect("delete row with missing blob must not error");
    assert_eq!(affected, 1, "row delete should still report 1 row");
}

/// Storing a second blob and deleting only its owning row leaves an
/// unrelated blob untouched — cleanup is scoped to the deleted row's keys.
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
