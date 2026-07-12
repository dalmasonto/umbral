//! Built-in image processing — thumbnails and resizing (gaps3 #50).
//!
//! The `on_upload` hook, the `processing` / `ready` / `failed` lifecycle and the
//! concurrency cap have all shipped for a while; what was missing was anything to
//! *put in the hook*. No imaging crate was even a dependency, so every app with
//! user-uploaded images wrote its own pipeline — which is exactly the "the
//! framework gives you the parts, you assemble them" tax this is meant to remove.
//!
//! Behind the `images` cargo feature, so an app with no user uploads never
//! compiles a codec.
//!
//! # What it does
//!
//! [`thumbnails`] returns a [`Processor`] that, for every image upload, writes one
//! resized variant per spec at a **derived key**, so a variant's URL is a pure
//! function of the original's — no extra column, no join, no second lookup:
//!
//! ```text
//! original :  media/8f3a…-avatar.png
//! thumb    :  media/8f3a…-avatar__thumb.png
//! ```
//!
//! # What it deliberately does not do
//!
//! It does not **upscale**. A 40×40 avatar asked for a 200×200 thumbnail stays
//! 40×40: enlarging it invents detail that was never there and costs bytes to say
//! nothing. Aspect ratio is always preserved — the spec is a bounding box the
//! image is fitted *within*, never a shape it is stretched to.

use std::sync::Arc;

use image::ImageFormat;
use image::imageops::FilterType;

use crate::media::{BoxError, MediaFile, Processor};

/// One resized variant: a name (which becomes the key suffix) and the bounding
/// box the image is fitted within.
#[derive(Debug, Clone, Copy)]
pub struct Thumbnail {
    /// Key suffix and lookup name — `"thumb"` → `…__thumb.png`.
    pub name: &'static str,
    /// Maximum width, in pixels.
    pub width: u32,
    /// Maximum height, in pixels.
    pub height: u32,
}

impl Thumbnail {
    pub const fn new(name: &'static str, width: u32, height: u32) -> Self {
        Self {
            name,
            width,
            height,
        }
    }
}

/// The storage key a variant lives at, derived from the original's.
///
/// A pure function of the original key, which is the whole point: a template can
/// build a thumbnail URL without a database round-trip or an extra column, and a
/// variant that hasn't been generated yet simply 404s rather than corrupting the
/// page.
pub fn variant_key(original_key: &str, name: &str) -> String {
    match original_key.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}__{name}.{ext}"),
        None => format!("{original_key}__{name}"),
    }
}

/// A [`Processor`] that writes one resized variant per spec for every image
/// upload. Register it with `StoragePlugin::on_upload`.
///
/// Non-image uploads pass straight through untouched — a PDF in the same bucket
/// is not an error, it is just not a thing to thumbnail.
pub fn thumbnails(specs: &'static [Thumbnail]) -> Processor {
    Arc::new(move |media: MediaFile| {
        Box::pin(async move {
            let Some(format) = format_for(&media.content_type) else {
                // Not an image (or not one we encode). Nothing to do — and
                // emphatically not a failure: the upload itself was fine.
                return Ok(());
            };
            let storage = umbral::storage::storage();
            let bytes = storage.retrieve(&media.key).await?;

            // Decoding is CPU-bound and can be slow on a large image; doing it on
            // the async runtime would stall unrelated requests on the same worker
            // thread. The ambient concurrency cap bounds how many of these run at
            // once.
            let specs_owned: Vec<Thumbnail> = specs.to_vec();
            let variants =
                tokio::task::spawn_blocking(move || -> Result<Vec<(String, Vec<u8>)>, BoxError> {
                    let img = image::load_from_memory(&bytes)?;
                    let mut out = Vec::new();
                    let (src_w, src_h) = (
                        image::GenericImageView::width(&img),
                        image::GenericImageView::height(&img),
                    );
                    for spec in specs_owned {
                        // Clamp the box to the source's own size: `resize` will happily
                        // ENLARGE to fill a bigger box, and upscaling invents detail
                        // that was never there while costing bytes to say nothing. A
                        // 40x40 avatar asked for a 200x200 thumb stays 40x40.
                        let (w, h) = (spec.width.min(src_w), spec.height.min(src_h));
                        let resized = img.resize(w, h, FilterType::Lanczos3);
                        let mut buf: Vec<u8> = Vec::new();
                        resized.write_to(&mut std::io::Cursor::new(&mut buf), format)?;
                        out.push((spec.name.to_string(), buf));
                    }
                    Ok(out)
                })
                .await
                .map_err(|e| Box::new(e) as BoxError)??;

            for (name, buf) in variants {
                let key = variant_key(&media.key, &name);
                // Store under the derived key. The bytes are re-encoded by us, so
                // they genuinely are `content_type` — an accept-policy decorator
                // underneath will sniff them and agree.
                // `store_at`, not `store`: `store` would mint a fresh UUID key
                // and the derived-key contract (a variant's URL is a pure
                // function of the original's) would silently not hold.
                storage
                    .store_at(&key, &media.content_type, &buf)
                    .await
                    .map_err(|e| Box::new(e) as BoxError)?;
            }
            Ok(())
        })
    })
}

/// The encoder to write a variant with — the same format as the original, so a
/// PNG stays a PNG and transparency survives.
fn format_for(content_type: &str) -> Option<ImageFormat> {
    match content_type.split(';').next().unwrap_or("").trim() {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::WebP),
        _ => None,
    }
}
