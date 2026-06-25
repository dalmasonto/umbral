//! Named storage registry (storage-unification stage 1).
//!
//! Proves the named storage registry: `"default"` (media)
//! and `"staticfiles"` (static) resolve to independent backends, the
//! back-compat accessors (`storage`/`set_storage`/`storage_opt`) operate on
//! `"default"`, and registration is set-once per name.
//!
//! The registry is a process-global `OnceLock<Mutex<HashMap>>`, so every
//! test in this file shares it. To keep the set-once assertions
//! deterministic, the cross-cutting behaviour lives in ONE sequential test
//! that owns the `"default"`/`"staticfiles"` names; isolated checks use
//! their own distinct names.

use std::sync::Arc;

use async_trait::async_trait;
use umbral_core::storage::{
    set_storage, set_storage_named, storage, storage_opt, storage_opt_named, try_storage_named,
    Storage, StorageError, StoredFile, DEFAULT, STATICFILES,
};

/// A trivial backend that tags every URL/key with a label, so a test can
/// tell two registered instances apart.
struct Labelled {
    label: &'static str,
}

#[async_trait]
impl Storage for Labelled {
    async fn store(
        &self,
        filename: &str,
        _ct: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        let key = format!("{}/{filename}", self.label);
        Ok(StoredFile {
            url: self.url(&key),
            key,
            size: bytes.len() as u64,
        })
    }

    async fn retrieve(&self, _key: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::NotFound)
    }

    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Ok(())
    }

    fn url(&self, key: &str) -> String {
        format!("/{}/{key}", self.label)
    }
}

#[tokio::test]
async fn named_registry_resolves_independently_and_is_set_once() {
    // "staticfiles" and "default" are different backends under different names.
    assert!(set_storage_named(
        STATICFILES,
        Arc::new(Labelled { label: "static" })
    ));
    // Back-compat `set_storage` registers the "default" name.
    assert!(set_storage(Arc::new(Labelled { label: "media" })));

    // Each name resolves to its OWN backend, independently.
    assert_eq!(storage_opt_named(STATICFILES).unwrap().url("x"), "/static/x");
    assert_eq!(storage_opt_named(DEFAULT).unwrap().url("x"), "/media/x");

    // Back-compat: `storage()` == the "default" instance.
    assert_eq!(storage().url("x"), "/media/x");
    assert_eq!(storage_opt().unwrap().url("x"), "/media/x");

    // Set-once per name: a SECOND set for "default" is rejected and the
    // first-registered backend is kept.
    assert!(!set_storage(Arc::new(Labelled { label: "other" })));
    assert_eq!(storage().url("x"), "/media/x");

    // Likewise a second set for "staticfiles" is rejected.
    assert!(!set_storage_named(
        STATICFILES,
        Arc::new(Labelled { label: "other" })
    ));
    assert_eq!(storage_opt_named(STATICFILES).unwrap().url("x"), "/static/x");
}

#[tokio::test]
async fn missing_name_resolves_to_none_and_err() {
    // A name that was never registered.
    assert!(storage_opt_named("definitely-missing").is_none());
    assert!(matches!(
        try_storage_named("definitely-missing"),
        Err(StorageError::NoBackend)
    ));
}

#[tokio::test]
async fn exists_default_via_retrieve_through_registry() {
    // A backend whose `exists` is the trait default (driven by `retrieve`),
    // registered under its own name so it doesn't collide with other tests.
    struct Mem {
        objects: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
    }

    #[async_trait]
    impl Storage for Mem {
        async fn store(
            &self,
            filename: &str,
            _ct: &str,
            bytes: &[u8],
        ) -> Result<StoredFile, StorageError> {
            let key = format!("k-{filename}");
            self.objects
                .lock()
                .unwrap()
                .insert(key.clone(), bytes.to_vec());
            Ok(StoredFile {
                url: self.url(&key),
                key,
                size: bytes.len() as u64,
            })
        }
        async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
            self.objects
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or(StorageError::NotFound)
        }
        async fn delete(&self, _key: &str) -> Result<(), StorageError> {
            Ok(())
        }
        fn url(&self, key: &str) -> String {
            format!("/mem/{key}")
        }
    }

    let backend: Arc<dyn Storage> = Arc::new(Mem {
        objects: std::sync::Mutex::new(std::collections::HashMap::new()),
    });
    assert!(set_storage_named("exists-test", backend.clone()));
    let s = try_storage_named("exists-test").unwrap();

    let stored = s.store("a.txt", "text/plain", b"hi").await.unwrap();
    assert!(s.exists(&stored.key).await.unwrap());
    assert!(!s.exists("missing-key").await.unwrap());
}
