//! Ambient `Storage` registry tests.
//!
//! The registry is a process-global `OnceLock`, so these run in their own
//! test binary: `storage_opt()` must observe `None` *before* any
//! registration, which only holds if no other test in the same process
//! has already set the lock. Keeping this file as its own integration
//! binary guarantees that isolation.

use std::sync::Arc;

use async_trait::async_trait;
use umbra_core::storage::{Storage, StorageError, StoredFile, set_storage, storage, storage_opt};

/// A trivial in-memory backend so the registry test doesn't touch disk.
struct DummyStorage {
    tag: String,
}

#[async_trait]
impl Storage for DummyStorage {
    async fn store(
        &self,
        _filename: &str,
        _content_type: &str,
        _bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        Ok(StoredFile {
            key: "k".to_string(),
            url: self.url("k"),
            size: _bytes.len() as u64,
        })
    }

    async fn retrieve(&self, _key: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::NotFound)
    }

    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Ok(())
    }

    fn url(&self, key: &str) -> String {
        format!("dummy:{}/{key}", self.tag)
    }
}

/// One test, run end-to-end: the set-once OnceLock can only be observed
/// in its `None` → first-set → second-set-ignored sequence within a
/// single process, so the whole lifecycle lives in one `#[test]`.
#[test]
fn ambient_registry_lifecycle() {
    // Before any registration, the optional accessor is `None`.
    assert!(
        storage_opt().is_none(),
        "storage_opt() must be None before any set_storage call"
    );

    // First registration wins and returns true.
    let first = Arc::new(DummyStorage {
        tag: "first".to_string(),
    });
    assert!(set_storage(first), "first set_storage should win");

    // Both accessors now return the registered backend.
    assert!(storage_opt().is_some());
    assert_eq!(storage().url("x"), "dummy:first/x");

    // A second registration is ignored (set-once); the first stays.
    let second = Arc::new(DummyStorage {
        tag: "second".to_string(),
    });
    assert!(
        !set_storage(second),
        "second set_storage should be ignored and return false"
    );
    assert_eq!(
        storage().url("x"),
        "dummy:first/x",
        "first-registered backend must survive a second set_storage"
    );
}
