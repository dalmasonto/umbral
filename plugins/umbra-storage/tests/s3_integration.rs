//! Live S3 integration test for the `umbra-storage` `S3Storage` backend.
//!
//! GATED — skipped at runtime unless `UMBRA_S3_TEST_BUCKET` is set (mirrors
//! the redis-gated cache tests in `umbra-cache/tests/redis_backend.rs`).
//! Point it at an S3-compatible **test** bucket (MinIO, Cloudflare R2,
//! Backblaze B2, DigitalOcean Spaces, real S3) and supply credentials via the
//! standard AWS chain:
//!
//! ```bash
//! UMBRA_S3_TEST_BUCKET=umbra-test \
//! UMBRA_S3_TEST_ENDPOINT=http://localhost:9000 \
//! UMBRA_S3_TEST_REGION=us-east-1 \
//! AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin \
//!   cargo test --features s3 -p umbra-storage --test s3_integration -- --nocapture
//! ```
//!
//! It uses a dedicated `UMBRA_S3_TEST_*` gate (NOT the production
//! `UMBRA_STATIC_*` that `S3Storage::from_env()` reads) so it can never
//! mutate a real configured bucket. Requires the `s3` cargo feature.
#![cfg(feature = "s3")]

use umbra::storage::Storage;
use umbra_storage::S3Storage;

/// Build an `S3Storage` from the `UMBRA_S3_TEST_*` env, or `None` to skip.
fn test_storage() -> Option<S3Storage> {
    let bucket = std::env::var("UMBRA_S3_TEST_BUCKET")
        .ok()
        .filter(|s| !s.is_empty())?;
    let mut builder = S3Storage::builder(bucket)
        .region(std::env::var("UMBRA_S3_TEST_REGION").unwrap_or_else(|_| "us-east-1".into()));
    if let Some(endpoint) = std::env::var("UMBRA_S3_TEST_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
    {
        builder = builder.endpoint(endpoint);
    }
    match builder.build() {
        Ok(storage) => Some(storage),
        Err(e) => {
            eprintln!("UMBRA_S3_TEST_BUCKET set but S3Storage::build failed: {e} — skipping");
            None
        }
    }
}

/// Resolve the test backend or `return` early (skipping the test) when the
/// `UMBRA_S3_TEST_*` env isn't configured.
macro_rules! storage_or_skip {
    () => {
        match test_storage() {
            Some(s) => s,
            None => {
                eprintln!("UMBRA_S3_TEST_BUCKET not set — skipping s3 integration test");
                return;
            }
        }
    };
}

/// The full media lifecycle against a live bucket: generated-key `store`,
/// `exists`, `retrieve` round-trip, a non-empty `url`, `delete`, and `exists`
/// flipping to false afterwards.
#[tokio::test]
async fn s3_store_retrieve_exists_url_delete_round_trip() {
    let s = storage_or_skip!();
    let body = b"hello s3 from umbra-storage";

    let stored = s
        .store("greeting.txt", "text/plain", body)
        .await
        .expect("store should succeed against the live bucket");
    assert_eq!(
        stored.size,
        body.len() as u64,
        "StoredFile.size reflects the uploaded byte count"
    );
    assert!(
        s.exists(&stored.key).await.expect("exists check"),
        "the object exists immediately after store"
    );
    let got = s.retrieve(&stored.key).await.expect("retrieve");
    assert_eq!(&got[..], body, "retrieved bytes round-trip exactly");
    assert!(!s.url(&stored.key).is_empty(), "url() returns a non-empty URL");

    s.delete(&stored.key).await.expect("delete");
    assert!(
        !s.exists(&stored.key).await.expect("exists after delete"),
        "the object is gone after delete"
    );
}

/// `put` writes at an EXACT key (the static/collectstatic path), distinct
/// from `store`'s generated key.
#[tokio::test]
async fn s3_put_exact_key_round_trip() {
    let s = storage_or_skip!();
    let key = "umbra-s3-test/exact/world.txt";

    s.put(key, "text/plain", b"world")
        .await
        .expect("put at an exact key");
    assert!(s.exists(key).await.expect("exists"));
    let got = s.retrieve(key).await.expect("retrieve exact key");
    assert_eq!(&got[..], b"world");

    s.delete(key).await.expect("delete");
}

/// `exists` is `false` for a key that was never written.
#[tokio::test]
async fn s3_exists_false_for_missing_key() {
    let s = storage_or_skip!();
    assert!(
        !s
            .exists("umbra-s3-test/definitely/missing.bin")
            .await
            .expect("exists check on a missing key"),
        "a never-written key does not exist"
    );
}
