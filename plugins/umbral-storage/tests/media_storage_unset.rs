//! Verify that accessing the ambient storage backend when no backend has
//! been registered returns a clean, typed error and does NOT panic. Moved
//! from umbral-media.

use std::sync::Arc;

use umbral::storage::{StorageError, storage_opt};
use umbral_storage::{FsStorage, MediaTracking};

#[test]
fn try_storage_returns_no_backend_error_when_unset() {
    let result = umbral::storage::try_storage();
    match result {
        Err(StorageError::NoBackend) => {}
        Ok(_) => panic!("expected NoBackend but storage() returned a backend"),
        Err(other) => panic!("expected NoBackend but got: {other}"),
    }
}

#[test]
fn storage_opt_returns_none_when_unset() {
    assert!(
        storage_opt().is_none(),
        "storage_opt() must be None before any set_storage call"
    );
}

#[test]
fn no_backend_error_formats_clearly() {
    let msg = StorageError::NoBackend.to_string();
    assert!(
        msg.contains("backend") || msg.contains("MediaPlugin") || msg.contains("set_storage"),
        "NoBackend message should be actionable, got: {msg}"
    );
}

#[test]
fn fs_storage_constructs_without_ambient_backend() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _fs = FsStorage::new("/media", dir.path());
}

#[test]
fn media_tracking_constructs_without_ambient_backend() {
    let dir = tempfile::tempdir().expect("tempdir");
    let inner: Arc<dyn umbral::storage::Storage> = Arc::new(FsStorage::new("/media", dir.path()));
    let _tracking = MediaTracking::new(inner);
}
