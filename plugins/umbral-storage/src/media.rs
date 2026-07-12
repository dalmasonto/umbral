//! Media (user-upload) storage backends, decorators, cleanup signal
//! handlers, and the `MediaFile` tracking model.
//!
//! Moved verbatim from the former `umbral-media` crate: [`FsStorage`]
//! (filesystem-backed [`Storage`]), the [`SizeLimitedStorage`] /
//! [`MediaTracking`] decorators, the file-lifecycle cleanup handlers
//! (`pre_delete` / `post_update`), [`MediaError`], and the [`MediaFile`]
//! model. The active-content guard, mid-stream size cap, and streaming
//! paths are preserved byte-for-byte.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use umbral::prelude::*;
use umbral::storage::{ByteStream, StorageError, StoredFile, cap_stream, is_cap_exceeded};

/// Status string a freshly-uploaded file carries when no background work is
/// pending — the upload is immediately usable.
pub const STATUS_READY: &str = "ready";
/// Status while a background task (processors and/or the deferred write) is
/// in flight. The original is already stored for `save`, but NOT yet stored
/// for `save_deferred`.
pub const STATUS_PROCESSING: &str = "processing";
/// Status after a processor or the deferred write errored. The original
/// bytes of a `save` upload are still stored (processing failure never loses
/// the upload); a `save_deferred` failure may mean the bytes were never
/// written.
pub const STATUS_FAILED: &str = "failed";

/// A boxed, send-able error any processor can return.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// An upload **processor**: an async fn run, in registration order, over a
/// saved [`MediaFile`] after its bytes land in storage (thumbnailing,
/// transcoding, virus scan, …). Register via [`crate::StoragePlugin::on_upload`].
///
/// Boxed so the registry can hold a heterogeneous list. The future is
/// `Send + 'static` so it runs in a detached [`tokio::spawn`]; the error is
/// boxed so any `E: Error` works.
pub type Processor = Arc<
    dyn Fn(MediaFile) -> Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send>> + Send + Sync,
>;

/// The ambient processor list, installed at `on_ready` (mirrors the ambient
/// storage seam's `OnceLock<Mutex<…>>`). ANY save path — `StoragePlugin::save`,
/// `save_deferred`, the admin/form multipart upload through `MediaTracking` —
/// reads this, so background processing isn't tied to one entry point. The
/// `Mutex` (rather than a bare `OnceLock<Vec>`) makes the install replaceable,
/// matching `set_storage_named`'s set-but-overwritable shape so a test
/// process that boots more than one plugin doesn't leak one test's
/// processors into the next.
static PROCESSORS: OnceLock<Mutex<Arc<Vec<Processor>>>> = OnceLock::new();

fn processor_slot() -> &'static Mutex<Arc<Vec<Processor>>> {
    PROCESSORS.get_or_init(|| Mutex::new(Arc::new(Vec::new())))
}

/// Default cap on concurrently-running media-processing tasks (audit_2
/// plugin-storage-tasks #4). Each upload with processors spawns a detached task
/// that decodes / scans the file (CPU + memory heavy); without a bound an upload
/// burst fans out unbounded parallel processing. Override with
/// `UMBRAL_MEDIA_PROCESSING_CONCURRENCY`.
const DEFAULT_MEDIA_PROCESSING_CONCURRENCY: usize = 8;

static PROCESSING_SLOTS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();

fn processing_slots() -> &'static tokio::sync::Semaphore {
    PROCESSING_SLOTS.get_or_init(|| {
        let cap = std::env::var("UMBRAL_MEDIA_PROCESSING_CONCURRENCY")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_MEDIA_PROCESSING_CONCURRENCY);
        tokio::sync::Semaphore::new(cap)
    })
}

/// Install the processor list ambiently. Called from `on_ready`.
pub(crate) fn set_processors(list: Arc<Vec<Processor>>) {
    *processor_slot().lock().expect("processor registry mutex") = list;
}

/// The ambient processor list, or an empty list when none were registered.
pub(crate) fn processors() -> Arc<Vec<Processor>> {
    processor_slot()
        .lock()
        .expect("processor registry mutex")
        .clone()
}

