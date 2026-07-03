//! `multipart/form-data` parsing and the storage-merge upload helper.
//!
//! ## What this is
//!
//! A browser that POSTs a form containing a `<input type="file">` sends a
//! `multipart/form-data` body, not the `application/x-www-form-urlencoded`
//! body the rest of the form layer ([`crate::forms`], the admin's
//! `serde_urlencoded` path) understands. This module turns that multipart
//! body into the *same* flat `Vec<(String, String)>` shape the urlencoded
//! path yields — text fields stay as `(name, value)` pairs, and each
//! uploaded file is stored through the ambient [`Storage`] backend and
//! reduced to a `(field_name, stored_key)` pair. A consumer (the admin's
//! `create` / `update` handlers, wired in a later wave) can then feed the
//! result to the ORM identically whether the body was urlencoded or
//! multipart.
//!
//! ## Layering
//!
//! Two layers, so each is independently testable:
//!
//! 1. [`parse_multipart`] — pure parsing. No storage, no I/O beyond reading
//!    the in-memory body. Returns a [`MultipartForm`] separating text
//!    [`MultipartForm::fields`] from binary [`MultipartForm::files`].
//! 2. [`parse_and_store_multipart`] — parse, then [`Storage::store`] every
//!    non-empty file part and flatten everything to `Vec<(String, String)>`.
//!
//! [`Storage`]: crate::storage::Storage
//! [`Storage::store`]: crate::storage::Storage::store

use std::convert::Infallible;

use crate::storage::StorageError;

/// Default in-memory cap [`parse_multipart`] enforces on a multipart body, in
/// bytes (**32 MiB**). Matches the framework-wide request-body limit
/// `App::build` installs, so the multipart parser is a defence-in-depth
/// backstop: even a caller that reads the raw body itself (the admin's upload
/// handlers) and hands it here can't buffer an unbounded body. Call
/// [`parse_multipart_capped`] to pick a different ceiling.
pub const DEFAULT_MAX_MULTIPART_BYTES: usize = 32 * 1024 * 1024;

/// One uploaded file part of a `multipart/form-data` body.
///
/// A multipart part is treated as a *file* iff multer reports a
/// `Content-Disposition` `filename` for it; a part with no filename is a
/// plain text field and lands in [`MultipartForm::fields`] instead. The
/// raw [`bytes`](FilePart::bytes) are kept verbatim — never lossy-decoded —
/// so binary uploads (images, PDFs) round-trip intact.
#[derive(Clone, Debug)]
pub struct FilePart {
    /// The form field name (the `<input name="...">`).
    pub field_name: String,
    /// The client-supplied filename from the `Content-Disposition` header,
    /// if any. Used to derive the storage key and as a content-type hint.
    pub filename: Option<String>,
    /// The part's declared `Content-Type`, if the client sent one.
    pub content_type: Option<String>,
    /// The raw file bytes, exactly as received.
    pub bytes: Vec<u8>,
}

/// A parsed `multipart/form-data` body: text fields and file parts.
///
/// [`fields`](MultipartForm::fields) preserves both order and repeats — a
/// multi-select / M2M widget sends the same field name multiple times and
/// every value has to survive — so it is a `Vec`, not a map.
#[derive(Debug, Default)]
pub struct MultipartForm {
    /// The non-file text parts, as `(name, value)` pairs, in body order,
    /// with repeats preserved.
    pub fields: Vec<(String, String)>,
    /// The uploaded file parts (those with a `filename`), in body order.
    pub files: Vec<FilePart>,
}

