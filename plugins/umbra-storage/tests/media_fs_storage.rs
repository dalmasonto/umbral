//! Behavioral tests for `FsStorage`. Moved from umbra-media.

use umbra::storage::{Storage, StorageError};
use umbra_storage::{FsStorage, StoragePlugin};

#[tokio::test]
async fn store_then_retrieve_round_trips_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let bytes = b"hello, umbra storage";
    let stored = fs
        .store("note.txt", "text/plain", bytes)
        .await
        .expect("store should succeed");

    assert_eq!(stored.url, fs.url(&stored.key));
    assert!(stored.url.starts_with("/media/"));
    assert!(stored.key.ends_with("-note.txt"));

    let got = fs
        .retrieve(&stored.key)
        .await
        .expect("retrieve should find it");
    assert_eq!(got, bytes);

    let on_disk = std::fs::read(dir.path().join(&stored.key)).unwrap();
    assert_eq!(on_disk, bytes);
}

#[tokio::test]
async fn two_stores_of_same_filename_get_distinct_keys() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let a = fs.store("avatar.png", "image/png", b"AAAA").await.unwrap();
    let b = fs.store("avatar.png", "image/png", b"BBBB").await.unwrap();

    assert_ne!(a.key, b.key, "distinct uploads must get distinct keys");

    assert_eq!(fs.retrieve(&a.key).await.unwrap(), b"AAAA");
    assert_eq!(fs.retrieve(&b.key).await.unwrap(), b"BBBB");
}

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

#[tokio::test]
async fn retrieve_missing_key_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    match fs.retrieve("does-not-exist").await {
        Err(StorageError::NotFound) => {}
        other => panic!("expected NotFound for a missing key, got {other:?}"),
    }
}

/// `put` writes at the EXACT key (the static-collection path) and `exists`
/// reports presence via a filesystem stat.
#[tokio::test]
async fn put_writes_at_exact_key_and_exists_reflects_it() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    assert!(!fs.exists("css/app.css").await.unwrap());
    let stored = fs
        .put("css/app.css", "text/css", b"body{}")
        .await
        .expect("put at exact key");
    assert_eq!(stored.key, "css/app.css", "put must keep the exact key");
    assert_eq!(stored.size, 6);
    assert!(fs.exists("css/app.css").await.unwrap());
    assert_eq!(fs.retrieve("css/app.css").await.unwrap(), b"body{}");
    // It really landed at <dir>/css/app.css.
    assert_eq!(
        std::fs::read(dir.path().join("css/app.css")).unwrap(),
        b"body{}"
    );
}

#[test]
fn url_is_relative_by_default() {
    let fs = FsStorage::new("/media", "./d");
    assert_eq!(fs.url("k.png"), "/media/k.png");
}

#[test]
fn public_base_yields_absolute_url() {
    let plugin = StoragePlugin::new()
        .media("/media", "./d")
        .public_base("http://localhost:8100");
    assert_eq!(
        plugin.storage().url("k.png"),
        "http://localhost:8100/media/k.png"
    );
}

#[test]
fn public_base_trailing_slash_is_normalized() {
    let plugin = StoragePlugin::new()
        .media("/media", "./d")
        .public_base("http://localhost:8100/");
    assert_eq!(
        plugin.storage().url("k.png"),
        "http://localhost:8100/media/k.png"
    );
}

#[tokio::test]
async fn path_traversal_filename_is_sanitised() {
    let dir = tempfile::tempdir().unwrap();
    let fs = FsStorage::new("/media", dir.path());

    let stored = fs
        .store("../../etc/passwd", "text/plain", b"x")
        .await
        .unwrap();

    assert!(!stored.key.contains('/'));
    assert!(dir.path().join(&stored.key).exists());
    assert!(fs.retrieve(&stored.key).await.is_ok());
}