/// Test-only: clear the ambient processor registry so a test process that
/// boots more than one plugin can reset to a known-empty baseline. Not
/// public API; used by the background-processing integration tests.
#[doc(hidden)]
pub fn clear_processors_for_test() {
    *processor_slot().lock().expect("processor registry mutex") = Arc::new(Vec::new());
}

/// Run every processor over `media` in order, then persist the terminal
/// status (`"ready"` on all-ok, `"failed"` on the first error) through the
/// ORM so `post_save:media_file` fires. Shared by the `save` and
/// `save_deferred` background tasks. `prelude` runs before the processors
/// (the deferred write); a `prelude` error short-circuits to `"failed"`.
async fn run_processing<F, Fut>(media: MediaFile, processors: Arc<Vec<Processor>>, prelude: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), BoxError>>,
{
    let id = media.id;
    let outcome: Result<(), BoxError> = async {
        // The deferred write (if any) runs FIRST and unbounded, so a
        // `save_deferred` frees its file bytes promptly rather than holding
        // them while queued for a processing slot.
        prelude().await?;
        // audit_2 plugin-storage-tasks #4: bound concurrent processing. The
        // permit is acquired only around the CPU/memory-heavy processor loop, so
        // an upload burst can't run unbounded parallel decodes/scans. Waiting
        // tasks hold only the (already-persisted) MediaFile row. The static
        // semaphore is never closed, so acquire never errors in practice.
        let _permit = processing_slots().acquire().await?;
        for processor in processors.iter() {
            processor(media.clone()).await?;
        }
        Ok(())
    }
    .await;

    let status = match &outcome {
        Ok(()) => STATUS_READY,
        Err(e) => {
            tracing::error!(
                media_id = id,
                "umbral-storage: background processing failed for media_file #{id}: {e}"
            );
            STATUS_FAILED
        }
    };
    persist_status(id, status).await;
}

