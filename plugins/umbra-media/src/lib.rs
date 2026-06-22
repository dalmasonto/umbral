//! umbra-media — filesystem-backed media plugin.
//!
//! The user-upload counterpart to [`umbra-static`]. Static serves
//! developer-shipped assets; media serves user-supplied content
//! (avatars, attachments, generated reports). Both wrap
//! `tower-http::ServeDir` at the bottom, so the GET path is
//! identical; the difference is direction: media also accepts writes
//! through [`MediaPlugin::save`] and tracks every file in the
//! `media_file` model so the admin can list / delete what's there.
//!
//! ```ignore
//! App::builder()
//!     .plugin(MediaPlugin::new("/media", "./media"))
//!     .build()?;
//! ```
//!
//! ## What v0 ships
//!
//! - `GET /<mount>/*path` — read the file at `<dir>/<path>`.
//! - [`MediaPlugin::save`] — async helper a handler can call after
//!   parsing a multipart upload: writes the bytes under `<dir>/`
//!   with a UUID-prefixed filename, inserts a row in `media_file`,
//!   and returns the public URL.
//! - [`MediaFile`] model — `(id, key, filename, content_type,
//!   size, uploaded_at)`. Tracked by migrations, surfaces in the
//!   admin like any other model.
//!
//! ## Storage backend seam
//!
//! File bytes route through the backend-agnostic
//! [`umbra::storage::Storage`] trait. [`FsStorage`] is the v0
//! filesystem impl; [`MediaPlugin::new`] wires it by default and
//! [`MediaPlugin::with_storage`] swaps in a custom one. On boot the
//! plugin registers its backend as the ambient default
//! (`umbra::storage::set_storage`) in `on_ready`, so the future
//! `FileField` + admin resolve uploads without naming a backend.
//!
//! ## What v0 does NOT ship (deferred to v0.1+)
//!
//! - **S3-compatible backend.** The current implementation is
//!   filesystem-only. The `Storage` trait is the extraction point: an
//!   `S3Storage` impl (object-store + AWS SDK) slots in behind the same
//!   trait without touching `MediaPlugin`'s surface.
//! - **Image library.** Thumbnail generation, EXIF stripping, format
//!   detection. The model has a `content_type` column ready to drive
//!   this; the processing pipeline lands when the image library is
//!   wired (likely the `image` crate behind an optional cargo feature).
//! - **Signed URLs.** Filesystem-served files are always public via
//!   their mount URL. The S3 backend needs presigned-URL support and
//!   a config knob to choose public vs auth-required.
//! - **Virus scanning / size caps.** `save` enforces a hard cap from
//!   `MediaPlugin::max_size`; deeper content inspection is the user's
//!   job until ClamAV / yara plumbing lands.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use http::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use umbra::prelude::*;
use umbra::storage::{ByteStream, StorageError, StoredFile, cap_stream, is_cap_exceeded};

/// Filesystem-backed [`Storage`] — the v0 backend, factored out of
/// `MediaPlugin` so the plugin routes file bytes through the
/// backend-agnostic trait (`umbra_core::storage::Storage`). This is the
/// concrete impl `umbra-core` deliberately doesn't name; the trait lives
/// in core, the filesystem impl lives here in the plugin (dependency
/// inversion, see `CLAUDE.md`).
///
/// `store` writes `<dir>/<key>` where the key is `<uuid>-<sanitised
/// filename>`; the UUID guarantees uniqueness without serialising on a
/// counter and the trailing filename keeps URLs human-readable. `url`
/// returns `<mount>/<key>` by default, or
/// `<public_base><mount>/<key>` when an absolute public base
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
            "umbra-media: stored an active-content upload as `.txt` to prevent inline \
             execution (stored-XSS defence, WEB-4)"
        );
        format!("{name}.txt")
    } else {
        name.to_string()
    }
}

/// The widget tags the `#[derive(Model)]` macro assigns to file columns:
/// `FileField` → `"file"`, `ImageField` → `"image"`. Used to detect file
/// columns from a model's metadata for [`MediaPlugin::cleanup_on_delete`].
const FILE_WIDGETS: &[&str] = &["file", "image"];

