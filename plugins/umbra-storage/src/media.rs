//! Media (user-upload) storage backends, decorators, cleanup signal
//! handlers, and the `MediaFile` tracking model.
//!
//! Moved verbatim from the former `umbra-media` crate: [`FsStorage`]
//! (filesystem-backed [`Storage`]), the [`SizeLimitedStorage`] /
//! [`MediaTracking`] decorators, the file-lifecycle cleanup handlers
//! (`pre_delete` / `post_update`), [`MediaError`], and the [`MediaFile`]
//! model. The active-content guard, mid-stream size cap, and streaming
//! paths are preserved byte-for-byte.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use umbra::prelude::*;
use umbra::storage::{ByteStream, StorageError, StoredFile, cap_stream, is_cap_exceeded};

/// Filesystem-backed [`Storage`] — the local backend factored out so the
/// plugin routes file bytes through the backend-agnostic trait
/// (`umbra_core::storage::Storage`). The concrete impl `umbra-core`
/// deliberately doesn't name; the trait lives in core, the filesystem
/// impl lives here in the plugin (dependency inversion, see `CLAUDE.md`).
///
/// `store` writes `<dir>/<key>` where the key is `<uuid>-<sanitised
/// filename>`; the UUID guarantees uniqueness without serialising on a
/// counter and the trailing filename keeps URLs human-readable. `put`
/// writes at the EXACT key the caller supplies (static-asset collection),
/// `exists` is a filesystem stat. `url` returns `<mount>/<key>` by
/// default, or `<public_base><mount>/<key>` when an absolute public base
/// (scheme + host, e.g. `http://localhost:8100`) has been configured.
#[derive(Debug, Clone)]
pub struct FsStorage {
    dir: PathBuf,
    mount: String,
    /// Optional absolute public base (scheme + host, no trailing slash),
    /// e.g. `http://localhost:8100`. When set, [`FsStorage::url`] returns
    /// a fully-qualified URL so a deploy can hand clients an absolute
    /// link; `None` keeps the relative `<mount>/<key>` form.
    public_base: Option<String>,
}

impl FsStorage {
    /// Build a filesystem backend serving `dir` under URL prefix `mount`.
    pub fn new(mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            mount: mount.into(),
            public_base: None,
        }
    }

    /// Set the absolute public base (scheme + host like
    /// `http://localhost:8100`). Any trailing slash is trimmed so the
    /// join with the (leading-slash) mount never double-slashes.
    pub fn with_public_base(mut self, base: impl Into<String>) -> Self {
        self.public_base = Some(base.into().trim_end_matches('/').to_string());
        self
    }

    /// On-disk directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Mount path.
    pub fn mount(&self) -> &str {
        &self.mount
    }

    /// Resolve a key to its on-disk path under `dir`.
    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }
}

/// Extensions whose files browsers render as active content (script can
/// run). A user-uploaded file with one of these served inline is a stored
/// XSS vector (WEB-4), so [`neutralise_active_content`] defangs them.
const ACTIVE_CONTENT_EXTENSIONS: &[&str] = &[
    "html", "htm", "xhtml", "shtml", "xml", "svg", "svgz", "js", "mjs", "mhtml", "htc", "vbs",
];

/// If `name`'s extension is active content (`.html`, `.svg`, `.js`, …),
/// append `.txt` so the served file is inert `text/plain` instead of
/// executable markup. Returns `name` unchanged otherwise. Case-insensitive.
fn neutralise_active_content(name: &str) -> String {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ACTIVE_CONTENT_EXTENSIONS.contains(&ext.as_str()) {
        tracing::warn!(
            filename = %name,
            "umbra-storage: stored an active-content upload as `.txt` to prevent inline \
             execution (stored-XSS defence, WEB-4)"
        );
        format!("{name}.txt")
    } else {
        name.to_string()
    }
}

/// The widget tags the `#[derive(Model)]` macro assigns to file columns:
/// `FileField` → `"file"`, `ImageField` → `"image"`. Used to detect file
/// columns from a model's metadata for cleanup.
const FILE_WIDGETS: &[&str] = &["file", "image"];

/// File-key column names on model `M` — every column whose `#[derive(Model)]`
/// widget is `"file"` / `"image"` (i.e. a `FileField` / `ImageField`).
pub(crate) fn file_columns_of<M: Model>() -> Vec<String> {
    M::FIELDS
        .iter()
        .filter(|f| f.widget.is_some_and(|w| FILE_WIDGETS.contains(&w)))
        .map(|f| f.name.to_string())
        .collect()
}