/// Re-fetch the `media_file` row, set its `status`, and `save()` it so the
/// UPDATE fires `post_save:media_file` (the realtime tie-in). Best-effort:
/// a failure here is logged and swallowed — the bytes are already stored.
async fn persist_status(id: i64, status: &str) {
    let row = match MediaFile::objects()
        .filter(media_file::ID.eq(id))
        .first()
        .await
    {
        Ok(Some(mut row)) => {
            row.status = status.to_string();
            row
        }
        Ok(None) => {
            tracing::warn!(
                media_id = id,
                "umbral-storage: media_file #{id} vanished before status update"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(
                media_id = id,
                "umbral-storage: could not load media_file #{id} for status update: {e}"
            );
            return;
        }
    };
    if let Err(e) = MediaFile::objects().save(row).await {
        tracing::warn!(
            media_id = id,
            "umbral-storage: failed to persist media_file #{id} status={status}: {e}"
        );
    }
}

/// Filesystem-backed [`Storage`] — the local backend factored out so the
/// plugin routes file bytes through the backend-agnostic trait
/// (`umbral_core::storage::Storage`). The concrete impl `umbral-core`
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

/// Sanitise a client-supplied filename for embedding in a generated
/// storage key: strip path separators, NUL bytes, and every other control
/// character (a raw `\n`/escape in a stored name is a log-injection
/// surface — audit `plugin-storage-tasks` #12), capped at 120 chars.
/// Shared by every key-generating backend (`FsStorage`, `S3Storage`) and
/// the deferred-save key generator so the rules can't drift apart.
pub(crate) fn sanitise_filename(filename: &str) -> String {
    filename
        .chars()
        .filter(|c| !matches!(c, '/' | '\\') && !c.is_control())
        .take(120)
        .collect()
}

/// The shared stored-XSS guard for every key-generating upload path
/// (audit `plugin-storage-tasks` #1): sanitise `filename`, defang
/// active-content extensions (`evil.html` → `evil.html.txt`), and return
/// the content type the backend should record — forced to `text/plain`
/// when the name was defanged, so a backend that serves the recorded
/// client-declared type verbatim (S3/CDN) can't serve attacker HTML
/// inline as `text/html`.
pub(crate) fn neutralised_upload(filename: &str, content_type: &str) -> (String, String) {
    let safe_name = sanitise_filename(filename);
    let neutralised = neutralise_active_content(&safe_name);
    let content_type = if neutralised != safe_name {
        "text/plain".to_string()
    } else {
        content_type.to_string()
    };
    (neutralised, content_type)
}

/// Control-char-stripped copy of the client's original filename for the
/// `media_file.filename` column. The on-disk/object key is sanitised
/// separately ([`sanitise_filename`]); this guards the *retained* original
/// name, which is stored and logged (audit `plugin-storage-tasks` #12).
fn retained_filename(filename: &str) -> String {
    filename.chars().filter(|c| !c.is_control()).collect()
}

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
            "umbral-storage: stored an active-content upload as `.txt` to prevent inline \
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
            tracing::debug!(key = %key, "umbral-storage: deleted orphaned blob on row delete");
        }
        Err(StorageError::NotFound) => {
            // Already absent — the desired end state. Not an error.
        }
        Err(e) => {
            tracing::warn!(
                key = %key,
                "umbral-storage: failed to delete blob on row delete (orphan may remain): {e}"
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
///    row about to be deleted (file cleanup on row delete).
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
    let Some(storage) = umbral::storage::storage_opt() else {
        tracing::warn!(
            "umbral-storage: {context} produced file keys but no storage backend \
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
    umbral::signals::subscribe_async(&signal, move |payload| {
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
    umbral::signals::subscribe_async(&signal, move |payload| {
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

#[umbral::storage::async_trait]
impl Storage for FsStorage {
    async fn store(
        &self,
        filename: &str,
        _content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        // Sanitise the filename: drop any path separators, NUL bytes, or
        // control chars a malicious client might submit so we never
        // escape `dir` (shared helper — see `sanitise_filename`).
        let safe_name = sanitise_filename(filename);
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
        let safe_name = sanitise_filename(filename);
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

#[umbral::storage::async_trait]
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
        match self
            .inner
            .store_stream(filename, content_type, capped)
            .await
        {
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

#[umbral::storage::async_trait]
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
        let stored = self
            .inner
            .store_stream(filename, content_type, body)
            .await?;
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
    let procs = processors();
    let initial = if procs.is_empty() {
        STATUS_READY
    } else {
        STATUS_PROCESSING
    };
    let row = MediaFile {
        id: 0,
        key: stored.key.clone(),
        filename: retained_filename(filename),
        content_type: content_type.to_string(),
        size: stored.size as i64,
        uploaded_at: chrono::Utc::now(),
        status: initial.to_string(),
    };
    let saved = match MediaFile::objects().create(row).await {
        Ok(saved) => saved,
        Err(e) => {
            tracing::warn!(
                key = %stored.key,
                "umbral-storage: upload stored but media_file tracking insert failed: {e}"
            );
            return;
        }
    };
    // The ambient/admin upload path runs processors too (the registry is
    // ambient by design), so a thumbnail/scan fires for admin uploads, not
    // just `StoragePlugin::save`. Bytes are already stored; spawn detached.
    if !procs.is_empty() {
        tokio::spawn(async move {
            run_processing(saved, procs, || async { Ok(()) }).await;
        });
    }
}

/// `media_file` — one row per uploaded file. The admin renders this like
/// any other model so a developer can browse / delete uploads.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(display = "Media file", icon = "file")]
pub struct MediaFile {
    pub id: i64,
    /// Storage key — the relative path under the media dir.
    #[umbral(noedit, string)]
    pub key: String,
    /// Original filename the client sent.
    #[umbral(noedit, max_length = 200)]
    pub filename: String,
    /// MIME type as declared by the client.
    #[umbral(noedit, max_length = 80)]
    pub content_type: String,
    /// Size in bytes.
    #[umbral(noedit)]
    pub size: i64,
    #[umbral(noedit)]
    pub uploaded_at: chrono::DateTime<chrono::Utc>,
    /// Background-processing lifecycle: `"ready"` (the default — a plain
    /// upload with no processors is immediately ready), `"processing"`
    /// (a background task is running processors / a deferred write is in
    /// flight), or `"failed"` (a processor or the deferred write errored).
    ///
    /// The `#[umbral(default = "ready")]` clause makes the additive column
    /// migration safe: existing `media_file` rows backfill to `"ready"`.
    /// A status change persisted through the ORM fires `post_save:media_file`,
    /// which a developer can forward to the frontend with
    /// `RealtimePlugin::new().expose::<MediaFile>(...)` — no coupling.
    #[umbral(noedit, default = "ready", max_length = 16)]
    pub status: String,
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
                "umbral-storage: upload {actual}B exceeds configured cap of {limit}B"
            ),
            MediaError::Io(e) => write!(f, "umbral-storage: io: {e}"),
            MediaError::Storage(s) => write!(f, "umbral-storage: storage: {s}"),
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
            StorageError::UnsupportedType {
                content_type,
                allowed,
            } => MediaError::Storage(format!(
                "`{content_type}` is not an accepted upload type (accepted: {})",
                allowed.join(", ")
            )),
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
    finish_save(stored, filename, content_type).await
}

/// Shared tail of the buffered/streaming `save` paths (Mode A): the original
/// IS already in storage, so the URL works immediately. Insert the
/// `media_file` row with `status="ready"` when no processors are registered,
/// else `status="processing"` + a detached [`tokio::spawn`] that runs every
/// processor and flips the status to `"ready"`/`"failed"`. Returns the
/// outcome IMMEDIATELY — never awaits processing.
async fn finish_save(
    stored: StoredFile,
    filename: &str,
    content_type: &str,
) -> Result<MediaSaveOutcome, MediaError> {
    let procs = processors();
    let initial = if procs.is_empty() {
        STATUS_READY
    } else {
        STATUS_PROCESSING
    };
    let row = MediaFile {
        id: 0,
        key: stored.key.clone(),
        filename: retained_filename(filename),
        content_type: content_type.to_string(),
        size: stored.size as i64,
        uploaded_at: chrono::Utc::now(),
        status: initial.to_string(),
    };
    let saved = MediaFile::objects()
        .save(row)
        .await
        .map_err(|e| MediaError::Storage(format!("media_file save failed: {e}")))?;

    if !procs.is_empty() {
        let spawn_row = saved.clone();
        tokio::spawn(async move {
            run_processing(spawn_row, procs, || async { Ok(()) }).await;
        });
    }

    Ok(MediaSaveOutcome {
        file: saved,
        url: stored.url,
    })
}

/// Persist one upload at a DEFERRED time (Mode B): generate the key + URL
/// upfront via the backend's deterministic `put` key-gen, insert the
/// `media_file` row with `status="processing"` and the known size, then
/// return IMMEDIATELY. A detached [`tokio::spawn`] writes `bytes` to the
/// backend at the exact key and runs the processors; success →
/// `status="ready"`, any failure (write or processor) → `status="failed"`.
///
/// The returned URL is final/deterministic but 404s until the background
/// write finishes — the frontend shows a placeholder until the
/// `post_save:media_file` → realtime "ready" push (or a poll). For very
/// large files a future optimisation is to stage `bytes` in a temp file
/// rather than holding them in the spawned task.
pub(crate) async fn save_deferred_through(
    storage: &Arc<dyn Storage>,
    max_size: Option<u64>,
    filename: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> Result<MediaSaveOutcome, MediaError> {
    if let Some(cap) = max_size {
        if bytes.len() as u64 > cap {
            return Err(MediaError::TooLarge {
                limit: cap,
                actual: bytes.len() as u64,
            });
        }
    }

    // Same key-gen `store` uses (`<uuid>-<sanitised name>`) so the URL is
    // final the instant we return; the bytes land at this exact key later.
    // The stored-XSS guard also picks the content type the backend records
    // for the deferred `put` — `text/plain` when the name was defanged —
    // so an S3-backed deferred upload can't be served inline as HTML.
    let (safe_name, put_content_type) = neutralised_upload(filename, content_type);
    let key = format!("{}-{safe_name}", uuid::Uuid::new_v4());
    let url = storage.url(&key);

    let row = MediaFile {
        id: 0,
        key: key.clone(),
        filename: retained_filename(filename),
        content_type: content_type.to_string(),
        size: bytes.len() as i64,
        uploaded_at: chrono::Utc::now(),
        status: STATUS_PROCESSING.to_string(),
    };
    let saved = MediaFile::objects()
        .save(row)
        .await
        .map_err(|e| MediaError::Storage(format!("media_file save failed: {e}")))?;

    let procs = processors();
    let storage = storage.clone();
    let spawn_row = saved.clone();
    tokio::spawn(async move {
        // The deferred WRITE is the `prelude`: it must succeed before the
        // processors run; a write error short-circuits to `status="failed"`.
        run_processing(spawn_row, procs, move || async move {
            storage
                .put(&key, &put_content_type, &bytes)
                .await
                .map(|_| ())
                .map_err(|e| Box::new(e) as BoxError)
        })
        .await;
    });

    Ok(MediaSaveOutcome { file: saved, url })
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

    finish_save(stored, filename, content_type).await
}

#[cfg(test)]
mod active_content_tests {
    use super::{neutralise_active_content, neutralised_upload, sanitise_filename};

    /// The shared stored-XSS guard every key-generating backend applies
    /// (audit `plugin-storage-tasks` #1): active content is renamed to
    /// `.txt` AND its recorded content type forced to `text/plain`, so a
    /// backend that serves the recorded type verbatim (S3) stays inert.
    #[test]
    fn neutralised_upload_defangs_name_and_content_type() {
        for (name, declared) in [
            ("evil.html", "text/html"),
            ("payload.svg", "image/svg+xml"),
            ("worm.js", "application/javascript"),
            ("page.XHTML", "application/xhtml+xml"),
        ] {
            let (safe_name, ct) = neutralised_upload(name, declared);
            assert!(
                safe_name.ends_with(".txt"),
                "{name} must be renamed to .txt, got {safe_name}"
            );
            assert_eq!(
                ct, "text/plain",
                "{name} ({declared}) must be recorded as text/plain, got {ct}"
            );
        }
    }

    #[test]
    fn neutralised_upload_leaves_inert_uploads_alone() {
        for (name, declared) in [
            ("photo.png", "image/png"),
            ("doc.pdf", "application/pdf"),
            ("notes.txt", "text/plain"),
        ] {
            let (safe_name, ct) = neutralised_upload(name, declared);
            assert_eq!(safe_name, name, "{name} must keep its name");
            assert_eq!(ct, declared, "{name} must keep its declared type");
        }
    }

    /// A traversal-shaped filename must still be defanged AFTER the
    /// separators are stripped — `../evil.html` sanitises to `..evil.html`,
    /// which is active content and must come back `.txt` + `text/plain`.
    #[test]
    fn neutralised_upload_sanitises_before_defanging() {
        let (safe_name, ct) = neutralised_upload("../evil.html", "text/html");
        assert!(!safe_name.contains('/'), "separators stripped: {safe_name}");
        assert!(safe_name.ends_with(".txt"), "defanged: {safe_name}");
        assert_eq!(ct, "text/plain");
    }

    /// Audit `plugin-storage-tasks` #12: control characters (newlines,
    /// escapes, NUL) must not survive into a generated key.
    #[test]
    fn sanitise_filename_strips_separators_and_control_chars() {
        assert_eq!(sanitise_filename("a/b\\c.txt"), "abc.txt");
        assert_eq!(sanitise_filename("evil\nname\r.png"), "evilname.png");
        assert_eq!(sanitise_filename("nul\0byte.gif"), "nulbyte.gif");
        assert_eq!(sanitise_filename("esc\u{1b}[31m.jpg"), "esc[31m.jpg");
        // Length cap preserved from the original inline sanitiser.
        let long = "x".repeat(200);
        assert_eq!(sanitise_filename(&long).len(), 120);
    }

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

// =========================================================================
// Upload content-type policy (gaps3 #51)
// =========================================================================

/// What an upload is *actually* made of, sniffed from its leading bytes.
///
/// The client-declared `Content-Type` is worth nothing on its own: renaming
/// `evil.exe` to `avatar.png` and claiming `image/png` is a two-second attack. A
/// policy that only reads the declaration stops nobody, so the bytes get the
/// final say.
///
/// `None` means "no signature we recognise" — which for an image allow-list is a
/// rejection, not a pass.
pub fn sniff_content_type(bytes: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    const GIF87: &[u8] = b"GIF87a";
    const GIF89: &[u8] = b"GIF89a";
    const PDF: &[u8] = b"%PDF-";

    if bytes.starts_with(PNG) {
        return Some("image/png");
    }
    // JPEG: FF D8 FF
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(GIF87) || bytes.starts_with(GIF89) {
        return Some("image/gif");
    }
    // WEBP: "RIFF" .... "WEBP"
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(PDF) {
        return Some("application/pdf");
    }
    None
}

/// The MIME types the built-in image allow-list accepts.
///
/// SVG is **not** here, and that is deliberate: an SVG is a script-execution
/// vector, not a picture. `neutralise_active_content` already defangs one that
/// slips in via another route; an image allow-list should not invite it.
pub const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Storage decorator enforcing an upload allow-list on **every** save path
/// (gaps3 #51).
///
/// A decorator for the same reason the size cap is one: wrapping the ambient
/// `Storage` means the admin, form handling, REST, and any hand-written upload
/// route all inherit the policy. Enforcing it in a handler would cover the
/// handlers that existed the day it was written.
pub(crate) struct TypeLimitedStorage {
    inner: Arc<dyn Storage>,
    allowed: Vec<String>,
}

impl TypeLimitedStorage {
    pub(crate) fn new(inner: Arc<dyn Storage>, allowed: Vec<String>) -> Self {
        Self { inner, allowed }
    }

    /// Decide whether these bytes may be stored.
    ///
    /// Both the declaration AND the sniffed reality must be on the list, and they
    /// must agree. Checking only the declaration lets a renamed `.exe` through;
    /// checking only the bytes lets a caller store a PNG into a field that was
    /// supposed to take PDFs. Requiring both closes each hole.
    fn check(&self, declared: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let reject = |what: &str| {
            Err(StorageError::UnsupportedType {
                content_type: what.to_string(),
                allowed: self.allowed.clone(),
            })
        };
        // Normalise `image/png; charset=binary` down to the essence.
        let declared = declared.split(';').next().unwrap_or("").trim();
        if !self.allowed.iter().any(|a| a == declared) {
            return reject(declared);
        }
        match sniff_content_type(bytes) {
            // The bytes are something we recognise — they must be allowed AND
            // must be what the caller claimed.
            Some(actual) => {
                if !self.allowed.iter().any(|a| a == actual) || actual != declared {
                    return reject(actual);
                }
                Ok(())
            }
            // Unrecognised bytes claiming to be an allowed type: that is the
            // renamed-executable case. Refuse.
            None => reject("unrecognised"),
        }
    }
}

#[umbral::storage::async_trait]
impl Storage for TypeLimitedStorage {
    async fn store(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        self.check(content_type, bytes)?;
        self.inner.store(filename, content_type, bytes).await
    }

    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.retrieve(key).await
    }

    async fn store_stream(
        &self,
        filename: &str,
        content_type: &str,
        body: umbral::storage::ByteStream,
    ) -> Result<StoredFile, StorageError> {
        // Sniffing needs the leading bytes, so a policed stream is buffered.
        // Correctness beats streaming here: you cannot decide whether bytes are
        // acceptable without looking at them, and the size cap (applied by the
        // decorator underneath) still bounds how much gets buffered.
        use futures_util::StreamExt;
        let mut body = body;
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = body.next().await {
            buf.extend_from_slice(&chunk.map_err(StorageError::Io)?);
        }
        self.check(content_type, &buf)?;
        self.inner.store(filename, content_type, &buf).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }

    fn url(&self, key: &str) -> String {
        self.inner.url(key)
    }
}