impl MultipartForm {
    /// The value of the text field `name`, last-wins if it repeats.
    ///
    /// Returns `None` if no text field by that name was sent. (File parts
    /// are not considered; look in [`files`](MultipartForm::files) for
    /// those.)
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Iterate over every text field as `(&name, &value)`, in body order,
    /// including repeats.
    pub fn iter_fields(&self) -> impl Iterator<Item = (&str, &str)> {
        self.fields.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// Errors [`parse_multipart`] can return.
#[derive(Debug)]
pub enum MultipartError {
    /// The `Content-Type` header had no `boundary` parameter, so the body
    /// can't be split into parts.
    MissingBoundary,
    /// The underlying multipart parser rejected the body (malformed part
    /// headers, truncated body, etc.). Carries multer's message.
    Parse(String),
    /// A part (or the whole body) exceeded the configured size cap.
    ///
    /// Produced by [`parse_multipart`] (using [`DEFAULT_MAX_MULTIPART_BYTES`])
    /// and [`parse_multipart_capped`] (using the caller's ceiling) when the
    /// body up front, or the running total of decoded parts, exceeds the cap.
    TooLarge {
        /// The configured limit, in bytes.
        limit: usize,
        /// The actual size that was rejected, in bytes.
        actual: usize,
    },
}

impl std::fmt::Display for MultipartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MultipartError::MissingBoundary => {
                write!(f, "multipart: Content-Type has no boundary parameter")
            }
            MultipartError::Parse(s) => write!(f, "multipart: parse error: {s}"),
            MultipartError::TooLarge { limit, actual } => write!(
                f,
                "multipart: body {actual}B exceeds configured cap of {limit}B"
            ),
        }
    }
}

impl std::error::Error for MultipartError {}

/// Errors [`parse_and_store_multipart`] can return: a parse failure, a
/// storage failure, or the absence of a registered storage backend.
#[derive(Debug)]
pub enum MultipartUploadError {
    /// Parsing the multipart body failed. See [`MultipartError`].
    Multipart(MultipartError),
    /// Storing an uploaded file through the [`Storage`] backend failed.
    ///
    /// [`Storage`]: crate::storage::Storage
    Storage(StorageError),
    /// No [`Storage`] backend was registered, but the body carried a file
    /// part that needed storing.
    ///
    /// A stray multipart POST against a server with no media backend lands
    /// here rather than panicking the worker; the boot-time system check
    /// (Wave 2) is what guarantees a backend exists whenever a model
    /// declares a file field.
    ///
    /// [`Storage`]: crate::storage::Storage
    NoStorageBackend,
}

impl std::fmt::Display for MultipartUploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MultipartUploadError::Multipart(e) => write!(f, "{e}"),
            MultipartUploadError::Storage(e) => write!(f, "{e}"),
            MultipartUploadError::NoStorageBackend => write!(
                f,
                "multipart upload: no Storage backend registered; add StoragePlugin \
                 or call umbral::storage::set_storage"
            ),
        }
    }
}

impl std::error::Error for MultipartUploadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MultipartUploadError::Multipart(e) => Some(e),
            MultipartUploadError::Storage(e) => Some(e),
            MultipartUploadError::NoStorageBackend => None,
        }
    }
}

impl From<MultipartError> for MultipartUploadError {
    fn from(e: MultipartError) -> Self {
        MultipartUploadError::Multipart(e)
    }
}

impl From<StorageError> for MultipartUploadError {
    fn from(e: StorageError) -> Self {
        MultipartUploadError::Storage(e)
    }
}

/// Whether a `Content-Type` header value denotes a `multipart/form-data`
/// body.
///
/// True when the header (ignoring leading whitespace) starts with
/// `multipart/form-data`; the trailing `; boundary=...` parameter is
/// ignored here and parsed later by [`parse_multipart`].
pub fn is_multipart(content_type: &str) -> bool {
    content_type
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
}

/// Parse a `multipart/form-data` body into text fields and file parts.
///
/// `content_type_header` is the full `Content-Type` header value (it must
/// carry the `boundary=...` parameter). `body` is the complete request body
/// in memory.
///
/// Text parts (no `filename`) land in [`MultipartForm::fields`] preserving
/// order and repeats; parts with a `filename` land in
/// [`MultipartForm::files`] as [`FilePart`]s with their bytes kept verbatim.
///
/// # Errors
///
/// - [`MultipartError::MissingBoundary`] if the header has no boundary.
/// - [`MultipartError::Parse`] on a malformed body.
pub async fn parse_multipart(
    content_type_header: &str,
    body: impl Into<bytes::Bytes>,
) -> Result<MultipartForm, MultipartError> {
    parse_multipart_capped(content_type_header, body, DEFAULT_MAX_MULTIPART_BYTES).await
}

