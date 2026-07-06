//! Markdown-editor image upload (gaps2 #36).
//!
//! `POST {base}/upload-image` — the endpoint the admin's EasyMDE markdown
//! editor calls from its `imageUploadFunction`. A staff user pastes, drops,
//! or selects an image in a markdown field; the editor POSTs the bytes here
//! as `multipart/form-data`, this handler validates + stores them through
//! the ambient [`umbral::storage`] seam, and returns `{ "url": "<public url>" }`
//! which the editor inserts into the markdown.
//!
//! ## Gating
//!
//! Staff-only, via [`require_staff`] — the same session gate every other
//! admin route uses. There is no `{table}` in the path (a media upload is
//! not scoped to one model), so the per-model `permcheck` gate does not
//! apply; staff status is the correct boundary, mirroring how the admin
//! treats its other cross-model endpoints (palette search, dashboard data).
//! Unauthenticated → redirect/401; logged-in-non-staff → 403.
//!
//! ## Storage seam
//!
//! Reads the backend through [`umbral::storage::storage_opt`]. When no
//! backend is installed (no `StoragePlugin`, no `set_storage`) the handler
//! returns a clear `409 Conflict` JSON error rather than panicking — the
//! editor surfaces it through `onError`. With a backend, the bytes go
//! through [`Storage::store`] and the returned [`StoredFile::url`] is sent
//! back.
//!
//! ## Validation
//!
//! - The `Content-Type` of the part must be a known image MIME
//!   (`png|jpeg|gif|webp|svg+xml`); anything else is `415`.
//! - The body must be non-empty and within [`MAX_UPLOAD_BYTES`]; an
//!   oversized body is `413`.
//!
//! The storage layer's active-content guard additionally renames a stored
//! `.svg`/`.html` to `.txt` so an uploaded SVG can't execute as markup when
//! served — that rename is correct and intentional.

use axum::body::Bytes;
use umbral::web::{HeaderMap, IntoResponse, Response, StatusCode};

use crate::auth::require_staff;

/// Maximum accepted upload size, in bytes. 10 MiB — a sane cap for an
/// inline editor image; larger media belongs in a dedicated upload flow.
const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;

/// MIME types accepted for an inline markdown image.
const ALLOWED_IMAGE_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
];

/// Recognise the allow-listed raster image types by their leading magic bytes,
/// returning the canonical MIME. `None` when the bytes match no known raster
/// format. SVG is text (no reliable signature) and is validated separately.
/// audit_2 admin #6.
fn sniff_raster_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Build a small JSON error response. EasyMDE's `imageUploadFunction`
/// surfaces the `error` string through its `onError` callback.
fn json_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "error": message }).to_string();
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

/// `POST {base}/upload-image` — store an editor image, return `{ "url": ... }`.
///
/// The body is `multipart/form-data` with one file part (field name
/// `image` or `file`). On success returns `200` + `{"url":"<public url>"}`;
/// the admin's `imageUploadFunction` reads `url` and calls `onSuccess(url)`.
pub(crate) async fn upload_image(headers: HeaderMap, body: Bytes) -> Response {
    let base = crate::branding::current().base_path;
    let path = format!("{base}/upload-image");
    // Staff gate — identical to every other admin route. Returns a
    // redirect (unauthenticated) or 403 (non-staff) Response on failure.
    let _who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };

    // The body must be multipart/form-data so we can pull out the file part.
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !umbral::web::multipart::is_multipart(content_type) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "upload must be multipart/form-data",
        );
    }

    let form = match umbral::web::multipart::parse_multipart(content_type, body).await {
        Ok(f) => f,
        Err(e) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("could not parse upload: {e}"),
            );
        }
    };

    // Pick the first file part named `image` or `file`, else the first file.
    let part = form
        .files
        .iter()
        .find(|f| f.field_name == "image" || f.field_name == "file")
        .or_else(|| form.files.first());
    let Some(part) = part else {
        return json_error(StatusCode::BAD_REQUEST, "no image part in upload");
    };

    if part.bytes.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "uploaded image is empty");
    }
    if part.bytes.len() > MAX_UPLOAD_BYTES {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "image exceeds the 10 MiB upload limit",
        );
    }

    // Validate the declared content-type against the image allow-list. We
    // trust the part's declared type for the allow-list check; the storage
    // layer's active-content guard is the second line of defence (it renames
    // svg/html to .txt so it can never be served as executable markup).
    let declared = part
        .content_type
        .as_deref()
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !ALLOWED_IMAGE_TYPES.contains(&declared.as_str()) {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "only image uploads are allowed (png, jpeg, gif, webp, svg)",
        );
    }

    // audit_2 admin #6 — the allow-list above trusts the *declared* content
    // type; a script/HTML payload labelled `image/png` sails through it. Sniff
    // the leading magic bytes and require them to match the declared raster
    // type. SVG has no binary signature (it's XML text), so we instead require
    // it to begin with markup; the storage layer's active-content rename
    // (svg/html → .txt) is its second line of defence.
    if declared == "image/svg+xml" {
        let head = part
            .bytes
            .strip_prefix(&[0xEF, 0xBB, 0xBF])
            .unwrap_or(&part.bytes);
        if head.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'<') {
            return json_error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "SVG upload does not look like SVG/XML markup",
            );
        }
    } else if sniff_raster_image(&part.bytes) != Some(declared.as_str()) {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "uploaded file content does not match its declared image type",
        );
    }

    // Resolve the storage backend through the ambient seam. None → the app
    // has no StoragePlugin / set_storage; report it instead of panicking.
    let Some(storage) = umbral::storage::storage_opt() else {
        return json_error(
            StatusCode::CONFLICT,
            "image upload requires a storage backend — add StoragePlugin",
        );
    };

    let filename = part
        .filename
        .as_deref()
        .filter(|f| !f.is_empty())
        .unwrap_or("upload.png");

    match storage.store(filename, &declared, &part.bytes).await {
        Ok(stored) => {
            let body = serde_json::json!({ "url": stored.url }).to_string();
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "admin: editor image upload failed to store");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to store image")
        }
    }
}
