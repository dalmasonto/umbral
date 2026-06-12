//! `media_url()` core template global.
//!
//! The storage backend is a process-global `OnceLock`, so this lives in
//! its own integration binary: the helper must be observed both *before*
//! any registration (raw key falls through) and *after* (key resolves to
//! the backend URL), and that lifecycle only holds in a single process
//! with nothing else racing the lock. The template engine is likewise a
//! process-global, initialised once here via `templates::init`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use umbra_core::storage::{Storage, StorageError, StoredFile, set_storage};
use umbra_core::templates::{init, render};

/// A trivial backend whose `url()` rewrites a key to a recognisable
/// absolute form, so the test can assert the helper routed through it.
struct DummyStorage;

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
        })
    }

    async fn retrieve(&self, _key: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::NotFound)
    }

    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Ok(())
    }

    fn url(&self, key: &str) -> String {
        format!("http://localhost:8100/media/{key}")
    }
}

/// One end-to-end test: with no backend the key falls through unchanged;
/// after registering one, the same template resolves to the backend URL.
#[test]
fn media_url_resolves_through_ambient_storage() {
    // Initialise the template engine from a tempdir holding one template
    // that exercises the `media_url()` global. The engine is a set-once
    // OnceLock, so init runs exactly once in this binary.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("templates");
    std::fs::create_dir_all(&dir).expect("create templates dir");
    std::fs::write(dir.join("m.txt"), "{{ media_url(key) }}").expect("write template");
    init(&[dir]).expect("init template engine");

    // Before any backend is registered, the raw key falls through.
    let raw = render("m.txt", &json!({ "key": "k.png" })).expect("render without backend");
    assert_eq!(raw, "k.png");

    // An empty key always yields the empty string, backend or not.
    let empty = render("m.txt", &json!({ "key": "" })).expect("render empty key");
    assert_eq!(empty, "");

    // Register a backend; now the same key resolves to its public URL.
    assert!(
        set_storage(Arc::new(DummyStorage)),
        "first set_storage wins"
    );
    let resolved = render("m.txt", &json!({ "key": "k.png" })).expect("render with backend");
    assert_eq!(resolved, "http://localhost:8100/media/k.png");
}
