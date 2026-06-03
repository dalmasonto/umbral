//! Path-traversal-safe static file serving for the bundled assets.
//!
//! Resolves the requested path under `dist/` and 404s if the result
//! escapes the directory. This is the only piece of code that
//! touches the filesystem on a per-request basis; everything else
//! is in-memory.

use std::path::{Path, PathBuf};

const DIST_DIR: &str = "dist";

pub fn resolve(asset_path: &str) -> Option<PathBuf> {
    // Strip leading slash; reject obvious traversal attempts up front.
    let trimmed = asset_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains("..") {
        return None;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    let candidate = manifest_dir.join(DIST_DIR).join(trimmed);

    // Canonicalize the parent + filename to catch symlink escapes.
    let canonical = candidate.canonicalize().ok()?;
    let dist_canonical = manifest_dir.join(DIST_DIR).canonicalize().ok()?;
    if !canonical.starts_with(&dist_canonical) {
        return None;
    }
    if !canonical.is_file() {
        return None;
    }
    Some(canonical)
}

pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("map") => "application/json; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}
