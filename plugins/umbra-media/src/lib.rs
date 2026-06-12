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

use axum::{Router, routing::get_service};
use http::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use umbra::prelude::*;
use umbra::storage::{StorageError, StoredFile};

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
/// returns `<mount>/<key>`.
#[derive(Debug, Clone)]
pub struct FsStorage {
    dir: PathBuf,
    mount: String,
}

impl FsStorage {
    /// Build a filesystem backend serving `dir` under URL prefix `mount`.
    pub fn new(mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            mount: mount.into(),
        }
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
        let key = format!("{}-{safe_name}", uuid::Uuid::new_v4());
        let path = self.path_for(&key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, bytes).await?;
        let url = self.url(&key);
        Ok(StoredFile { key, url })
    }

    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        match tokio::fs::read(self.path_for(key)).await {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StorageError::NotFound),
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
        format!("{}/{}", self.mount.trim_end_matches('/'), key)
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
        }
    }

    /// Enforce a hard upload-size cap. [`Self::save`] rejects bytes
    /// longer than this with [`MediaError::TooLarge`].
    pub fn max_size(mut self, bytes: u64) -> Self {
        self.max_size = Some(bytes);
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
        let nest = format!("{mount}/{{*path}}");
        let serve = ServeDir::new(&self.dir);

        let svc = tower::ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::if_not_present(
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            ))
            .service(serve);

        Router::new().route(&nest, get_service(svc))
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
        umbra::storage::set_storage(self.storage.clone());
        Ok(())
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
    /// `Io` is preserved; `NotFound` / `Backend` collapse to `Storage`.
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::TooLarge { limit, actual } => MediaError::TooLarge { limit, actual },
            StorageError::Io(io) => MediaError::Io(io),
            StorageError::NotFound => MediaError::Storage("object not found".to_string()),
            StorageError::Backend(s) => MediaError::Storage(s),
        }
    }
}
