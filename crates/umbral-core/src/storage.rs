//! Storage: the file-bytes backend abstraction and its ambient registry.
//!
//! ## What this is
//!
//! [`Storage`] is to file bytes what [`crate::db::DbPool`] is to database
//! rows: a small, backend-agnostic seam the rest of the framework writes
//! against without caring whether the bytes land on a local filesystem,
//! S3, or anything else. A plugin (today `umbral-storage` with its
//! `FsStorage`) provides the concrete impl and registers it as the
//! ambient default; future `FileField` / `ImageField` and the admin
//! resolve uploads through [`storage`] without knowing the backend.
//!
//! `umbral-core` defines the trait but never names a concrete impl — the
//! filesystem backend lives in the `umbral-storage` plugin. This is the
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;

/// Re-export of `async-trait` so a plugin implementing the
/// `#[async_trait]` [`Storage`] trait can name the attribute without a
/// direct `async-trait` dep. Surfaced on the facade as
/// `umbral::storage::async_trait`. Mirrors the forms module's re-export.
pub use async_trait::async_trait as async_trait_reexport;

/// A boxed, pinned byte-stream — the streaming-upload/download currency of
/// [`Storage::store_stream`] / [`Storage::retrieve_stream`].
///
/// Object-safe (it's a trait object behind a `Box`, so it survives through
/// `Arc<dyn Storage>` dispatch) and `Send` so it can cross an `.await` on a
/// multi-threaded runtime. Each item is a `bytes::Bytes` chunk or an
/// [`std::io::Error`]; an error item aborts the stream.
pub type ByteStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>>;

/// The `ErrorKind` a [`cap_stream`] over-limit error carries, so a wrapper
/// (e.g. `SizeLimitedStorage`) can recognise "the cap tripped" versus a
/// genuine backend IO failure and map it to [`StorageError::TooLarge`].
pub const CAP_EXCEEDED_KIND: std::io::ErrorKind = std::io::ErrorKind::Other;

/// Sentinel string carried in a [`cap_stream`] over-limit error's message,
/// so the cap can be distinguished from any other `ErrorKind::Other`.
pub const CAP_EXCEEDED_MARKER: &str = "umbral-storage-cap-exceeded";

/// Wrap `body` so it passes bytes through untouched until the cumulative
/// byte count would exceed `max`, at which point it yields a single
/// `Err(io::Error)` (kind [`CAP_EXCEEDED_KIND`], message [`CAP_EXCEEDED_MARKER`])
/// and ends.
///
/// **This is the load-bearing security primitive for streaming uploads.**
/// The cap is enforced *as bytes flow*, never from a declared length: a
/// client that lies about (or omits) its `Content-Length` is still cut off
/// the instant the real bytes cross `max`, so an oversized upload can never
/// be fully written. A wrapper maps the marker error to
/// [`StorageError::TooLarge`].
pub fn cap_stream(body: ByteStream, max: u64) -> ByteStream {
    use futures_util::StreamExt;
    let mut seen: u64 = 0;
    let mut tripped = false;
    let capped = body.flat_map(move |item| {
        // Once the cap has tripped, end the stream — don't forward more.
        if tripped {
            return futures_util::stream::iter(Vec::new());
        }
        match item {
            Ok(chunk) => {
                seen = seen.saturating_add(chunk.len() as u64);
                if seen > max {
                    tripped = true;
                    let err = std::io::Error::new(CAP_EXCEEDED_KIND, CAP_EXCEEDED_MARKER);
                    futures_util::stream::iter(vec![Err(err)])
                } else {
                    futures_util::stream::iter(vec![Ok(chunk)])
                }
            }
            Err(e) => {
                tripped = true;
                futures_util::stream::iter(vec![Err(e)])
            }
        }
    });
    Box::pin(capped)
}

