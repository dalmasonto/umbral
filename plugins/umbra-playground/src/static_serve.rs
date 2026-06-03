//! Path-traversal-safe static file serving for the bundled assets.
//!
//! Resolves the requested path under `dist/` and 404s if the result
//! escapes the directory. This is the only piece of code that
//! touches the filesystem on a per-request basis; everything else
//! is in-memory.

use std::path::{Path, PathBuf};

const DIST_DIR: &str = "dist";

/// Crate manifest directory baked at compile time. Used to locate
/// the `dist/` directory at *runtime* — `std::env::var("CARGO_MANIFEST_DIR")`
/// would return `Err` at runtime because Cargo only exports that env
/// var during the build phase (and during `cargo test`, which is why
/// the integration tests passed while the running server 404ed every
/// asset).
///
/// `env!()` evaluates at compile time, so the resulting absolute path
/// is correct for the machine that *built* the binary. Cross-machine
/// deploys would need the assets embedded directly into the binary
/// (a follow-up); for the dev/CI loop this matches where the
/// `build.rs` script wrote the bundle.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

pub fn resolve(asset_path: &str) -> Option<PathBuf> {
    // Strip leading slash; reject obvious traversal attempts up front.
    let trimmed = asset_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains("..") {
        return None;
    }

    let manifest_dir = PathBuf::from(MANIFEST_DIR);
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