/// Like [`parse_multipart`], but with a caller-chosen in-memory size cap
/// (`max_bytes`) instead of the [`DEFAULT_MAX_MULTIPART_BYTES`] default.
///
/// The cap is enforced two ways (audit_2 core-web H11 — wiring the previously
/// dead [`MultipartError::TooLarge`]): the whole body is rejected up front if
/// it already exceeds `max_bytes`, and the running total of decoded part bytes
/// is checked as each part is read, so parsing stops the instant the sum
/// crosses the ceiling instead of buffering an unbounded body. Pass
/// `usize::MAX` to opt out of the cap.
///
/// # Errors
///
/// - [`MultipartError::MissingBoundary`] if the header has no boundary.
/// - [`MultipartError::TooLarge`] if the body / accumulated parts exceed
///   `max_bytes`.
/// - [`MultipartError::Parse`] on a malformed body.
pub async fn parse_multipart_capped(
    content_type_header: &str,
    body: impl Into<bytes::Bytes>,
    max_bytes: usize,
) -> Result<MultipartForm, MultipartError> {
    let boundary =
        multer::parse_boundary(content_type_header).map_err(|_| MultipartError::MissingBoundary)?;

    let body: bytes::Bytes = body.into();
    // Fast reject: the raw body is already in memory, and its length is an
    // upper bound on the sum of all part payloads, so a body over the cap can
    // never yield in-cap parts. Bail before spinning up the parser.
    if body.len() > max_bytes {
        return Err(MultipartError::TooLarge {
            limit: max_bytes,
            actual: body.len(),
        });
    }
    // multer's constructor wants a Bytes stream; the whole body is already
    // in memory, so a single-chunk, never-erroring stream is enough.
    let stream = futures_util::stream::once(async move { Ok::<_, Infallible>(body) });
    let mut multipart = multer::Multipart::new(stream, boundary);

    let mut form = MultipartForm::default();
    // Running total of decoded part payload bytes, checked against the cap as
    // each part lands so buffering stops the moment the sum crosses it.
    let mut accumulated: usize = 0;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| MultipartError::Parse(e.to_string()))?
    {
        // Capture all metadata BEFORE reading the body: multer's `bytes()`
        // / `text()` consume the field handle, after which name/filename/
        // content_type are gone.
        let field_name = field.name().map(str::to_owned).unwrap_or_default();
        let filename = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(|m| m.to_string());

        if filename.is_some() {
            // A part with a filename is a file: keep raw bytes, never decode.
            let bytes = field
                .bytes()
                .await
                .map_err(|e| MultipartError::Parse(e.to_string()))?;
            accumulated = accumulated.saturating_add(bytes.len());
            if accumulated > max_bytes {
                return Err(MultipartError::TooLarge {
                    limit: max_bytes,
                    actual: accumulated,
                });
            }
            form.files.push(FilePart {
                field_name,
                filename,
                content_type,
                bytes: bytes.to_vec(),
            });
        } else {
            // A part with no filename is a plain text field.
            let value = field
                .text()
                .await
                .map_err(|e| MultipartError::Parse(e.to_string()))?;
            accumulated = accumulated.saturating_add(value.len());
            if accumulated > max_bytes {
                return Err(MultipartError::TooLarge {
                    limit: max_bytes,
                    actual: accumulated,
                });
            }
            form.fields.push((field_name, value));
        }
    }

    Ok(form)
}

