//! Built-in image processing (gaps3 #50).
//!
//! The `on_upload` hook and the processing lifecycle have shipped for a while —
//! what was missing was anything to put *in* the hook. No imaging crate was even
//! a dependency, so every app with user-uploaded images wrote its own pipeline.

#![cfg(feature = "images")]

use std::sync::Arc;
use umbral::storage::Storage;
use umbral_storage::{FsStorage, StoragePlugin, Thumbnail, variant_key};

/// A variant's key is a pure function of the original's — so a template can
/// build a thumbnail URL with no extra column, no join and no second lookup.
#[test]
fn a_variant_key_is_derived_from_the_original() {
    assert_eq!(
        variant_key("media/8f3a-avatar.png", "thumb"),
        "media/8f3a-avatar__thumb.png",
    );
    // Extension-less keys still get a stable, collision-free suffix.
    assert_eq!(variant_key("media/blob", "thumb"), "media/blob__thumb");
}

fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::new(w, h);
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .expect("encode");
    buf
}

async fn boot_storage() -> Arc<dyn Storage> {
    let dir = tempfile::tempdir().expect("tempdir");
    let fs = FsStorage::new(dir.keep().to_string_lossy().to_string(), "/media");
    let storage: Arc<dyn Storage> = Arc::new(fs);
    umbral::storage::set_storage(storage.clone());
    storage
}

/// The feature: an uploaded image gets its resized variants written, fitted
/// within the box and keeping aspect ratio.
#[tokio::test]
async fn thumbnails_are_generated_at_derived_keys() {
    let storage = boot_storage().await;
    let stored = storage
        .store("avatar.png", "image/png", &png_bytes(400, 200))
        .await
        .expect("store original");

    static SPECS: &[Thumbnail] = &[Thumbnail::new("thumb", 100, 100)];
    let proc = umbral_storage::thumbnails(SPECS);

    let media = umbral_storage::MediaFile {
        id: 1,
        key: stored.key.clone(),
        filename: "avatar.png".into(),
        content_type: "image/png".into(),
        size: 0,
        status: "ready".into(),
        uploaded_at: chrono::Utc::now(),
    };
    proc(media).await.expect("processor");

    let key = variant_key(&stored.key, "thumb");
    let bytes = storage.retrieve(&key).await.expect("thumbnail was written");
    let img = image::load_from_memory(&bytes).expect("decodes");

    // 400x200 fitted into a 100x100 box → 100x50. Aspect ratio preserved; the
    // box is a bound, never a shape to stretch to.
    assert_eq!(
        (
            image::GenericImageView::width(&img),
            image::GenericImageView::height(&img)
        ),
        (100, 50),
        "the image is fitted WITHIN the box, keeping its aspect ratio",
    );
}

/// It must never upscale: enlarging a 40x40 avatar to 200x200 invents detail that
/// was never there and costs bytes to say nothing.
#[tokio::test]
async fn a_small_image_is_not_upscaled() {
    let storage = boot_storage().await;
    let stored = storage
        .store("tiny.png", "image/png", &png_bytes(40, 40))
        .await
        .expect("store");

    static SPECS: &[Thumbnail] = &[Thumbnail::new("thumb", 200, 200)];
    let proc = umbral_storage::thumbnails(SPECS);
    proc(umbral_storage::MediaFile {
        id: 1,
        key: stored.key.clone(),
        filename: "tiny.png".into(),
        content_type: "image/png".into(),
        size: 0,
        status: "ready".into(),
        uploaded_at: chrono::Utc::now(),
    })
    .await
    .expect("processor");

    let bytes = storage
        .retrieve(&variant_key(&stored.key, "thumb"))
        .await
        .expect("variant");
    let img = image::load_from_memory(&bytes).expect("decodes");
    assert_eq!(
        (
            image::GenericImageView::width(&img),
            image::GenericImageView::height(&img)
        ),
        (40, 40),
        "a 40x40 source asked for a 200x200 thumb must stay 40x40",
    );
}

/// A non-image upload passes through untouched. A PDF in the same bucket is not
/// an error — it is just not a thing to thumbnail.
#[tokio::test]
async fn a_non_image_upload_is_left_alone() {
    let storage = boot_storage().await;
    let stored = storage
        .store("doc.pdf", "application/pdf", b"%PDF-1.7\n")
        .await
        .expect("store");

    static SPECS: &[Thumbnail] = &[Thumbnail::new("thumb", 100, 100)];
    let proc = umbral_storage::thumbnails(SPECS);
    proc(umbral_storage::MediaFile {
        id: 1,
        key: stored.key.clone(),
        filename: "doc.pdf".into(),
        content_type: "application/pdf".into(),
        size: 0,
        status: "ready".into(),
        uploaded_at: chrono::Utc::now(),
    })
    .await
    .expect("a non-image must not be an error");

    assert!(
        storage
            .retrieve(&variant_key(&stored.key, "thumb"))
            .await
            .is_err(),
        "no variant should have been written for a PDF",
    );
}
