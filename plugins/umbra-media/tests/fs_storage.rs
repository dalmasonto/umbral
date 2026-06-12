//! Behavioral tests for `FsStorage` — real files in a tempdir, exercised
//! through the public `Storage` trait methods (no process-global involved,
//! so these are safe to run alongside everything else).

use umbra::storage::{Storage, StorageError};
use umbra_media::FsStorage;

/// store → retrieve round-trips the exact bytes; the returned url matches
/// `url(key)`.
#[tokio::test]
async fn store_then_retrieve_round_trips_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let bytes = b"hello, umbra storage";
    let stored = fs
        .store("note.txt", "text/plain", bytes)
        .await
        .expect("store should succeed");

    // The url the store reported matches url(key).
    assert_eq!(stored.url, fs.url(&stored.key));
    assert!(stored.url.starts_with("/media/"));
    assert!(stored.key.ends_with("-note.txt"));

    // Reading back returns the same bytes.
    let got = fs
        .retrieve(&stored.key)
        .await
        .expect("retrieve should find it");
    assert_eq!(got, bytes);

    // The bytes actually landed on disk under <dir>/<key>.
    let on_disk = std::fs::read(dir.path().join(&stored.key)).unwrap();
    assert_eq!(on_disk, bytes);
}

/// Two stores of the same filename get distinct keys — no clobber.
#[tokio::test]
async fn two_stores_of_same_filename_get_distinct_keys() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let a = fs.store("avatar.png", "image/png", b"AAAA").await.unwrap();
    let b = fs.store("avatar.png", "image/png", b"BBBB").await.unwrap();

    assert_ne!(a.key, b.key, "distinct uploads must get distinct keys");

    // Each retrieves its own bytes; neither clobbered the other.
    assert_eq!(fs.retrieve(&a.key).await.unwrap(), b"AAAA");
    assert_eq!(fs.retrieve(&b.key).await.unwrap(), b"BBBB");
}

/// delete removes the object; a subsequent retrieve reports NotFound.
#[tokio::test]
async fn delete_removes_the_object() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let stored = fs
        .store("doc.pdf", "application/pdf", b"%PDF")
        .await
        .unwrap();
    fs.delete(&stored.key).await.expect("delete should succeed");

    match fs.retrieve(&stored.key).await {
        Err(StorageError::NotFound) => {}
        other => panic!("expected NotFound after delete, got {other:?}"),
    }
    assert!(!dir.path().join(&stored.key).exists());
}

/// retrieve of a key that was never stored → NotFound.
#[tokio::test]
async fn retrieve_missing_key_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    match fs.retrieve("does-not-exist").await {
        Err(StorageError::NotFound) => {}
        other => panic!("expected NotFound for a missing key, got {other:?}"),
    }
}

/// A filename with path separators can't escape `dir` — the separators are
/// stripped, so the file lands directly under the storage dir.
#[tokio::test]
async fn path_traversal_filename_is_sanitised() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let stored = fs
        .store("../../etc/passwd", "text/plain", b"x")
        .await
        .unwrap();

    // The key has no separators, so the on-disk path stays inside `dir`.
    assert!(!stored.key.contains('/'));
    assert!(dir.path().join(&stored.key).exists());
    assert!(fs.retrieve(&stored.key).await.is_ok());
}