/// File-key column names on model `M` — every column whose `#[derive(Model)]`
/// widget is `"file"` / `"image"` (i.e. a `FileField` / `ImageField`). Read
/// off `M::FIELDS` so the caller doesn't pass them by hand. Returns an empty
/// vec when `M` has no file columns (or all of them overrode the widget).
fn file_columns_of<M: Model>() -> Vec<String> {
    M::FIELDS
        .iter()
        .filter(|f| f.widget.is_some_and(|w| FILE_WIDGETS.contains(&w)))
        .map(|f| f.name.to_string())
        .collect()
}

/// Best-effort blob delete for one storage key. A missing blob
/// ([`StorageError::NotFound`]) is success — the orphan we wanted gone is
/// already gone. Any other backend error is `tracing::warn!`-logged and
/// swallowed: file cleanup must never fail the row delete that triggered it.
async fn delete_blob_best_effort(storage: &Arc<dyn Storage>, key: &str) {
    if key.is_empty() {
        return;
    }
    match storage.delete(key).await {
        Ok(()) => {
            tracing::debug!(key = %key, "umbra-media: deleted orphaned blob on row delete");
        }
        Err(StorageError::NotFound) => {
            // Already absent — the desired end state. Not an error.
        }
        Err(e) => {
            tracing::warn!(
                key = %key,
                "umbra-media: failed to delete blob on row delete (orphan may remain): {e}"
            );
        }
    }
}

/// Register the file-lifecycle cleanup signal handlers for one model:
///
/// 1. **`pre_delete:<table>`** — deletes the blobs behind `columns` on the
///    row about to be deleted (Django's `FileField` delete cleanup).
/// 2. **`post_update:<table>`** — replace-cleanup (gaps2 #92): when a file
///    column changes from an old key to a new one, the OLD blob is deleted
///    so the backend doesn't accumulate orphans on file replace.
///
/// Both read storage keys from the signal payload JSON, so they work by
/// table name without naming the concrete model type. Both are best-effort.
fn register_cleanup(spec: &CleanupSpec) {
    register_delete_cleanup(spec);
    register_replace_cleanup(spec);
}

