//! Upload content-type policy (gaps3 #51).
//!
//! The size cap stops a 20 MB file. It did **nothing** to stop a 2 MB `.exe`
//! renamed to `avatar.png` from landing in an `ImageField` — and neither did
//! anything else, which is the gap this closes.
//!
//! The load-bearing decision: enforcement sniffs the **bytes**. A client-declared
//! `Content-Type` is trivially spoofed, so a policy that only reads the
//! declaration stops nobody. The declaration must be on the list, the bytes' real
//! signature must be on the list, and the two must agree.

use std::sync::Arc;
use umbral::storage::{Storage, StorageError};
use umbral_storage::{FsStorage, StoragePlugin};

const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0];
const ELF: &[u8] = &[0x7F, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0];

fn images() -> Arc<dyn Storage> {
    let dir = tempfile::tempdir().expect("tempdir");
    let inner: Arc<dyn Storage> = Arc::new(FsStorage::new(
        dir.keep().to_string_lossy().to_string(),
        "/media",
    ));
    StoragePlugin::type_limited_for_test(inner, &["image/png", "image/jpeg"])
}

#[tokio::test]
async fn a_real_png_is_accepted() {
    let s = images();
    assert!(
        s.store("avatar.png", "image/png", PNG).await.is_ok(),
        "a genuine PNG declared as image/png must store",
    );
}

/// **The attack this exists for.** An executable renamed `avatar.png`, declaring
/// `image/png`. Only the bytes give it away.
#[tokio::test]
async fn an_executable_renamed_to_png_is_rejected() {
    let s = images();
    let err = s
        .store("avatar.png", "image/png", ELF)
        .await
        .expect_err("a renamed executable must NOT be stored");
    assert!(
        matches!(err, StorageError::UnsupportedType { .. }),
        "expected UnsupportedType, got {err:?}",
    );
}

/// The declared type must also *agree* with the bytes: you can't store a JPEG
/// into a field that was told it's getting a PNG.
#[tokio::test]
async fn a_declared_type_that_contradicts_the_bytes_is_rejected() {
    let s = images();
    let err = s
        .store("x.png", "image/png", JPEG)
        .await
        .expect_err("declared PNG, actually JPEG");
    assert!(matches!(err, StorageError::UnsupportedType { .. }));
}

/// A type that's simply not on the list is refused even when it's genuine.
#[tokio::test]
async fn a_type_off_the_allowlist_is_rejected() {
    let s = images();
    let err = s
        .store("doc.pdf", "application/pdf", b"%PDF-1.7\n")
        .await
        .expect_err("pdf is not on the image allow-list");
    assert!(matches!(err, StorageError::UnsupportedType { .. }));
}

/// SVG is not in the image set on purpose — it's a script-execution vector, not
/// a picture, and an image allow-list should not invite one in.
#[test]
fn svg_is_not_in_the_built_in_image_set() {
    assert!(
        !umbral_storage::IMAGE_TYPES.contains(&"image/svg+xml"),
        "SVG must not be in the default image allow-list",
    );
}
