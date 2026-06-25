//! Wave 3 — `parse_and_store_multipart` stores file parts through the
//! ambient backend and returns the flat `(name, value)` pairs the form
//! pipeline expects, with empty file parts skipped.
//!
//! Dedicated binary: `set_storage` writes a process-wide OnceLock, so the
//! fake backend registered here can't poison other tests.

use std::sync::Arc;
use std::sync::Mutex;

use umbral::storage::{Storage, StorageError, StoredFile, set_storage};
use umbral::web::parse_and_store_multipart;

const BOUNDARY: &str = "X-UMBRAL-MERGE-BOUNDARY";

/// An in-memory fake: every `store` records the call and returns a key
/// derived from the filename, so the test can assert which key flowed back.
#[derive(Debug, Default)]
struct FakeStorage {
    stored: Mutex<Vec<(String, String, Vec<u8>)>>,
}

#[umbral::storage::async_trait]
impl Storage for FakeStorage {
    async fn store(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        self.stored.lock().unwrap().push((
            filename.to_string(),
            content_type.to_string(),
            bytes.to_vec(),
        ));
        let key = format!("stored/{filename}");
        let url = format!("https://cdn.example.test/{key}");
        Ok(StoredFile {
            key,
            url,
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
        format!("https://cdn.example.test/{key}")
    }
}

/// `(name, filename, content_type, value)`; `None` filename = text field.
type PartSpec<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

fn build_body(parts: &[PartSpec<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, filename, content_type, value) in parts {
        out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match filename {
            Some(fname) => {
                out.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\n"
                    )
                    .as_bytes(),
                );
                if let Some(ct) = content_type {
                    out.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                }
            }
            None => {
                out.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
                );
            }
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
}

#[tokio::test]
async fn stores_files_and_flattens_to_pairs() {
    set_storage(Arc::new(FakeStorage::default()));
    let ct = format!("multipart/form-data; boundary={BOUNDARY}");

    let png = b"\x89PNGfake";
    let body = build_body(&[
        ("title", None, None, b"Hello"),
        ("cover", Some("p.png"), Some("image/png"), png),
    ]);

    let pairs = parse_and_store_multipart(&ct, body).await.unwrap();

    // Text fields plus the stored key for the file. Key comes from the
    // fake backend's store(), proving the file went through Storage.
    assert!(
        pairs.contains(&("title".to_string(), "Hello".to_string())),
        "text field must pass through: {pairs:?}"
    );
    assert!(
        pairs.contains(&("cover".to_string(), "stored/p.png".to_string())),
        "file must become (field, stored_key): {pairs:?}"
    );
    assert_eq!(pairs.len(), 2);
}

#[tokio::test]
async fn empty_file_part_is_skipped() {
    // Reuses the backend registered by the first test (same process, same
    // OnceLock); set_storage is first-wins so this call is a no-op if the
    // other test ran first — either way a backend is present.
    set_storage(Arc::new(FakeStorage::default()));
    let ct = format!("multipart/form-data; boundary={BOUNDARY}");

    let body = build_body(&[
        ("title", None, None, b"Edit"),
        // User submitted the edit form WITHOUT choosing a new file: the
        // browser still sends the part, but with an empty body.
        ("cover", Some(""), Some("application/octet-stream"), b""),
    ]);

    let pairs = parse_and_store_multipart(&ct, body).await.unwrap();

    // No pair for `cover` — the empty part is skipped so the existing
    // stored value isn't clobbered. Text field still present.
    assert_eq!(pairs, vec![("title".to_string(), "Edit".to_string())]);
    assert!(
        !pairs.iter().any(|(k, _)| k == "cover"),
        "empty file part must not emit a pair: {pairs:?}"
    );
}