/// Resolve the ambient storage backend and delete every key in `keys`
/// (best-effort). Shared by the delete and replace cleanup handlers.
async fn delete_keys_best_effort(keys: Vec<String>, context: &str) {
    if keys.is_empty() {
        return;
    }
    // Resolve the ambient backend lazily: it isn't registered until
    // `on_ready` runs, and a test may swap it.
    let Some(storage) = umbra::storage::storage_opt() else {
        tracing::warn!(
            "umbra-media: {context} produced file keys but no storage backend \
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
        // Read the keys synchronously off the payload, then move only the
        // owned `Vec<String>` into the async block — the handler closure is
        // `Fn`, so it can't move `columns` out, and the payload is a borrow.
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
/// differ and the OLD key is non-empty, deletes the old blob. Same key →
/// no delete (the file wasn't replaced); a newly-set file → old blob gone.
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
                // Only delete the old blob when the key actually changed
                // AND the old key is non-empty. Same key → file unchanged.
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
    /// neutralisation as [`store`](Storage::store) — streaming changes
    /// only *how* the bytes land, never the filename guards.
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

        // Map the stream's `io::Error` items into an `AsyncRead`, then copy
        // it to the destination file. `tokio::io::copy` pulls chunks and
        // writes them without ever holding the whole body.
        let reader = tokio_util::io::StreamReader::new(body);
        let mut reader = std::pin::pin!(reader);
        let mut file = tokio::fs::File::create(&path).await?;
        let written = match tokio::io::copy(&mut reader, &mut file).await {
            Ok(n) => {
                // Flush/close so the byte count is durable before we report it.
                use tokio::io::AsyncWriteExt;
                if let Err(e) = file.flush().await {
                    drop(file);
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(StorageError::Io(e));
                }
                n
            }
            Err(e) => {
                // Remove the partial write so an aborted/over-cap upload
                // leaves nothing behind on disk.
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
            // `base` is scheme+host with any trailing slash already
            // trimmed; `mount` starts with `/`, so the join is
            // `http://host` + `/media` + `/` + key.
            Some(base) => format!("{base}{mount}/{key}"),
            None => format!("{mount}/{key}"),
        }
    }
}

/// A [`Storage`] decorator that records every successful `store` in the
/// `media_file` table, so the admin's MediaFile changelist is populated
/// no matter which entry point performed the upload.
///
/// The admin / form upload path (`umbra::web::parse_and_store_multipart`)
/// writes through the *ambient* backend (`umbra::storage::storage()`),
/// which BYPASSES [`MediaPlugin::save`]. Without this decorator those
/// uploads would land on disk but never produce a tracking row.
/// [`MediaPlugin::on_ready`] therefore registers a `MediaTracking`
/// wrapping the real backend as the ambient default.
///
/// The insert is BEST-EFFORT: if it fails (e.g. no ambient DB pool in a
/// non-`App` context) it is logged via `tracing::warn!` and the
/// [`StoredFile`] is still returned. An upload must never fail because
/// tracking failed. `retrieve` / `delete` / `url` delegate straight to
/// the inner backend.
pub struct MediaTracking {
    inner: Arc<dyn Storage>,
}

impl MediaTracking {
    /// Wrap `inner` so every successful `store` records a `media_file`
    /// row.
    pub fn new(inner: Arc<dyn Storage>) -> Self {
        Self { inner }
    }
}

/// Storage decorator enforcing the plugin's upload-size cap on every
/// ambient upload path, including admin/form multipart handling.
struct SizeLimitedStorage {
    inner: Arc<dyn Storage>,
    max_size: u64,
}

impl SizeLimitedStorage {
    fn new(inner: Arc<dyn Storage>, max_size: u64) -> Self {
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
    /// The inner `store_stream` writes through the capped stream; the cap's
    /// over-limit marker error surfaces as `Io(..)` and is mapped here back
    /// to [`StorageError::TooLarge`]. The inner backend's partial-write
    /// cleanup (see `FsStorage::store_stream`) ensures no oversized blob is
    /// left on disk.
    async fn store_stream(
        &self,
        filename: &str,
        content_type: &str,
        body: ByteStream,
    ) -> Result<StoredFile, StorageError> {
        let capped = cap_stream(body, self.max_size);
        match self.inner.store_stream(filename, content_type, capped).await {
            Ok(stored) => Ok(stored),
            // The inner write aborted because the cap tripped mid-stream:
            // report it as TooLarge, not a generic IO error. `actual` is at
            // least max_size + 1 (the cap fires the moment we cross max), but
            // we don't know the true total since we cut the stream off; report
            // the cap as the actual floor.
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
        // Persist the bytes first; the row only makes sense once the
        // object actually exists.
        let stored = self.inner.store(filename, content_type, bytes).await?;
        record_tracking_row(filename, content_type, &stored).await;
        Ok(stored)
    }

    /// Streaming counterpart: delegate to the inner backend's
    /// `store_stream` (which true-streams to disk and reports the actual
    /// written `size`), then record the tracking row from that real byte
    /// count — best-effort, exactly like the buffered `store`.
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

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }

    fn url(&self, key: &str) -> String {
        self.inner.url(key)
    }
}

/// Best-effort `media_file` tracking insert shared by `MediaTracking::store`
/// and `store_stream`. The `size` comes from [`StoredFile::size`] — the
/// ACTUAL byte count the backend wrote, which for a stream is the only
/// trustworthy length. A failure (no ambient pool, transient DB error) must
/// NOT fail the upload: it is logged and swallowed.
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
            "umbra-media: upload stored but media_file tracking insert failed: {e}"
        );
    }
}

