//! gaps4 #56 (signed, time-bounded media URLs) + #58 (a gated proxy route
//! that streams gated bytes through a non-FS backend).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbral::prelude::Plugin;
use umbral::storage::{Storage, StorageError, StoredFile};
use umbral_storage::{StoragePlugin, signed_media_url};

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

async fn body_of(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap()
        .to_vec()
}

// ── #56: signed URLs on the filesystem backend ─────────────────────────

#[tokio::test]
async fn a_valid_signed_url_serves_and_anything_else_403s() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("invoice.pdf"), b"PRIVATE-PDF").unwrap();

    let app = StoragePlugin::new()
        .media("/media", dir.path())
        .media_signed_urls()
        .routes();

    // A freshly minted, unexpired link serves the file.
    let url = signed_media_url("/media", "invoice.pdf", Duration::from_secs(300));
    let resp = app.clone().oneshot(get(&url)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "valid signed url serves");
    assert_eq!(body_of(resp).await, b"PRIVATE-PDF");

    // The bare URL (no signature) is denied — signed mode gates everything.
    let resp = app
        .clone()
        .oneshot(get("/media/invoice.pdf"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "unsigned request denied"
    );

    // A tampered signature is denied.
    let mut chars: Vec<char> = url.chars().collect();
    let last = chars.len() - 1;
    chars[last] = if chars[last] == 'a' { 'b' } else { 'a' };
    let tampered: String = chars.into_iter().collect();
    let resp = app.oneshot(get(&tampered)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "tampered sig denied");
}

#[tokio::test]
async fn signed_urls_compose_with_a_closure_gate() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), b"OK").unwrap();

    // Both a signed-url mode AND a header gate: a valid signature grants
    // access outright; otherwise the closure decides.
    let app = StoragePlugin::new()
        .media("/media", dir.path())
        .media_signed_urls()
        .media_access(|headers: axum::http::HeaderMap, _key: String| async move {
            headers.contains_key("x-allow")
        })
        .routes();

    // Signed link (no header) → allowed by the signature.
    let url = signed_media_url("/media", "f.txt", Duration::from_secs(300));
    assert_eq!(
        app.clone().oneshot(get(&url)).await.unwrap().status(),
        StatusCode::OK,
    );
    // Header, no signature → allowed by the closure.
    let req = Request::builder()
        .uri("/media/f.txt")
        .header("x-allow", "1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    // Neither → denied.
    assert_eq!(
        app.oneshot(get("/media/f.txt")).await.unwrap().status(),
        StatusCode::FORBIDDEN,
    );
}

// ── #58: the gated proxy route for a non-FS backend ────────────────────

/// A minimal in-memory Storage backend — the stand-in for S3 / a custom
/// backend that serves its own URLs (`dir: None`).
#[derive(Default)]
struct MemStorage(Arc<Mutex<HashMap<String, Vec<u8>>>>);

#[umbral::async_trait]
impl Storage for MemStorage {
    async fn store(
        &self,
        filename: &str,
        _content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        self.0
            .lock()
            .unwrap()
            .insert(filename.to_string(), bytes.to_vec());
        Ok(StoredFile {
            key: filename.to_string(),
            url: format!("mem://{filename}"),
            size: bytes.len() as u64,
        })
    }
    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(StorageError::NotFound)
    }
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
    fn url(&self, key: &str) -> String {
        format!("mem://{key}")
    }
}

#[tokio::test]
async fn a_gated_non_fs_backend_serves_bytes_through_the_proxy() {
    let mem = MemStorage::default();
    mem.0
        .lock()
        .unwrap()
        .insert("report.bin".into(), b"BACKEND-BYTES".to_vec());

    // A gate on a dir:None backend now mounts a proxy route (gaps4 #58)
    // instead of being a silent no-op.
    let app = StoragePlugin::new()
        .media_with_storage("/files", Arc::new(mem))
        .media_access(|headers: axum::http::HeaderMap, _key: String| async move {
            headers.contains_key("x-allow")
        })
        .routes();

    // Denied without the credential — the gate runs before any byte.
    let resp = app.clone().oneshot(get("/files/report.bin")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Allowed → the proxy streams the bytes through the backend.
    let req = Request::builder()
        .uri("/files/report.bin")
        .header("x-allow", "1")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "gated proxy serves the bytes"
    );
    assert_eq!(body_of(resp).await, b"BACKEND-BYTES");

    // An unknown key behind the (open, header-carrying) gate → 404.
    let req = Request::builder()
        .uri("/files/nope.bin")
        .header("x-allow", "1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.oneshot(req).await.unwrap().status(),
        StatusCode::NOT_FOUND,
    );
}

#[tokio::test]
async fn signed_urls_work_through_the_non_fs_proxy_too() {
    let mem = MemStorage::default();
    mem.0
        .lock()
        .unwrap()
        .insert("share.jpg".into(), b"IMG".to_vec());

    let app = StoragePlugin::new()
        .media_with_storage("/files", Arc::new(mem))
        .media_signed_urls()
        .routes();

    let url = signed_media_url("/files", "share.jpg", Duration::from_secs(300));
    let resp = app.clone().oneshot(get(&url)).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "signed url through the proxy"
    );
    assert_eq!(body_of(resp).await, b"IMG");

    // Unsigned → 403.
    assert_eq!(
        app.oneshot(get("/files/share.jpg")).await.unwrap().status(),
        StatusCode::FORBIDDEN,
    );
}
