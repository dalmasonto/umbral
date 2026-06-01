//! File descriptor helpers — MIME / extension → preview kind, plus the
//! `serde_json::Value` shape the file-preview macros consume.
//!
//! ⚠ A first-class `File` / `Image` ORM field type is deferred. For now
//! you store file paths in `Text` columns and emit a descriptor JSON
//! string for the admin to render. This module owns the descriptor
//! shape so other plugins (and user code) can construct one without
//! re-implementing the MIME table.

/// Resolve the `preview_kind` from a MIME type and file extension.
///
/// Returns a `&'static str` matching one of: `image`, `pdf`, `video`,
/// `audio`, `text`, `code`, `download`. Extension wins over MIME so a
/// `text/plain; charset=utf-8` `.py` file resolves to `code`, not `text`.
pub fn resolve_preview_kind(mime: &str, filename: &str) -> &'static str {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    // Check extension-based code/text first so that e.g. "text/plain; charset=utf-8"
    // on a .py file resolves to "code" rather than "text".
    match ext.as_str() {
        "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "json" | "toml" | "yaml" | "yml" | "html"
        | "css" | "sql" | "sh" | "bash" | "zsh" | "fish" | "md" | "mdx" => return "code",
        "txt" | "log" => return "text",
        _ => {}
    }
    // Then MIME-based rules.
    if mime.starts_with("image/") {
        return "image";
    }
    if mime == "application/pdf" {
        return "pdf";
    }
    if mime.starts_with("video/") {
        return "video";
    }
    if mime.starts_with("audio/") {
        return "audio";
    }
    if mime.starts_with("text/plain") {
        return "text";
    }
    "download"
}

/// Build a file descriptor JSON value.
///
/// `url` is the pre-signed / auth-checked URL the admin should embed;
/// `thumbnail_url` is optional and only set for the `image` kind where
/// a thumbnail has been generated.
pub fn file_descriptor(
    filename: &str,
    size: u64,
    mime: &str,
    url: &str,
    thumbnail_url: Option<&str>,
) -> serde_json::Value {
    let preview_kind = resolve_preview_kind(mime, filename);
    let language: Option<&str> = if preview_kind == "code" {
        Some(filename.rsplit('.').next().unwrap_or("text"))
    } else {
        None
    };
    serde_json::json!({
        "filename":      filename,
        "size":          size,
        "mime":          mime,
        "preview_kind":  preview_kind,
        "url":           url,
        "thumbnail_url": thumbnail_url,
        "language":      language,
    })
}