/// Best-effort blob delete for one storage key. A missing blob
/// ([`StorageError::NotFound`]) is success. Any other backend error is
/// `tracing::warn!`-logged and swallowed: file cleanup must never fail the
/// row delete that triggered it.
async fn delete_blob_best_effort(storage: &Arc<dyn Storage>, key: &str) {
    if key.is_empty() {
        return;
    }
    match storage.delete(key).await {
        Ok(()) => {
            tracing::debug!(key = %key, "umbra-storage: deleted orphaned blob on row delete");
        }
        Err(StorageError::NotFound) => {
            // Already absent — the desired end state. Not an error.
        }
        Err(e) => {
            tracing::warn!(
                key = %key,
                "umbra-storage: failed to delete blob on row delete (orphan may remain): {e}"
            );
        }
    }
}

/// One model's file-lifecycle cleanup registration: the table whose row
/// deletes should cascade to blob deletes, and the file-key columns on
/// that table to read the keys from.
#[derive(Clone, Debug)]
pub(crate) struct CleanupSpec {
    /// `Model::TABLE` — used to subscribe to `pre_delete:<table>`.
    pub(crate) table: String,
    /// File-key column names (`FileField` / `ImageField`) on the table.
    pub(crate) columns: Vec<String>,
}

/// Register the file-lifecycle cleanup signal handlers for one model:
///
/// 1. **`pre_delete:<table>`** — deletes the blobs behind `columns` on the
///    row about to be deleted (Django's `FileField` delete cleanup).
/// 2. **`post_update:<table>`** — replace-cleanup (gaps2 #92): when a file
///    column changes from an old key to a new one, the OLD blob is deleted
///    so the backend doesn't accumulate orphans on file replace.
pub(crate) fn register_cleanup(spec: &CleanupSpec) {
    register_delete_cleanup(spec);
    register_replace_cleanup(spec);
}

/// Resolve the ambient storage backend and delete every key in `keys`
/// (best-effort). Shared by the delete and replace cleanup handlers.
async fn delete_keys_best_effort(keys: Vec<String>, context: &str) {
    if keys.is_empty() {
        return;
    }
    let Some(storage) = umbra::storage::storage_opt() else {
        tracing::warn!(
            "umbra-storage: {context} produced file keys but no storage backend \
             registered; blobs not cleaned up"
        );
        return;
    };
    for key in keys {
        delete_blob_best_effort(&storage, &key).await;
    }
}

/// `pre_delete:<table>` handler — deletes the blobs behind `columns` on the
/// row about to be deleted. Reads keys from the payload's `"instance"` JSON.
fn register_delete_cleanup(spec: &CleanupSpec) {
    let columns = spec.columns.clone();
    let signal = format!("pre_delete:{}", spec.table);
    umbra::signals::subscribe_async(&signal, move |payload| {
        let instance = &payload["instance"];
        let keys: Vec<String> = columns
            .iter()
            .filter_map(|col| instance.get(col).and_then(|v| v.as_str()))
            .filter(|k| !k.is_empty())
            .map(|k| k.to_string())
            .collect();
        async move { delete_keys_best_effort(keys, "row delete").await }
    });
}

/// `post_update:<table>` handler — replace-cleanup (gaps2 #92). For each
/// file column, compares `previous[col]` vs `instance[col]`; when they
/// differ and the OLD key is non-empty, deletes the old blob.
fn register_replace_cleanup(spec: &CleanupSpec) {
    let columns = spec.columns.clone();
    let signal = format!("post_update:{}", spec.table);
    umbra::signals::subscribe_async(&signal, move |payload| {
        let previous = &payload["previous"];
        let instance = &payload["instance"];
        let keys: Vec<String> = columns
            .iter()
            .filter_map(|col| {
                let old = previous.get(col).and_then(|v| v.as_str()).unwrap_or("");
                let new = instance.get(col).and_then(|v| v.as_str()).unwrap_or("");
                if !old.is_empty() && old != new {
                    Some(old.to_string())
                } else {
                    None
                }
            })
            .collect();
        async move { delete_keys_best_effort(keys, "file replace").await }
    });
}

