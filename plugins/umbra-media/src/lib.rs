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
//! ## What v0 does NOT ship (deferred to v0.1+)
//!
//! - **S3-compatible backend.** The current implementation is
//!   filesystem-only. The plan: extract a `MediaBackend` trait
//!   (`save_bytes`, `delete`, `public_url`) with two impls — `FsBackend`
//!   today, `S3Backend` (object-store + AWS SDK) next.
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

use axum::{Router, routing::get_service};
use http::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use umbra::prelude::*;

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
#[derive(Debug, Clone)]
pub struct MediaPlugin {
    mount: String,
    dir: PathBuf,
    /// Hard cap on upload size. `None` means "no enforced cap" — the
    /// reverse proxy or axum's own body-size guard remains the
    /// outer defence.
    max_size: Option<u64>,
}

impl MediaPlugin {
    /// Build a plugin serving `dir` (filesystem path) under the URL
    /// prefix `mount`.
    pub fn new(mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        Self {
            mount: mount.into(),
            dir: dir.as_ref().to_path_buf(),
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
        // Sanitise the filename: drop any path separators or NUL bytes
        // a malicious client might submit so we never escape `dir`.
        let safe_name: String = filename
            .chars()
            .filter(|c| !matches!(c, '/' | '\\' | '\0'))
            .take(120)
            .collect();
        let key = format!("{}-{safe_name}", uuid::Uuid::new_v4());
        let path = self.dir.join(&key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(MediaError::Io)?;
        }
        tokio::fs::write(&path, bytes)
            .await
            .map_err(MediaError::Io)?;

        let row = MediaFile {
            id: 0,
            key: key.clone(),
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            size: bytes.len() as i64,
            uploaded_at: chrono::Utc::now(),
        };
        let saved = MediaFile::objects()
            .save(row)
            .await
            .map_err(|e| MediaError::Storage(format!("media_file save failed: {e}")))?;

        let url = format!("{}/{}", self.mount.trim_end_matches('/'), key);
        Ok(MediaSaveOutcome { file: saved, url })
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