/// Parse a `multipart/form-data` body, store its file parts, and return a
/// flat `Vec<(String, String)>` of every field — text values plus the
/// storage key of each uploaded file.
///
/// This is the upload entry point a handler calls instead of
/// `serde_urlencoded::from_str::<Vec<(String, String)>>` when the body is
/// multipart: the return shape is identical, so the rest of the form
/// pipeline doesn't care which encoding arrived.
///
/// Each non-empty [`FilePart`] is stored via the ambient [`Storage`]
/// backend and contributes one `(field_name, stored_key)` pair, using the
/// part's `filename` (falling back to the field name) and its
/// `content_type` (falling back to `application/octet-stream`).
///
/// ## Empty file parts are skipped — "keep current file on edit"
///
/// When a user edits a record with a file field but does *not* choose a new
/// file, the browser still sends the file part — with an empty body. Such a
/// part is **skipped entirely**: no pair is emitted for it. This is
/// deliberate. Emitting `(field, "")` would overwrite the stored key with
/// an empty string and lose the existing file; omitting the pair leaves the
/// current value untouched downstream.
///
/// # Errors
///
/// - [`MultipartUploadError::Multipart`] on a parse failure.
/// - [`MultipartUploadError::NoStorageBackend`] if a file needs storing but
///   no backend is registered (returned, never panicked).
/// - [`MultipartUploadError::Storage`] if the backend's `store` fails.
///
/// [`Storage`]: crate::storage::Storage
pub async fn parse_and_store_multipart(
    content_type_header: &str,
    body: impl Into<bytes::Bytes>,
) -> Result<Vec<(String, String)>, MultipartUploadError> {
    let form = parse_multipart(content_type_header, body).await?;

    let mut pairs: Vec<(String, String)> = Vec::new();

    for file in &form.files {
        // Skip empty file parts: the user submitted the edit form without
        // choosing a new file, so leave the existing stored value alone.
        if file.bytes.is_empty() {
            continue;
        }

        // Resolve the backend lazily and only when a file actually needs
        // storing, so a multipart POST with no file (or only empty parts)
        // never trips on a missing backend.
        let backend =
            crate::storage::storage_opt().ok_or(MultipartUploadError::NoStorageBackend)?;

        let filename = file
            .filename
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&file.field_name);
        let content_type = file
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream");

        let stored = backend.store(filename, content_type, &file.bytes).await?;
        pairs.push((file.field_name.clone(), stored.key));
    }

    // Text fields always pass through, after the file keys.
    pairs.extend(form.fields);

    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDARY: &str = "X-UMBRAL-BOUNDARY";

    /// One part spec for [`build_body`]: `(name, filename, content_type,
    /// value)`. A `None` filename means a text field; `Some` means a file.
    type PartSpec<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

    /// Build a real `multipart/form-data` body from part specs. A `None`
    /// filename emits a plain text field; `Some(name)` emits a file part
    /// with a `Content-Type` line.
    fn build_body(parts: &[PartSpec<'_>]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, filename, content_type, value) in parts {
            out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            match filename {
                Some(fname) => {
                    out.extend_from_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\n"
                        )
                        .as_bytes(),
                    );
                    if let Some(ct) = content_type {
                        out.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                    }
                }
                None => {
                    out.extend_from_slice(
                        format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
                    );
                }
            }
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(value);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        out
    }

    fn ct_header() -> String {
        format!("multipart/form-data; boundary={BOUNDARY}")
    }

    #[test]
    fn is_multipart_matches_form_data_content_types() {
        assert!(is_multipart("multipart/form-data; boundary=abc"));
        assert!(is_multipart("multipart/form-data"));
        assert!(is_multipart("  Multipart/Form-Data; boundary=Z")); // case + leading ws
        assert!(!is_multipart("application/x-www-form-urlencoded"));
        assert!(!is_multipart("application/json"));
        assert!(!is_multipart("multipart/mixed; boundary=abc"));
    }

    #[tokio::test]
    async fn parse_separates_text_and_file_parts() {
        let png = b"\x89PNG\r\n\x1a\nfake-image-bytes";
        let body = build_body(&[
            ("title", None, None, b"Hello"),
            ("cover", Some("p.png"), Some("image/png"), png),
        ]);

        let form = parse_multipart(&ct_header(), body).await.unwrap();

        assert_eq!(
            form.fields,
            vec![("title".to_string(), "Hello".to_string())]
        );
        assert_eq!(form.files.len(), 1);
        let file = &form.files[0];
        assert_eq!(file.field_name, "cover");
        assert_eq!(file.filename.as_deref(), Some("p.png"));
        assert_eq!(file.content_type.as_deref(), Some("image/png"));
        assert_eq!(file.bytes, png);
    }

    #[tokio::test]
    async fn parse_preserves_repeated_text_field_names() {
        let body = build_body(&[
            ("tags", None, None, b"red"),
            ("tags", None, None, b"blue"),
            ("name", None, None, b"shirt"),
        ]);

        let form = parse_multipart(&ct_header(), body).await.unwrap();

        // Both `tags` survive, in order — M2M / multi-select correctness.
        assert_eq!(
            form.fields,
            vec![
                ("tags".to_string(), "red".to_string()),
                ("tags".to_string(), "blue".to_string()),
                ("name".to_string(), "shirt".to_string()),
            ]
        );
        // field() is last-wins.
        assert_eq!(form.field("tags"), Some("blue"));
        assert_eq!(form.field("name"), Some("shirt"));
        assert_eq!(form.field("missing"), None);
        // iter_fields yields every entry including the repeat.
        assert_eq!(form.iter_fields().filter(|(k, _)| *k == "tags").count(), 2);
    }

    #[tokio::test]
    async fn parse_keeps_binary_bytes_intact() {
        // Non-UTF8 bytes: 0xFF / 0x80 are invalid UTF-8 and must not be
        // decoded. (Built at runtime, not a const literal — clippy
        // const-folds a literal `from_utf8` and warns it always errors.)
        let raw: Vec<u8> = vec![0x00, 0xFF, 0xFE, 0x80, 0x01, 0x7F];
        assert!(std::str::from_utf8(&raw).is_err());
        let body = build_body(&[(
            "blob",
            Some("data.bin"),
            Some("application/octet-stream"),
            &raw,
        )]);

        let form = parse_multipart(&ct_header(), body).await.unwrap();

        assert_eq!(form.files.len(), 1);
        assert_eq!(form.files[0].bytes, raw, "raw bytes must round-trip");
    }

    #[tokio::test]
    async fn capped_rejects_body_over_the_limit() {
        // A body whose parts sum past the cap must error with TooLarge instead
        // of buffering unbounded (audit_2 core-web H11).
        let big = vec![b'x'; 4096];
        let body = build_body(&[(
            "blob",
            Some("big.bin"),
            Some("application/octet-stream"),
            &big,
        )]);
        let err = parse_multipart_capped(&ct_header(), body, 1024)
            .await
            .unwrap_err();
        match err {
            MultipartError::TooLarge { limit, actual } => {
                assert_eq!(limit, 1024);
                assert!(actual > 1024, "reports the offending size, got {actual}");
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn capped_allows_body_under_the_limit() {
        let small = b"hello";
        let body = build_body(&[(
            "blob",
            Some("s.bin"),
            Some("application/octet-stream"),
            small,
        )]);
        let form = parse_multipart_capped(&ct_header(), body, 1024)
            .await
            .unwrap();
        assert_eq!(form.files.len(), 1);
        assert_eq!(form.files[0].bytes, small);
    }

    #[tokio::test]
    async fn capped_counts_text_field_bytes_too() {
        // The cap covers text parts, not just files, so a flood of oversized
        // text fields is bounded as well.
        let big = vec![b'a'; 4096];
        let body = build_body(&[("notes", None, None, &big)]);
        let err = parse_multipart_capped(&ct_header(), body, 512)
            .await
            .unwrap_err();
        assert!(matches!(err, MultipartError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn parse_errors_on_missing_boundary() {
        let body = build_body(&[("title", None, None, b"Hi")]);
        let err = parse_multipart("multipart/form-data", body)
            .await
            .unwrap_err();
        assert!(matches!(err, MultipartError::MissingBoundary));
    }
}