#[umbra::storage::async_trait]
impl Storage for FsStorage {
    async fn store(
        &self,
        filename: &str,
        _content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        // Sanitise the filename: drop any path separators or NUL bytes a
        // malicious client might submit so we never escape `dir`.
        let safe_name: String = filename
            .chars()
            .filter(|c| !matches!(c, '/' | '\\' | '\0'))
            .take(120)
            .collect();
        // WEB-4: neutralise active-content extensions. The serving layer
        // (ServeDir) derives Content-Type from the on-disk extension, so a
        // stored `x.html` / `x.svg` would be served as `text/html` /
        // `image/svg+xml` and RENDERED INLINE — running attacker script on
        // the app's origin (stored XSS), since these uploads are arbitrary
        // bytes from untrusted clients. Appending `.txt` makes the file
        // serve as inert `text/plain`; the bytes are preserved and still
        // retrievable. Images/docs are untouched and still render normally.
        let safe_name = neutralise_active_content(&safe_name);
        let key = format!("{}-{safe_name}", uuid::Uuid::new_v4());
        let path = self.path_for(&key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, bytes).await?;
        let url = self.url(&key);
        Ok(StoredFile {
            key,
            url,
            size: bytes.len() as u64,
        })
    }

    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        match tokio::fs::read(self.path_for(key)).await {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StorageError::NotFound),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    /// True-streaming store: write `body` to disk chunk-by-chunk via
    /// `tokio::io::copy` over a `StreamReader`, never buffering the whole
    /// payload. Applies the SAME `safe_name` sanitise + active-content
    /// neutralisation as [`store`](Storage::store).
    ///
    /// On any error mid-write the partial file is removed (best-effort) so
    /// a rejected/aborted upload never leaves an oversized or truncated
    /// blob on disk — the key contract the `SizeLimitedStorage` cap relies
    /// on.
    async fn store_stream(
        &self,
        filename: &str,
        _content_type: &str,
        body: ByteStream,
    ) -> Result<StoredFile, StorageError> {
        let safe_name: String = filename
            .chars()
            .filter(|c| !matches!(c, '/' | '\\' | '\0'))
            .take(120)
            .collect();
        let safe_name = neutralise_active_content(&safe_name);
        let key = format!("{}-{safe_name}", uuid::Uuid::new_v4());
        let path = self.path_for(&key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let reader = tokio_util::io::StreamReader::new(body);
        let mut reader = std::pin::pin!(reader);
        let mut file = tokio::fs::File::create(&path).await?;
        let written = match tokio::io::copy(&mut reader, &mut file).await {
            Ok(n) => {
                use tokio::io::AsyncWriteExt;
                if let Err(e) = file.flush().await {
                    drop(file);
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(StorageError::Io(e));
                }
                n
            }
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                return Err(StorageError::Io(e));
            }
        };

        let url = self.url(&key);
        Ok(StoredFile {
            key,
            url,
            size: written,
        })
    }

    /// True-streaming retrieve: stream the file off disk via `ReaderStream`,
    /// never loading the whole blob into memory. Maps a missing key to
    /// [`StorageError::NotFound`].
    async fn retrieve_stream(&self, key: &str) -> Result<ByteStream, StorageError> {
        let file = match tokio::fs::File::open(self.path_for(key)).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StorageError::NotFound);
            }
            Err(e) => return Err(StorageError::Io(e)),
        };
        let stream = tokio_util::io::ReaderStream::new(file);
        Ok(Box::pin(stream))
    }

    /// Write `bytes` at the EXACT `key` — the deterministic-path sibling of
    /// [`store`](Storage::store). Static-asset collection needs this: a CSS
    /// file collected to `css/app.css` must land at that key. Creates
    /// parent dirs and overwrites, so a re-collect is idempotent.
    async fn put(
        &self,
        key: &str,
        _content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, bytes).await?;
        let url = self.url(key);
        Ok(StoredFile {
            key: key.to_string(),
            url,
            size: bytes.len() as u64,
        })
    }

    /// Cheap presence check via a filesystem stat — never reads the blob.
    async fn exists(&self, key: &str) -> Result<bool, StorageError> {
        match tokio::fs::metadata(self.path_for(key)).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        match tokio::fs::remove_file(self.path_for(key)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StorageError::NotFound),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    fn url(&self, key: &str) -> String {
        let mount = self.mount.trim_end_matches('/');
        match &self.public_base {
            Some(base) => format!("{base}{mount}/{key}"),
            None => format!("{mount}/{key}"),
        }
    }
}

/// A [`Storage`] decorator that records every successful `store` in the
/// `media_file` table, so the admin's MediaFile changelist is populated
/// no matter which entry point performed the upload.
pub struct MediaTracking {
    inner: Arc<dyn Storage>,
}

impl MediaTracking {
    /// Wrap `inner` so every successful `store` records a `media_file` row.
    pub fn new(inner: Arc<dyn Storage>) -> Self {
        Self { inner }
    }
}