/// Is `e` the over-limit error produced by [`cap_stream`]? Used by a
/// streaming wrapper to map the cap trip onto [`StorageError::TooLarge`]
/// rather than a generic [`StorageError::Io`].
pub fn is_cap_exceeded(e: &std::io::Error) -> bool {
    e.kind() == CAP_EXCEEDED_KIND && e.to_string().contains(CAP_EXCEEDED_MARKER)
}

/// A storage backend for file bytes.
///
/// Implementors persist opaque byte blobs under a generated *key* and
/// expose them at a public URL. The default impl ships in `umbral-storage`
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

    /// Streaming counterpart of [`store`](Storage::store): persist a
    /// `body` byte-stream without buffering the whole payload in memory.
    ///
    /// **Additive, with a default impl** — an existing backend that does
    /// not override this still works, just buffered: the default collects
    /// the stream into a `Vec<u8>` (propagating any mid-stream IO error)
    /// and delegates to [`store`](Storage::store). Override it to true-stream
    /// to the backend (the filesystem impl writes chunk-by-chunk to disk).
    ///
    /// Size enforcement is a *decorator* concern, not this method's: wrap
    /// `body` with [`cap_stream`] before calling so the cap is applied as
    /// bytes flow, never trusting a declared `Content-Length`.
    async fn store_stream(
        &self,
        filename: &str,
        content_type: &str,
        body: ByteStream,
    ) -> Result<StoredFile, StorageError> {
        // Default: buffer the stream, then delegate to the buffered `store`.
        let mut bytes: Vec<u8> = Vec::new();
        let mut body = body;
        while let Some(chunk) = futures_util::StreamExt::next(&mut body).await {
            let chunk = chunk.map_err(StorageError::Io)?;
            bytes.extend_from_slice(&chunk);
        }
        self.store(filename, content_type, &bytes).await
    }

    /// Streaming counterpart of [`retrieve`](Storage::retrieve): read the
    /// object back as a byte-stream without holding the whole blob.
    ///
    /// **Additive, with a default impl** — the default calls
    /// [`retrieve`](Storage::retrieve) and wraps the resulting `Vec<u8>`
    /// as a single-chunk stream. Override it to true-stream from the
    /// backend (the filesystem impl streams the file off disk).
    async fn retrieve_stream(&self, key: &str) -> Result<ByteStream, StorageError> {
        let bytes = self.retrieve(key).await?;
        let chunk: Result<bytes::Bytes, std::io::Error> = Ok(bytes::Bytes::from(bytes));
        Ok(Box::pin(futures_util::stream::once(async move { chunk })))
    }

    /// Persist `bytes` at the *exact* `key` the caller supplies — the
    /// deterministic-path sibling of [`store`](Storage::store), which
    /// generates a collision-resistant key. Static asset collection needs
    /// this: a CSS file collected to `css/app.css` must land at that key,
    /// not a `uuid-app.css` one.
    ///
    /// **Additive, with a default impl** — but the default *cannot*
    /// generically write-at-exact-key without backend knowledge (the
    /// trait has no "write these bytes here" primitive beyond
    /// [`store`](Storage::store), which owns its own key). So the default
    /// returns [`StorageError::Unsupported`]. Backends that can honour an
    /// exact key (the filesystem backend, the future `LocalStorage` /
    /// `S3Storage`) override it; media's [`store`](Storage::store) stays
    /// the key-generating path.
    ///
    /// `content_type` is recorded by backends that track it (e.g. an S3
    /// object's `Content-Type`); the filesystem backend derives the
    /// served type from the key's extension instead.
    async fn put(
        &self,
        key: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<StoredFile, StorageError> {
        let _ = (key, content_type, bytes);
        Err(StorageError::Unsupported(
            "this Storage backend does not implement put(); override it to write at an exact key"
                .to_string(),
        ))
    }

    /// Streaming counterpart of [`put`](Storage::put): persist a `body`
    /// byte-stream at the exact `key` without buffering the whole payload.
    ///
    /// **Additive, with a default impl** that mirrors the
    /// [`store_stream`](Storage::store_stream)/[`store`](Storage::store)
    /// relationship: it collects the stream into a `Vec<u8>` (propagating
    /// any mid-stream IO error) and delegates to [`put`](Storage::put), so
    /// a backend that overrides `put` gets a working `put_stream` for
    /// free. Override it to true-stream to the backend.
    async fn put_stream(
        &self,
        key: &str,
        content_type: &str,
        body: ByteStream,
    ) -> Result<StoredFile, StorageError> {
        let mut bytes: Vec<u8> = Vec::new();
        let mut body = body;
        while let Some(chunk) = futures_util::StreamExt::next(&mut body).await {
            let chunk = chunk.map_err(StorageError::Io)?;
            bytes.extend_from_slice(&chunk);
        }
        self.put(key, content_type, &bytes).await
    }

    /// Does an object exist under `key`?
    ///
    /// **Additive, with a default impl** — `Ok(self.retrieve(key).await.is_ok())`,
    /// which works for any backend through [`retrieve`](Storage::retrieve).
    /// Backends with a cheaper presence check (an S3 `HEAD`, a filesystem
    /// `metadata` stat) override it to avoid reading the whole blob.
    async fn exists(&self, key: &str) -> Result<bool, StorageError> {
        Ok(self.retrieve(key).await.is_ok())
    }

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
    /// The number of bytes actually written. For [`Storage::store`] this
    /// equals `bytes.len()`; for [`Storage::store_stream`] it is the
    /// cumulative count streamed to the backend (the truth a `media_file`
    /// row records, since a stream has no trustworthy up-front length).
    pub size: u64,
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
    /// The backend doesn't implement the requested operation — returned by
    /// the default [`Storage::put`] impl for a backend that can't write at
    /// an exact key. The message names what's missing.
    Unsupported(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::NoBackend => write!(
                f,
                "storage: no backend registered; add StoragePlugin or call set_storage"
            ),
            StorageError::NotFound => write!(f, "storage: object not found"),
            StorageError::TooLarge { limit, actual } => write!(
                f,
                "storage: object {actual}B exceeds configured cap of {limit}B"
            ),
            StorageError::Io(e) => write!(f, "storage: io: {e}"),
            StorageError::Backend(s) => write!(f, "storage: backend: {s}"),
            StorageError::Unsupported(s) => write!(f, "storage: unsupported: {s}"),
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

/// The conventional name of the **media** (user-upload) storage instance,
/// the `"default"` entry in the storage registry. The back-compat accessors
/// ([`storage`], [`set_storage`], …) operate on this name.
pub const DEFAULT: &str = "default";

/// The conventional name of the **static-files** storage instance,
/// the `"staticfiles"` entry in the storage registry, where `collectstatic`
/// writes collected assets. Resolved independently of [`DEFAULT`].
pub const STATICFILES: &str = "staticfiles";

/// The ambient, **named** storage registry, published at boot.
///
/// The named storage map: a small map from a static name
/// (`"default"` for media, `"staticfiles"` for collected assets) to its
/// backend. Replaces the former single-global `OnceLock<Arc<dyn Storage>>`
/// so media and static can resolve independent backends under one
/// abstraction. Registration is boot-time, so a `Mutex<HashMap>` behind a
/// `OnceLock` is the right shape; the set-once-*per-name* discipline
/// (first-wins, warn-and-keep on a re-set) mirrors the old single global.
///
/// Same "one intentional global" family as `crate::db`'s pool registry
/// and the settings handle.
static STORAGES: OnceLock<Mutex<HashMap<&'static str, Arc<dyn Storage>>>> = OnceLock::new();

/// Access the named registry, initialising the empty map on first use.
fn registry() -> &'static Mutex<HashMap<&'static str, Arc<dyn Storage>>> {
    STORAGES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register the storage backend under `name` (e.g. [`DEFAULT`] for media,
/// [`STATICFILES`] for collected static assets).
///
/// Set-once **per name**, first-wins: a second call for the *same* name
/// logs a warning and keeps the originally registered backend (different
/// names register independently). Returns `true` when this call won the
/// registration for `name`, `false` when that name was already taken.
pub fn set_storage_named(name: &'static str, s: Arc<dyn Storage>) -> bool {
    let mut map = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.contains_key(name) {
        tracing::warn!(
            name,
            "umbral::storage::set_storage_named called more than once for the same name; \
             keeping the first-registered backend and ignoring the new one"
        );
        false
    } else {
        map.insert(name, s);
        true
    }
}

/// Return the storage backend registered under `name`.
///
/// # Panics
///
/// Panics if no backend has been registered under `name`. Wire one by
/// adding the plugin that owns that name (`StoragePlugin` for [`DEFAULT`])
/// or by calling [`set_storage_named`] directly.
pub fn storage_named(name: &str) -> Arc<dyn Storage> {
    try_storage_named(name).unwrap_or_else(|_| {
        panic!(
            "no Storage backend registered under `{name}`; add the owning plugin \
             (StoragePlugin for `default`) or call umbral::storage::set_storage_named"
        )
    })
}

/// Return the storage backend registered under `name`, or
/// [`StorageError::NoBackend`] if none is.
pub fn try_storage_named(name: &str) -> Result<Arc<dyn Storage>, StorageError> {
    storage_opt_named(name).ok_or(StorageError::NoBackend)
}

/// Return the storage backend registered under `name` if one exists, else
/// `None`. The non-panicking variant of [`storage_named`].
pub fn storage_opt_named(name: &str) -> Option<Arc<dyn Storage>> {
    let map = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.get(name).cloned()
}

/// Register the ambient **default** (media) storage backend — the
/// back-compat alias for `set_storage_named(`[`DEFAULT`]`, s)`.
///
/// Set-once, first-wins: a second call logs a warning and keeps the
/// originally registered backend, mirroring `crate::settings::init` and
/// `crate::db::init_atomic_default` rather than panicking on a double
/// set. Returns `true` when this call won the registration, `false` when
/// a backend was already registered.
///
/// `umbral-storage`'s `StoragePlugin::on_ready` calls this so the ambient
/// default is its `FsStorage`; an app can also call it directly to wire a
/// custom backend before (or instead of) any storage plugin.
pub fn set_storage(s: Arc<dyn Storage>) -> bool {
    set_storage_named(DEFAULT, s)
}

/// Return the ambient **default** (media) storage backend — the
/// back-compat alias for `storage_named(`[`DEFAULT`]`)`.
///
/// # Panics
///
/// Panics if no backend has been registered. Wire one by adding
/// `StoragePlugin` (which registers its `FsStorage` in `on_ready`) or by
/// calling [`set_storage`] directly.
pub fn storage() -> Arc<dyn Storage> {
    try_storage().expect(
        "no Storage backend registered; add StoragePlugin or call umbral::storage::set_storage",
    )
}

/// Return the ambient **default** (media) storage backend, or an explicit
/// error if none is registered. Back-compat alias for
/// `try_storage_named(`[`DEFAULT`]`)`.
pub fn try_storage() -> Result<Arc<dyn Storage>, StorageError> {
    try_storage_named(DEFAULT)
}

/// Return the ambient **default** (media) storage backend if registered,
/// else `None`. Back-compat alias for `storage_opt_named(`[`DEFAULT`]`)`.
///
/// The non-panicking variant of [`storage`]. Useful for boot-time
/// system checks (a future `FileField` check can warn when a model
/// declares a file field but no `Storage` backend is wired) and for
/// plugin code that runs before `on_ready`.
pub fn storage_opt() -> Option<Arc<dyn Storage>> {
    storage_opt_named(DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;
    use std::sync::Mutex as StdMutex;

    /// A minimal in-memory backend. `store` generates a key; `put` is left
    /// at the trait default (returns `Unsupported`) so we can assert the
    /// default path; `exists` is left at the trait default (via `retrieve`).
    struct MemNoPut {
        objects: StdMutex<Map<String, Vec<u8>>>,
    }

    impl MemNoPut {
        fn new() -> Self {
            Self {
                objects: StdMutex::new(Map::new()),
            }
        }
    }

    #[async_trait]
    impl Storage for MemNoPut {
        async fn store(
            &self,
            filename: &str,
            _content_type: &str,
            bytes: &[u8],
        ) -> Result<StoredFile, StorageError> {
            let key = format!("k-{filename}");
            self.objects
                .lock()
                .unwrap()
                .insert(key.clone(), bytes.to_vec());
            Ok(StoredFile {
                url: self.url(&key),
                key,
                size: bytes.len() as u64,
            })
        }

        async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
            self.objects
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or(StorageError::NotFound)
        }

        async fn delete(&self, key: &str) -> Result<(), StorageError> {
            self.objects.lock().unwrap().remove(key);
            Ok(())
        }

        fn url(&self, key: &str) -> String {
            format!("/mem/{key}")
        }
    }

    /// Same backend but overriding `put` to write at the exact key.
    struct MemWithPut {
        objects: StdMutex<Map<String, Vec<u8>>>,
    }

    impl MemWithPut {
        fn new() -> Self {
            Self {
                objects: StdMutex::new(Map::new()),
            }
        }
    }

    #[async_trait]
    impl Storage for MemWithPut {
        async fn store(
            &self,
            filename: &str,
            ct: &str,
            bytes: &[u8],
        ) -> Result<StoredFile, StorageError> {
            self.put(&format!("k-{filename}"), ct, bytes).await
        }

        async fn retrieve(&self, key: &str) -> Result<Vec<u8>, StorageError> {
            self.objects
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or(StorageError::NotFound)
        }

        async fn put(
            &self,
            key: &str,
            _ct: &str,
            bytes: &[u8],
        ) -> Result<StoredFile, StorageError> {
            self.objects
                .lock()
                .unwrap()
                .insert(key.to_string(), bytes.to_vec());
            Ok(StoredFile {
                url: self.url(key),
                key: key.to_string(),
                size: bytes.len() as u64,
            })
        }

        async fn delete(&self, key: &str) -> Result<(), StorageError> {
            self.objects.lock().unwrap().remove(key);
            Ok(())
        }

        fn url(&self, key: &str) -> String {
            format!("/mem/{key}")
        }
    }

    #[tokio::test]
    async fn put_default_returns_unsupported() {
        let s = MemNoPut::new();
        let err = s.put("css/app.css", "text/css", b"x").await.unwrap_err();
        match err {
            StorageError::Unsupported(msg) => {
                assert!(msg.contains("does not implement put"), "msg = {msg}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn put_override_writes_at_exact_key() {
        let s = MemWithPut::new();
        let stored = s
            .put("css/app.css", "text/css", b"body{}")
            .await
            .unwrap();
        // The key is EXACTLY what we asked for — no generation.
        assert_eq!(stored.key, "css/app.css");
        assert_eq!(stored.size, 6);
        // And it round-trips back at that exact key.
        assert_eq!(s.retrieve("css/app.css").await.unwrap(), b"body{}");
    }

    #[tokio::test]
    async fn exists_default_true_after_store_false_when_missing() {
        let s = MemNoPut::new();
        let stored = s.store("a.txt", "text/plain", b"hi").await.unwrap();
        assert!(s.exists(&stored.key).await.unwrap());
        assert!(!s.exists("nope").await.unwrap());
    }

    #[tokio::test]
    async fn put_stream_default_delegates_to_put() {
        let s = MemWithPut::new();
        let body: ByteStream = Box::pin(futures_util::stream::once(async {
            Ok(bytes::Bytes::from_static(b"streamed"))
        }));
        let stored = s.put_stream("js/app.js", "text/javascript", body).await.unwrap();
        assert_eq!(stored.key, "js/app.js");
        assert_eq!(s.retrieve("js/app.js").await.unwrap(), b"streamed");
    }
}
