//! Storage: the file-bytes backend abstraction and its ambient registry.
//!
//! ## What this is
//!
//! [`Storage`] is to file bytes what [`crate::db::DbPool`] is to database
//! rows: a small, backend-agnostic seam the rest of the framework writes
//! against without caring whether the bytes land on a local filesystem,
//! S3, or anything else. A plugin (today `umbra-media` with its
//! `FsStorage`) provides the concrete impl and registers it as the
//! ambient default; future `FileField` / `ImageField` and the admin
//! resolve uploads through [`storage`] without knowing the backend.
//!
//! `umbra-core` defines the trait but never names a concrete impl — the
//! filesystem backend lives in the `umbra-media` plugin. This is the
//! dependency-inversion rule from `CLAUDE.md`: dependencies point inward
//! toward core, control flows outward through the trait. Cargo's ban on
//! circular deps enforces that core can't reach back into the plugin.
//!
//! ## Why an ambient global
//!
//! The storage backend is registered once at boot and read ambiently,
//! exactly like the DB pool (`crate::db`'s `DB_POOL`) and the template
//! engine — "the one intentional global" family sanctioned in `CLAUDE.md`.
//! A storage backend is a *backend service* (like the pool), not arbitrary
//! shared state: threading an `Arc<dyn Storage>` through every field
//! render, admin view, and upload handler would be the same boilerplate
//! the pool's `OnceLock` was introduced to avoid. The set-once discipline
//! (first registration wins; a second warns rather than panics) mirrors
//! `crate::db::init` / `crate::settings::init`.
//!
//! The ORM-only rule (`CLAUDE.md`) governs *database rows*, not file
//! bytes: `std::fs` / object-store I/O inside a `Storage` impl is the
//! sanctioned path, not a raw-SQL workaround.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

/// Re-export of `async-trait` so a plugin implementing the
/// `#[async_trait]` [`Storage`] trait can name the attribute without a
/// direct `async-trait` dep. Surfaced on the facade as
/// `umbra::storage::async_trait`. Mirrors the forms module's re-export.
pub use async_trait::async_trait as async_trait_reexport;

/// A storage backend for file bytes.
///
/// Implementors persist opaque byte blobs under a generated *key* and
/// expose them at a public URL. The default impl ships in `umbra-media`
/// (`FsStorage`, filesystem-backed); an S3 backend slots in behind the
/// same trait later (see `docs/decisions/2026-06-02-media-and-s3.md`).
///
/// Signed / auth-gated URLs are deliberately out of scope here: [`url`]
/// returns a *public* URL only. Private media is a deferred v0.x feature.
///
/// [`url`]: Storage::url
#[async_trait]
pub trait Storage: Send + Sync {
    /// Persist `bytes` under a freshly generated, collision-resistant key
    /// derived from `filename`, returning the key plus its public URL.
    ///
    /// `content_type` is the MIME type the caller declares; backends may
    /// record it (e.g. for an S3 object's `Content-Type`) but are not
    /// required to validate it — the upload handler should validate
    /// against an allow-list before calling this.
    async fn store(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError>;

    /// Read back the bytes stored under `key`.
    ///
    /// Returns [`StorageError::NotFound`] if no object exists for `key`.
    async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError>;

    /// Remove the object stored under `key`. Idempotent at the backend's
    /// discretion; deleting a missing key may succeed or return
    /// [`StorageError::NotFound`].
    async fn delete(&self, key: &str) -> Result<(), StorageError>;

    /// The public URL a client can fetch the object at. Public-only;
    /// signed URLs are deferred.
    fn url(&self, key: &str) -> String;
}

/// The outcome of a successful [`Storage::store`]: the generated key and
/// its public URL.
#[derive(Debug, Clone)]
pub struct StoredFile {
    /// The backend-generated key the bytes live under. Stable for the
    /// lifetime of the object; pass it back to [`Storage::retrieve`] /
    /// [`Storage::delete`] / [`Storage::url`].
    pub key: String,
    /// The public URL the object is served at. Equal to
    /// `storage.url(&key)`.
    pub url: String,
}

/// Errors a [`Storage`] operation can return.
#[derive(Debug)]
pub enum StorageError {
    /// No ambient backend has been registered.
    NoBackend,
    /// No object exists under the given key.
    NotFound,
    /// The bytes exceeded the backend's configured size cap.
    TooLarge {
        /// The configured limit, in bytes.
        limit: u64,
        /// The actual size that was rejected, in bytes.
        actual: u64,
    },
    /// An underlying I/O error (filesystem read/write, etc.).
    Io(std::io::Error),
    /// A backend-specific failure that doesn't map to the variants above
    /// (e.g. an S3 API error, or a row-insert failure in a wrapper).
    Backend(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::NoBackend => write!(
                f,
                "storage: no backend registered; add MediaPlugin or call set_storage"
            ),
            StorageError::NotFound => write!(f, "storage: object not found"),
            StorageError::TooLarge { limit, actual } => write!(
                f,
                "storage: object {actual}B exceeds configured cap of {limit}B"
            ),
            StorageError::Io(e) => write!(f, "storage: io: {e}"),
            StorageError::Backend(s) => write!(f, "storage: backend: {s}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StorageError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        StorageError::Io(e)
    }
}

/// The ambient storage backend, published once at boot.
///
/// Same `OnceLock` pattern as `crate::db`'s pool registry and the
/// settings handle — the sanctioned "one intentional global" family.
static STORAGE: OnceLock<Arc<dyn Storage>> = OnceLock::new();

/// Register the ambient default storage backend.
///
/// Set-once, first-wins: a second call logs a warning and keeps the
/// originally registered backend, mirroring `crate::settings::init` and
/// `crate::db::init_atomic_default` rather than panicking on a double
/// set. Returns `true` when this call won the registration, `false` when
/// a backend was already registered.
///
/// `umbra-media`'s `MediaPlugin::on_ready` calls this so the ambient
/// default is its `FsStorage`; an app can also call it directly to wire a
/// custom backend before (or instead of) any media plugin.
pub fn set_storage(s: Arc<dyn Storage>) -> bool {
    match STORAGE.set(s) {
        Ok(()) => true,
        Err(_) => {
            tracing::warn!(
                "umbra::storage::set_storage called more than once; keeping the \
                 first-registered backend and ignoring the new one"
            );
            false
        }
    }
}

/// Return the ambient storage backend.
///
/// # Panics
///
/// Panics if no backend has been registered. Wire one by adding
/// `MediaPlugin` (which registers its `FsStorage` in `on_ready`) or by
/// calling [`set_storage`] directly.
pub fn storage() -> Arc<dyn Storage> {
    try_storage().expect(
        "no Storage backend registered; add MediaPlugin or call umbra::storage::set_storage",
    )
}

/// Return the ambient storage backend, or an explicit error if one has
/// not been registered.
pub fn try_storage() -> Result<Arc<dyn Storage>, StorageError> {
    STORAGE.get().cloned().ok_or(StorageError::NoBackend)
}

/// Return the ambient storage backend if one has been registered, else
/// `None`.
///
/// The non-panicking variant of [`storage`]. Useful for boot-time
/// system checks (a future `FileField` check can warn when a model
/// declares a file field but no `Storage` backend is wired) and for
/// plugin code that runs before `on_ready`.
pub fn storage_opt() -> Option<Arc<dyn Storage>> {
    STORAGE.get().cloned()
}