/// Storage decorator enforcing the plugin's upload-size cap on every
/// ambient upload path, including admin/form multipart handling.
pub(crate) struct SizeLimitedStorage {
    inner: Arc<dyn Storage>,
    max_size: u64,
}

impl SizeLimitedStorage {
    pub(crate) fn new(inner: Arc<dyn Storage>, max_size: u64) -> Self {
        Self { inner, max_size }
    }
}

#[umbra::storage::async_trait]
impl Storage for SizeLimitedStorage {
    async fn store(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        if bytes.len() as u64 > self.max_size {
            return Err(StorageError::TooLarge {
                limit: self.max_size,
                actual: bytes.len() as u64,
            });
        }
        self.inner.store(filename, content_type, bytes).await
    }

    /// **The load-bearing streaming security change.** Wrap the incoming
    /// `body` with the mid-stream byte cap BEFORE delegating to the inner
    /// backend, so an upload is rejected the instant its real bytes cross
    /// `max_size` — even when it lies about or omits its `Content-Length`.
    async fn store_stream(
        &self,
        filename: &str,
        content_type: &str,
        body: ByteStream,
    ) -> Result<StoredFile, StorageError> {
        let capped = cap_stream(body, self.max_size);
        match self.inner.store_stream(filename, content_type, capped).await {
            Ok(stored) => Ok(stored),
            Err(StorageError::Io(e)) if is_cap_exceeded(&e) => Err(StorageError::TooLarge {
                limit: self.max_size,
                actual: self.max_size.saturating_add(1),
            }),
            Err(other) => Err(other),
        }
    }

    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.retrieve(key).await
    }

    async fn retrieve_stream(&self, key: &str) -> Result<ByteStream, StorageError> {
        self.inner.retrieve_stream(key).await
    }

    async fn put(
        &self,
        key: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        self.inner.put(key, content_type, bytes).await
    }

    async fn exists(&self, key: &str) -> Result<bool, StorageError> {
        self.inner.exists(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }

    fn url(&self, key: &str) -> String {
        self.inner.url(key)
    }
}

#[umbra::storage::async_trait]
impl Storage for MediaTracking {
    async fn store(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        let stored = self.inner.store(filename, content_type, bytes).await?;
        record_tracking_row(filename, content_type, &stored).await;
        Ok(stored)
    }

    async fn store_stream(
        &self,
        filename: &str,
        content_type: &str,
        body: ByteStream,
    ) -> Result<StoredFile, StorageError> {
        let stored = self.inner.store_stream(filename, content_type, body).await?;
        record_tracking_row(filename, content_type, &stored).await;
        Ok(stored)
    }

    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.retrieve(key).await
    }

    async fn retrieve_stream(&self, key: &str) -> Result<ByteStream, StorageError> {
        self.inner.retrieve_stream(key).await
    }

    async fn put(
        &self,
        key: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        self.inner.put(key, content_type, bytes).await
    }

    async fn exists(&self, key: &str) -> Result<bool, StorageError> {
        self.inner.exists(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }

    fn url(&self, key: &str) -> String {
        self.inner.url(key)
    }
}

/// Best-effort `media_file` tracking insert shared by `MediaTracking::store`
/// and `store_stream`. The `size` comes from [`StoredFile::size`] — the
/// ACTUAL byte count the backend wrote. A failure must NOT fail the upload:
/// it is logged and swallowed.
async fn record_tracking_row(filename: &str, content_type: &str, stored: &StoredFile) {
    let row = MediaFile {
        id: 0,
        key: stored.key.clone(),
        filename: filename.to_string(),
        content_type: content_type.to_string(),
        size: stored.size as i64,
        uploaded_at: chrono::Utc::now(),
    };
    if let Err(e) = MediaFile::objects().create(row).await {
        tracing::warn!(
            key = %stored.key,
            "umbra-storage: upload stored but media_file tracking insert failed: {e}"
        );
    }
}

/// `media_file` — one row per uploaded file. The admin renders this like
/// any other model so a developer can browse / delete uploads.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(display = "Media file", icon = "file")]
pub struct MediaFile {
    pub id: i64,
    /// Storage key — the relative path under the media dir.
    #[umbra(noedit, string)]
    pub key: String,
    /// Original filename the client sent.
    #[umbra(noedit, max_length = 200)]
    pub filename: String,
    /// MIME type as declared by the client.
    #[umbra(noedit, max_length = 80)]
    pub content_type: String,
    /// Size in bytes.
    #[umbra(noedit)]
    pub size: i64,
    #[umbra(noedit)]
    pub uploaded_at: chrono::DateTime<chrono::Utc>,
}