/// `media_file` — one row per uploaded file. The admin renders this
/// like any other model so a developer can browse / delete uploads.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(display = "Media file", icon = "file")]
pub struct MediaFile {
    pub id: i64,
    /// Storage key — the relative path under the media dir.
    /// `<uuid>-<original-filename>` to avoid collisions while keeping
    /// a hint of the source name in the URL.
    #[umbra(noedit, string)]
    pub key: String,
    /// Original filename the client sent. Stored as metadata for
    /// download Content-Disposition headers and human display in
    /// the admin.
    #[umbra(noedit, max_length = 200)]
    pub filename: String,
    /// MIME type as declared by the client. The plugin doesn't
    /// verify — the user-facing handler should validate against an
    /// allow-list before calling `save`.
    #[umbra(noedit, max_length = 80)]
    pub content_type: String,
    /// Size in bytes. `i64` for sqlx compatibility; v0 caps at
    /// `i64::MAX` which is well above any realistic file.
    #[umbra(noedit)]
    pub size: i64,
    #[umbra(noedit)]
    pub uploaded_at: chrono::DateTime<chrono::Utc>,
}

/// Media plugin — serves a filesystem directory + tracks uploads
/// in the `media_file` table.
///
/// The plugin routes file bytes through a [`Storage`] backend
/// ([`FsStorage`] by default). `MediaPlugin::new` is unchanged: it
/// constructs an `FsStorage` internally, so existing callers keep
/// compiling. `with_storage` is the opt-in for a custom backend (S3
/// later). On boot, [`MediaPlugin::on_ready`] registers the plugin's
/// backend as the ambient default via `umbra::storage::set_storage`,
/// which is what lets the (future) `FileField` + admin resolve uploads
/// without naming a backend.
#[derive(Clone)]
pub struct MediaPlugin {
    mount: String,
    dir: PathBuf,
    /// The backend file bytes are stored through. Defaults to
    /// [`FsStorage`]; `with_storage` swaps it.
    storage: Arc<dyn Storage>,
    /// Hard cap on upload size. `None` means "no enforced cap" — the
    /// reverse proxy or axum's own body-size guard remains the
    /// outer defence.
    max_size: Option<u64>,
    /// Models opted into file-lifecycle cleanup. Each entry names a
    /// table and the file-key columns on it that hold a [`FileField`] /
    /// [`ImageField`] storage key; on row delete the blob behind each
    /// non-empty key is removed from the backend. Populated by
    /// [`MediaPlugin::cleanup_on_delete`] / [`MediaPlugin::cleanup_files`]
    /// and consumed in [`MediaPlugin::on_ready`].
    cleanup: Vec<CleanupSpec>,
}

/// One model's file-lifecycle cleanup registration: the table whose row
/// deletes should cascade to blob deletes, and the file-key columns on
/// that table to read the keys from.
#[derive(Clone, Debug)]
struct CleanupSpec {
    /// `Model::TABLE` — used to subscribe to `pre_delete:<table>`.
    table: String,
    /// File-key column names (`FileField` / `ImageField`) on the table.
    columns: Vec<String>,
}

impl MediaPlugin {
    /// Build a plugin serving `dir` (filesystem path) under the URL
    /// prefix `mount`. Uses an [`FsStorage`] backend internally.
    pub fn new(mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        let mount = mount.into();
        let dir = dir.as_ref().to_path_buf();
        let storage: Arc<dyn Storage> = Arc::new(FsStorage::new(mount.clone(), dir.clone()));
        Self {
            mount,
            dir,
            storage,
            max_size: None,
            cleanup: Vec::new(),
        }
    }

    /// Build a plugin backed by a custom [`Storage`] (e.g. a future S3
    /// backend) mounted at `mount`. The GET-serving route still reads
    /// the filesystem `dir` (here defaulted to `mount` for the legacy
    /// `ServeDir` path); a non-filesystem backend typically serves its
    /// own URLs, so the `routes()` `ServeDir` is a no-op for keys that
    /// never hit local disk.
    pub fn with_storage(mount: impl Into<String>, storage: Arc<dyn Storage>) -> Self {
        let mount = mount.into();
        Self {
            dir: PathBuf::from(&mount),
            mount,
            storage,
            max_size: None,
            cleanup: Vec::new(),
        }
    }

    /// Test-only: wrap `inner` in the internal `SizeLimitedStorage` decorator
    /// so the mid-stream cap can be exercised in isolation, exactly as
    /// `on_ready` wires it for the ambient backend. Not public API.
    #[doc(hidden)]
    pub fn size_limited_for_test(inner: Arc<dyn Storage>, max_size: u64) -> Arc<dyn Storage> {
        Arc::new(SizeLimitedStorage::new(inner, max_size))
    }

