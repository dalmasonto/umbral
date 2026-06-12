//! Wave 2 — `FileField::url()` resolves through the ambient storage
//! backend when one IS registered.
//!
//! Dedicated binary: `set_storage` writes a process-wide OnceLock, so
//! registering a fake backend here can't poison the no-storage tests in
//! `file_image_field.rs`.

use std::sync::Arc;

use umbra::orm::FileField;
use umbra::storage::{Storage, StorageError, StoredFile, set_storage};

/// A fake backend whose `url` prefixes the key with a CDN host — enough
/// to prove `FileField::url()` routes through the registered backend
/// rather than echoing the raw key.
#[derive(Debug)]
struct FakeStorage;

#[umbra::storage::async_trait]
impl Storage for FakeStorage {
    async fn store(
        &self,
        _filename: &str,
        _content_type: &str,
        _bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        Err(StorageError::Backend("not used in this test".into()))
    }
    async fn retrieve(&self, _key: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::NotFound)
    }
    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Ok(())
    }
    fn url(&self, key: &str) -> String {
        format!("https://cdn.example.test/{key}")
    }
}

#[test]
fn url_resolves_through_registered_backend() {
    set_storage(Arc::new(FakeStorage));
    let f = FileField::from("ab12-photo.jpg");
    assert_eq!(
        f.url(),
        "https://cdn.example.test/ab12-photo.jpg",
        "url() should route through the ambient Storage backend"
    );
    // ImageField goes through the same path via Deref.
    let img = umbra::orm::ImageField::from("logo.png");
    assert_eq!(img.url(), "https://cdn.example.test/logo.png");
}