/// Result of a successful upload.
#[derive(Debug, Clone)]
pub struct MediaSaveOutcome {
    pub file: MediaFile,
    pub url: String,
}

/// Errors `save` can return.
#[derive(Debug)]
pub enum MediaError {
    /// Upload exceeded the configured `max_size`.
    TooLarge { limit: u64, actual: u64 },
    /// Local filesystem error writing the body.
    Io(std::io::Error),
    /// `media_file` row insert failed.
    Storage(String),
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaError::TooLarge { limit, actual } => write!(
                f,
                "umbra-storage: upload {actual}B exceeds configured cap of {limit}B"
            ),
            MediaError::Io(e) => write!(f, "umbra-storage: io: {e}"),
            MediaError::Storage(s) => write!(f, "umbra-storage: storage: {s}"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<StorageError> for MediaError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::TooLarge { limit, actual } => MediaError::TooLarge { limit, actual },
            StorageError::Io(io) => MediaError::Io(io),
            StorageError::NoBackend => MediaError::Storage("no storage backend registered".into()),
            StorageError::NotFound => MediaError::Storage("object not found".to_string()),
            StorageError::Backend(s) => MediaError::Storage(s),
            StorageError::Unsupported(s) => MediaError::Storage(s),
        }
    }
}

/// Persist one upload through `storage`, enforcing `max_size`, and record
/// it in `media_file`. Shared by `StoragePlugin::save`.
pub(crate) async fn save_through(
    storage: &Arc<dyn Storage>,
    max_size: Option<u64>,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<MediaSaveOutcome, MediaError> {
    if let Some(cap) = max_size {
        if bytes.len() as u64 > cap {
            return Err(MediaError::TooLarge {
                limit: cap,
                actual: bytes.len() as u64,
            });
        }
    }

    let stored = storage.store(filename, content_type, bytes).await?;

    let row = MediaFile {
        id: 0,
        key: stored.key.clone(),
        filename: filename.to_string(),
        content_type: content_type.to_string(),
        size: bytes.len() as i64,
        uploaded_at: chrono::Utc::now(),
    };
    let saved = MediaFile::objects()
        .save(row)
        .await
        .map_err(|e| MediaError::Storage(format!("media_file save failed: {e}")))?;

    Ok(MediaSaveOutcome {
        file: saved,
        url: stored.url,
    })
}

/// Streaming counterpart of [`save_through`]: persist an upload from a
/// byte-stream WITHOUT buffering, enforcing `max_size` MID-STREAM, then
/// record it in `media_file`.
pub(crate) async fn save_stream_through(
    storage: &Arc<dyn Storage>,
    max_size: Option<u64>,
    filename: &str,
    content_type: &str,
    body: ByteStream,
) -> Result<MediaSaveOutcome, MediaError> {
    let stored = match max_size {
        Some(cap) => {
            let capped = cap_stream(body, cap);
            match storage.store_stream(filename, content_type, capped).await {
                Ok(s) => s,
                Err(StorageError::Io(e)) if is_cap_exceeded(&e) => {
                    return Err(MediaError::TooLarge {
                        limit: cap,
                        actual: cap.saturating_add(1),
                    });
                }
                Err(other) => return Err(other.into()),
            }
        }
        None => storage.store_stream(filename, content_type, body).await?,
    };

    let row = MediaFile {
        id: 0,
        key: stored.key.clone(),
        filename: filename.to_string(),
        content_type: content_type.to_string(),
        size: stored.size as i64,
        uploaded_at: chrono::Utc::now(),
    };
    let saved = MediaFile::objects()
        .save(row)
        .await
        .map_err(|e| MediaError::Storage(format!("media_file save failed: {e}")))?;

    Ok(MediaSaveOutcome {
        file: saved,
        url: stored.url,
    })
}

#[cfg(test)]
mod active_content_tests {
    use super::neutralise_active_content;

    #[test]
    fn dangerous_extensions_get_neutralised() {
        for n in [
            "evil.html",
            "x.HTM",
            "a.svg",
            "p.SVG",
            "s.js",
            "m.mjs",
            "d.xhtml",
            "q.xml",
        ] {
            let out = neutralise_active_content(n);
            assert!(out.ends_with(".txt"), "{n} should be defanged, got {out}");
        }
    }

    #[test]
    fn safe_files_are_untouched() {
        for n in [
            "photo.png",
            "doc.pdf",
            "a.jpg",
            "data.csv",
            "noext",
            "archive.zip",
        ] {
            assert_eq!(neutralise_active_content(n), n, "{n} must be left as-is");
        }
    }
}