    /// Configure an absolute public base (scheme + host like
    /// `http://localhost:8100`) for the default [`FsStorage`] backend, so
    /// resolved URLs are fully-qualified (`http://localhost:8100/media/<key>`)
    /// instead of relative (`/media/<key>`). Any trailing slash on `base`
    /// is trimmed.
    ///
    /// Only meaningful for the [`FsStorage`] built by [`MediaPlugin::new`];
    /// it rebuilds that backend with the base threaded in. A backend
    /// supplied via [`MediaPlugin::with_storage`] owns its own URL scheme,
    /// so this builder leaves a custom backend untouched.
    pub fn public_base(mut self, base: impl Into<String>) -> Self {
        let base = base.into();
        self.storage =
            Arc::new(FsStorage::new(self.mount.clone(), self.dir.clone()).with_public_base(base));
        self
    }

    /// Enforce a hard upload-size cap. [`Self::save`] rejects bytes
    /// longer than this with [`MediaError::TooLarge`].
    pub fn max_size(mut self, bytes: u64) -> Self {
        self.max_size = Some(bytes);
        self
    }

    /// Opt model `M` into **file-lifecycle cleanup**, auto-detecting its
    /// file columns. When a row of `M` is deleted via the per-row delete
    /// path (`M::objects().delete_instance(&row)`), the blob behind every
    /// non-empty [`FileField`] / [`ImageField`] key on that row is deleted
    /// from the storage backend, so the backend doesn't accumulate
    /// orphaned files — Django's `FileField` cleanup.
    ///
    /// File columns are detected from `M`'s metadata: any column the
    /// `#[derive(Model)]` macro tagged with the `file` / `image` widget
    /// (i.e. declared as `FileField` / `ImageField`). If you overrode the
    /// widget on a file column with an explicit `#[umbra(widget = "...")]`,
    /// auto-detection won't see it — name the columns explicitly with
    /// [`MediaPlugin::cleanup_files`] instead.
    ///
    /// Cleanup is **best-effort**: a storage delete error (including an
    /// already-absent blob) is `tracing::warn!`-logged and never fails the
    /// row delete. Registration is wired in [`MediaPlugin::on_ready`] via a
    /// `pre_delete:<table>` signal handler, so [`umbra_signals::SignalsPlugin`]
    /// must be registered (the ORM fires the signal regardless of plugin
    /// order; the marker plugin only documents the dependency).
    ///
    /// ```ignore
    /// App::builder()
    ///     .plugin(SignalsPlugin)
    ///     .plugin(MediaPlugin::new("/media", "./media").cleanup_on_delete::<Post>())
    ///     .build()?;
    /// ```
    ///
    /// **Replace-cleanup (gaps2 #92).** Opting in here ALSO removes the
    /// OLD blob when a file field is changed to a new key: saving a row
    /// whose `FileField` moves from key `A` to key `B` via
    /// `M::objects().save(row)` deletes blob `A`. Saving with the SAME key
    /// deletes nothing; a non-file-column update deletes nothing. This
    /// rides a `post_update:<table>` handler.
    ///
    /// **Bulk deletes/updates don't fire per-row signals.**
    /// `QuerySet::delete()` / `QuerySet::update_values()` (the filter-chain
    /// paths) fire only the `bulk_*` signals with PKs, not `pre_delete` /
    /// `post_update`, so a bulk delete/update will NOT trigger cleanup —
    /// same limitation Django's bulk `QuerySet` cascade has for the
    /// post-row hook. Use `save` / `delete_instance` per row when cleanup
    /// matters.
    pub fn cleanup_on_delete<M: Model>(mut self) -> Self {
        let columns = file_columns_of::<M>();
        if columns.is_empty() {
            tracing::warn!(
                table = M::TABLE,
                "umbra-media: cleanup_on_delete found no FileField/ImageField columns on \
                 `{}`; nothing will be cleaned up. Name the columns explicitly with \
                 `cleanup_files` if you overrode the file widget.",
                M::TABLE
            );
        } else {
            self.cleanup.push(CleanupSpec {
                table: M::TABLE.to_string(),
                columns,
            });
        }
        self
    }

