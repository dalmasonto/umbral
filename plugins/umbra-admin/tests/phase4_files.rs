//! Phase 4 file infrastructure tests.
//!
//! 1. `resolve_preview_kind` maps MIME + extension to the correct kind.
//! 2. `file_descriptor` builds the expected JSON shape.
//! 3. Unknown types fall back to `download`.

#![allow(dead_code)]

use umbra_admin::{file_descriptor, resolve_preview_kind};

// =========================================================================
// resolve_preview_kind
// =========================================================================

#[test]
fn image_mime_resolves_to_image() {
    assert_eq!(resolve_preview_kind("image/png", "photo.png"), "image");
    assert_eq!(resolve_preview_kind("image/jpeg", "photo.jpg"), "image");
    assert_eq!(resolve_preview_kind("image/webp", "img.webp"), "image");
    assert_eq!(resolve_preview_kind("image/svg+xml", "img.svg"), "image");
}

#[test]
fn pdf_mime_resolves_to_pdf() {
    assert_eq!(resolve_preview_kind("application/pdf", "report.pdf"), "pdf");
}

#[test]
fn video_mime_resolves_to_video() {
    assert_eq!(resolve_preview_kind("video/mp4", "clip.mp4"), "video");
    assert_eq!(resolve_preview_kind("video/webm", "clip.webm"), "video");
}

#[test]
fn audio_mime_resolves_to_audio() {
    assert_eq!(resolve_preview_kind("audio/mpeg", "track.mp3"), "audio");
    assert_eq!(resolve_preview_kind("audio/ogg", "track.ogg"), "audio");
}

#[test]
fn code_extensions_resolve_to_code() {
    assert_eq!(
        resolve_preview_kind("application/octet-stream", "main.rs"),
        "code"
    );
    assert_eq!(resolve_preview_kind("text/plain", "script.py"), "code");
    assert_eq!(
        resolve_preview_kind("application/octet-stream", "app.js"),
        "code"
    );
    assert_eq!(
        resolve_preview_kind("application/json", "data.json"),
        "code"
    );
    assert_eq!(
        resolve_preview_kind("application/octet-stream", "cfg.toml"),
        "code"
    );
}

#[test]
fn text_mime_resolves_to_text() {
    assert_eq!(resolve_preview_kind("text/plain", "readme.txt"), "text");
    assert_eq!(resolve_preview_kind("text/plain", "log.log"), "text");
}

#[test]
fn unknown_binary_resolves_to_download() {
    assert_eq!(
        resolve_preview_kind("application/zip", "archive.zip"),
        "download"
    );
    assert_eq!(
        resolve_preview_kind("application/octet-stream", "binary.exe"),
        "download"
    );
    assert_eq!(
        resolve_preview_kind("application/x-tar", "backup.tar.gz"),
        "download"
    );
}

// =========================================================================
// file_descriptor JSON shape
// =========================================================================

#[test]
fn file_descriptor_has_required_fields() {
    let desc = file_descriptor(
        "report.pdf",
        184320,
        "application/pdf",
        "/admin/api/files/tok123",
        None,
    );
    assert_eq!(desc["filename"].as_str().unwrap(), "report.pdf");
    assert_eq!(desc["size"].as_u64().unwrap(), 184320);
    assert_eq!(desc["mime"].as_str().unwrap(), "application/pdf");
    assert_eq!(desc["preview_kind"].as_str().unwrap(), "pdf");
    assert_eq!(desc["url"].as_str().unwrap(), "/admin/api/files/tok123");
    assert!(desc["thumbnail_url"].is_null());
    assert!(desc["language"].is_null());
}

#[test]
fn file_descriptor_image_has_thumbnail_url() {
    let desc = file_descriptor(
        "photo.jpg",
        12345,
        "image/jpeg",
        "/admin/api/files/tok456",
        Some("/admin/api/files/tok456/thumb"),
    );
    assert_eq!(desc["preview_kind"].as_str().unwrap(), "image");
    assert_eq!(
        desc["thumbnail_url"].as_str().unwrap(),
        "/admin/api/files/tok456/thumb"
    );
}

#[test]
fn file_descriptor_code_sets_language() {
    let desc = file_descriptor(
        "main.rs",
        0,
        "application/octet-stream",
        "/admin/api/files/t",
        None,
    );
    assert_eq!(desc["preview_kind"].as_str().unwrap(), "code");
    assert_eq!(desc["language"].as_str().unwrap(), "rs");
}