    /// Opt model `M` into file-lifecycle cleanup for the named file
    /// columns explicitly. Use this when [`MediaPlugin::cleanup_on_delete`]'s
    /// widget-based auto-detection can't see a file column (e.g. you
    /// overrode the `file` / `image` widget on it). Each name must be a
    /// `FileField` / `ImageField` column on `M` holding a storage key;
    /// semantics are otherwise identical to
    /// [`MediaPlugin::cleanup_on_delete`] (best-effort, per-row delete only).
    pub fn cleanup_files<M: Model>(mut self, fields: &[&str]) -> Self {
        self.cleanup.push(CleanupSpec {
            table: M::TABLE.to_string(),
            columns: fields.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    /// Mount path.
    pub fn mount(&self) -> &str {
        &self.mount
    }

    /// On-disk directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The storage backend this plugin will register as the ambient
    /// default (before the `MediaTracking` decorator is applied in
    /// `on_ready`). Exposed so callers / tests can resolve a key's public
    /// URL the same way the registered backend will.
    pub fn storage(&self) -> &Arc<dyn Storage> {
        &self.storage
    }

    /// Persist one upload to disk and record it in `media_file`.
    /// Returns the public URL the caller can hand back to the client.
    ///
    /// The key is `<uuid>-<sanitised filename>`; the UUID guarantees
    /// uniqueness without serializing on a counter, and the trailing
    /// filename keeps URLs human-readable. The original filename is
    /// preserved separately in the row for Content-Disposition on
    /// downloads.
    pub async fn save(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<MediaSaveOutcome, MediaError> {
        if let Some(cap) = self.max_size {
            if bytes.len() as u64 > cap {
                return Err(MediaError::TooLarge {
                    limit: cap,
                    actual: bytes.len() as u64,
                });
            }
        }

        // Persist the bytes through the storage backend; it owns key
        // generation, the on-disk (or object-store) write, and the
        // public URL.
        let stored = self.storage.store(filename, content_type, bytes).await?;

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

    /// Streaming counterpart of [`save`](MediaPlugin::save): persist an
    /// upload from a byte-stream `body` WITHOUT buffering the whole payload
    /// in memory, then record it in `media_file`.
    ///
    /// **`max_size` is enforced MID-STREAM**, not from a declared length:
    /// when a cap is configured the `body` is wrapped with [`cap_stream`]
    /// so the upload is rejected the instant its real bytes cross the cap
    /// — even if the client lies about or omits its `Content-Length`. A
    /// rejected stream leaves no oversized blob on disk (the FsStorage
    /// backend cleans up its partial write).
    ///
    /// The recorded `MediaFile.size` is the ACTUAL streamed byte count
    /// ([`StoredFile::size`]), the only trustworthy length for a stream.
    ///
    /// Use [`save`](MediaPlugin::save) for small uploads where you already
    /// hold the bytes (form fields, generated content); reach for
    /// `save_stream` when the body is large or arrives as a stream (a
    /// proxied download, a multipart part) and buffering it would waste
    /// memory.
    pub async fn save_stream(
        &self,
        filename: &str,
        content_type: &str,
        body: ByteStream,
    ) -> Result<MediaSaveOutcome, MediaError> {
        // Apply the mid-stream cap here when configured: `self.storage` is
        // the raw backend (no size decorator), so the cap can't be skipped
        // by going through `save_stream` instead of the ambient
        // `SizeLimitedStorage`.
        let stored = match self.max_size {
            Some(cap) => {
                let capped = cap_stream(body, cap);
                match self.storage.store_stream(filename, content_type, capped).await {
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
            None => self.storage.store_stream(filename, content_type, body).await?,
        };

        let row = MediaFile {
            id: 0,
            key: stored.key.clone(),
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            // The ACTUAL streamed byte count — a stream has no trustworthy
            // up-front length, so this is the only correct size.
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
}

impl Plugin for MediaPlugin {
    fn name(&self) -> &'static str {
        "media"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![umbra::migrate::ModelMeta::for_::<MediaFile>()]
    }

    fn routes(&self) -> Router {
        if !self.dir.exists() {
            tracing::warn!(
                "umbra-media: directory `{}` does not exist; requests under `{}` will return 404",
                self.dir.display(),
                self.mount
            );
        }
        let mount = self.mount.trim_end_matches('/').to_string();
        let serve = ServeDir::new(&self.dir);

        let svc = tower::ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::if_not_present(
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            ))
            .service(serve);

        // `nest_service` strips the `/media` prefix before `ServeDir`
        // resolves the path, so `/media/<key>` maps to `<dir>/<key>`. A
        // plain `route("/media/{*path}", ...)` does NOT strip, leaving
        // ServeDir to look under `<dir>/media/<key>` — a guaranteed 404.
        Router::new().nest_service(&mount, svc)
    }

    /// Register this plugin's storage backend as the ambient default.
    ///
    /// This is the wiring that lets the (future) `FileField` + admin
    /// resolve uploads through `umbra::storage::storage()` without
    /// naming a backend — exactly as the DB pool is registered once at
    /// boot and read ambiently thereafter. First registration wins; a
    /// second `MediaPlugin` (or an app that pre-registered a backend via
    /// `umbra::storage::set_storage`) leaves the first in place and only
    /// warns.
    fn on_ready(&self, _ctx: &AppContext) -> Result<(), umbra::plugin::PluginError> {
        // Register the storage wrapped in a `MediaTracking` decorator so
        // the admin / form upload path (which writes through the ambient
        // backend, BYPASSING `MediaPlugin::save`) still records a
        // `media_file` row per upload. `save` keeps writing through the
        // inner `self.storage` plus its own single insert, so the two
        // entry points each record exactly one row — no double-insert.
        let storage: Arc<dyn Storage> = match self.max_size {
            Some(max_size) => Arc::new(SizeLimitedStorage::new(self.storage.clone(), max_size)),
            None => self.storage.clone(),
        };
        umbra::storage::set_storage(Arc::new(MediaTracking::new(storage)));

        // File-lifecycle cleanup: for every model opted in via
        // `cleanup_on_delete` / `cleanup_files`, subscribe a
        // `pre_delete:<table>` handler (delete the blobs on row delete) AND
        // a `post_update:<table>` handler (delete the OLD blob when a file
        // field is replaced — gaps2 #92). Best-effort — see
        // `register_cleanup`.
        for spec in &self.cleanup {
            tracing::debug!(
                table = %spec.table,
                columns = ?spec.columns,
                "umbra-media: registering file-lifecycle cleanup on delete"
            );
            register_cleanup(spec);
        }
        Ok(())
    }

    /// `MediaPlugin` registers an `FsStorage` (or the custom backend
    /// passed to `with_storage`) as the ambient default in `on_ready`,
    /// so the boot `field.storage_backend` check is satisfied for any
    /// model that declares a `FileField` / `ImageField`.
    fn provides_storage(&self) -> bool {
        true
    }
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
                "umbra-media: upload {actual}B exceeds configured cap of {limit}B"
            ),
            MediaError::Io(e) => write!(f, "umbra-media: io: {e}"),
            MediaError::Storage(s) => write!(f, "umbra-media: storage: {s}"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<StorageError> for MediaError {
    /// Map a backend [`StorageError`] onto `MediaError`'s existing
    /// variants so `save` can `?` on the storage call without growing a
    /// new public variant. `TooLarge` carries the limit/actual through;
    /// `Io` is preserved; `NoBackend` / `NotFound` / `Backend` collapse
    /// to `Storage`.
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::TooLarge { limit, actual } => MediaError::TooLarge { limit, actual },
            StorageError::Io(io) => MediaError::Io(io),
            StorageError::NoBackend => MediaError::Storage("no storage backend registered".into()),
            StorageError::NotFound => MediaError::Storage("object not found".to_string()),
            StorageError::Backend(s) => MediaError::Storage(s),
        }
    }
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
