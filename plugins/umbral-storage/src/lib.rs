//! umbral-storage — the unified storage plugin.
//!
//! Stage 2 of unifying umbral's storage: this crate MERGES the former
//! `umbral-static` (static-file serving) and `umbral-media` (user uploads)
//! plugins into ONE [`StoragePlugin`] built on the unified
//! [`umbral::storage::Storage`] trait. A single S3 backend ([`S3Storage`],
//! feature-gated) serves both the media (`"default"`) and static
//! (`"staticfiles"`) storage instances.
//!
//! ## Two sides, one plugin
//!
//! ```ignore
//! App::builder()
//!     .plugin(
//!         StoragePlugin::new()
//!             .static_files("/static", "./assets")   // the umbral-static side
//!             .media("/media", "./media")            // the umbral-media side
//!             .max_size(10 * 1024 * 1024)            // media upload cap
//!             .cleanup_on_delete::<Post>(),          // FileField cleanup
//!     )
//!     .build()?;
//! ```
//!
//! - **Static side.** [`StoragePlugin::static_files`] /
//!   [`StoragePlugin::embedded`] / [`StoragePlugin::max_age`] serve a
//!   filesystem or `include_dir!`-embedded tree with ETag / cache headers
//!   and a symlink-escape guard. The `collectstatic` command (with
//!   `--hashed` / `--clear` / `--storage s3`) collects assets through the
//!   `"staticfiles"` storage instance.
//! - **Media side.** [`StoragePlugin::media`] /
//!   [`StoragePlugin::media_with_storage`] serve user uploads, track them
//!   in the [`MediaFile`] model, enforce a streaming size cap, and run
//!   file-lifecycle cleanup on delete / replace. The media backend is
//!   registered as the ambient `"default"` storage.
//!
//! Either side is optional: a static-only or media-only `StoragePlugin`
//! works. `on_ready` registers `set_storage_named("default", media)` and
//! `set_storage_named("staticfiles", static)` for whichever sides are
//! configured.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use http::header::{HeaderName, HeaderValue};
use include_dir::Dir;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use umbral::prelude::*;

/// An access-control callback for media serving (audit_2 plugin-storage-tasks
/// #3). Given the request headers and the requested media key (the path under
/// the mount), it returns `true` to allow the response or `false` to deny it
/// (403). It runs on EVERY `GET <mount>/<key>` before any bytes are served —
/// check a session cookie / bearer token and, if the file is private, its
/// per-user ownership. `None` (the default) serves every file to anyone, the
/// original backward-compatible behaviour.
pub type MediaAccessFn = Arc<
    dyn Fn(
            &http::HeaderMap,
            &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;
use umbral::storage::{ByteStream, DEFAULT, STATICFILES};

mod collect;
mod media;
#[cfg(feature = "s3")]
mod s3;
mod static_serve;

pub use media::clear_processors_for_test;
pub use media::{
    BoxError, FsStorage, MediaError, MediaFile, MediaSaveOutcome, MediaTracking, Processor,
    STATUS_FAILED, STATUS_PROCESSING, STATUS_READY,
};
#[doc(hidden)]
pub use media::{IMAGE_TYPES, sniff_content_type};

#[cfg(feature = "images")]
pub mod images;
#[cfg(feature = "images")]
pub use images::{Thumbnail, thumbnails, variant_key};
#[cfg(feature = "s3")]
pub use s3::{S3Storage, S3StorageBuilder};
// Re-export the core Storage trait through this crate for ergonomic
// `umbral_storage::Storage` use, matching the old crates' surface.
pub use umbral::storage::{Storage, StorageError, StoredFile};

// The derived `media_file` column-const module (`media_file::KEY`, …) is
// emitted by `#[derive(Model)]` at the crate root where the model is
// defined. Re-export it so consumers reach `umbral_storage::media_file::KEY`.
pub use media::media_file;

use collect::CollectStaticCommand;
use media::{
    CleanupSpec, SizeLimitedStorage, file_columns_of, save_deferred_through, save_stream_through,
    save_through,
};
use static_serve::StaticServe;

/// The default media upload-size cap (25 MiB), applied whenever a media
/// side is configured without an explicit [`StoragePlugin::max_size`].
///
/// A media side with NO cap lets any client stream an arbitrarily large
/// body to storage — an unauthenticated disk-exhaustion DoS (audit
/// `plugin-storage-tasks` #2) — so uncapped is opt-in via
/// [`StoragePlugin::max_size_unlimited`], never the default.
pub const DEFAULT_MAX_UPLOAD_SIZE: u64 = 25 * 1024 * 1024;

/// The unified storage plugin: a static-serving side, a media-upload side,
/// or both. Replaces the former `StaticPlugin` + `MediaPlugin`.
#[derive(Clone)]
pub struct StoragePlugin {
    /// The static-serving side, if configured.
    static_side: Option<StaticServe>,
    /// The media-upload side, if configured.
    media: Option<MediaSide>,
    /// Background upload processors, run in registration order after a file's
    /// bytes land in storage. Installed ambiently at `on_ready` (see
    /// [`media::set_processors`]) so EVERY save path can trigger them.
    processors: Vec<Processor>,
    /// Optional access-control gate for the media GET route (audit_2
    /// plugin-storage-tasks #3). `None` serves every file to anyone.
    media_access: Option<MediaAccessFn>,
}

/// The media side's configuration: mount, on-disk dir, the backend, an
/// optional size cap, and the file-lifecycle cleanup registrations.
#[derive(Clone)]
struct MediaSide {
    mount: String,
    dir: PathBuf,
    storage: Arc<dyn Storage>,
    max_size: Option<u64>,
    /// gaps3 #51 — the accepted upload types. `None` = accept anything (the
    /// pre-existing behaviour); `Some(list)` enforces an allow-list on every
    /// save path, sniffing the bytes rather than trusting the declared type.
    accept: Option<Vec<String>>,
    cleanup: Vec<CleanupSpec>,
}

impl Default for StoragePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl StoragePlugin {
    /// An empty plugin — add a static side ([`Self::static_files`] /
    /// [`Self::embedded`]) and/or a media side ([`Self::media`] /
    /// [`Self::media_with_storage`]).
    pub fn new() -> Self {
        Self {
            static_side: None,
            media: None,
            processors: Vec::new(),
            media_access: None,
        }
    }

    /// Gate the media GET route behind an access-control callback (audit_2
    /// plugin-storage-tasks #3). By default `ServeDir` serves **every** uploaded
    /// file to anyone who knows (or guesses / is handed) its URL — fine for
    /// public assets, an IDOR for private uploads. Set this and the callback
    /// runs on every `GET <mount>/<key>`: return `true` to serve, `false` for a
    /// 403. The closure receives the request headers (read a session cookie /
    /// bearer token) and the requested key (look up per-file ownership).
    ///
    /// ```ignore
    /// StoragePlugin::new()
    ///     .media_with_storage("/media", fs)
    ///     .media_access(|headers: HeaderMap, key: String| async move {
    ///         // e.g. resolve the session user and check they own `key`
    ///         is_authenticated(&headers).await
    ///     })
    /// ```
    pub fn media_access<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(http::HeaderMap, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = bool> + Send + 'static,
    {
        self.media_access = Some(Arc::new(move |headers: &http::HeaderMap, key: &str| {
            Box::pin(f(headers.clone(), key.to_string()))
        }));
        self
    }

    /// Register a **background upload processor** — an async fn run over each
    /// saved [`MediaFile`] after its bytes land in storage (thumbnailing,
    /// transcoding, virus scan, …). Multiple are allowed; they run in
    /// registration order. While they run the row's `status` is
    /// `"processing"`; on all-ok it becomes `"ready"`, on any error
    /// `"failed"`.
    ///
    /// ```ignore
    /// StoragePlugin::new()
    ///     .media("/media", "./media")
    ///     .on_upload(|media: MediaFile| async move {
    ///         make_thumbnail(&media).await?;
    ///         Ok(())
    ///     })
    /// ```
    ///
    /// Processing runs via an in-process [`tokio::spawn`] (no `umbral-tasks`
    /// dependency). For crash-durable processing, have the processor enqueue
    /// an `umbral-tasks` job instead of doing the work inline. To notify the
    /// frontend when a file finishes, expose the model over realtime —
    /// `RealtimePlugin::new().expose::<MediaFile>(...)` — and the
    /// `post_save:media_file` a status change fires is pushed automatically;
    /// umbral-storage never imports realtime.
    pub fn on_upload<F, Fut, E>(mut self, f: F) -> Self
    where
        F: Fn(MediaFile) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), E>> + Send + 'static,
        E: Into<media::BoxError>,
    {
        let processor: Processor = Arc::new(move |media: MediaFile| {
            let fut = f(media);
            Box::pin(async move { fut.await.map_err(Into::into) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<(), media::BoxError>> + Send>,
                >
        });
        self.processors.push(processor);
        self
    }

    // ── Static side ────────────────────────────────────────────────────

    /// Serve a *filesystem* directory at `mount` (the old
    /// `StaticPlugin::new`). Requests to `/<mount>/<rest>` look for
    /// `<dir>/<rest>` on disk, guarded against symlink escape.
    pub fn static_files(mut self, mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        self.static_side = Some(StaticServe::fs(mount, dir));
        self
    }

    /// Serve a compile-time-embedded asset tree at `mount` (the old
    /// `StaticPlugin::embedded`). `dir` is a `&'static Dir<'static>` from
    /// [`include_dir::include_dir!`]. Path traversal is structurally
    /// impossible.
    pub fn embedded(mut self, mount: impl Into<String>, dir: &'static Dir<'static>) -> Self {
        self.static_side = Some(StaticServe::embedded(mount, dir));
        self
    }

    /// Set a `Cache-Control: public, max-age=<duration>` header on every
    /// static response. Forced to `0` in `Environment::Dev`. Only
    /// meaningful when a static side is configured.
    pub fn max_age(mut self, duration: Duration) -> Self {
        if let Some(side) = self.static_side.take() {
            self.static_side = Some(side.with_max_age(duration));
        } else {
            tracing::warn!(
                "umbral-storage: max_age() called before a static side was configured; \
                 add .static_files(..) or .embedded(..) first"
            );
        }
        self
    }

    /// On-disk static directory, when the static side is a filesystem
    /// source. `None` for embedded (or no static side).
    pub fn static_dir(&self) -> Option<&Path> {
        self.static_side.as_ref().and_then(StaticServe::dir)
    }

    /// The static mount path, if a static side is configured.
    pub fn static_mount(&self) -> Option<&str> {
        self.static_side.as_ref().map(|s| s.mount.as_str())
    }

    // ── Media side ─────────────────────────────────────────────────────

    /// Serve user uploads from `dir` under URL prefix `mount`, backed by an
    /// [`FsStorage`] (the old `MediaPlugin::new`).
    ///
    /// Uploads are capped at [`DEFAULT_MAX_UPLOAD_SIZE`] (25 MiB) by
    /// default; raise or lower it with [`Self::max_size`], or remove it
    /// (deliberately) with [`Self::max_size_unlimited`].
    pub fn media(mut self, mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        let mount = mount.into();
        let dir = dir.as_ref().to_path_buf();
        let storage: Arc<dyn Storage> = Arc::new(FsStorage::new(mount.clone(), dir.clone()));
        self.media = Some(MediaSide {
            mount,
            dir,
            storage,
            max_size: Some(DEFAULT_MAX_UPLOAD_SIZE),
            accept: None,
            cleanup: Vec::new(),
        });
        self
    }

    /// Serve uploads backed by a custom [`Storage`] mounted at `mount` (the
    /// old `MediaPlugin::with_storage`). The GET-serving `ServeDir` still
    /// reads `mount` as a dir; a non-filesystem backend serves its own
    /// URLs, so the route is a no-op for keys that never hit local disk.
    pub fn media_with_storage(
        mut self,
        mount: impl Into<String>,
        storage: Arc<dyn Storage>,
    ) -> Self {
        let mount = mount.into();
        self.media = Some(MediaSide {
            dir: PathBuf::from(&mount),
            mount,
            storage,
            max_size: Some(DEFAULT_MAX_UPLOAD_SIZE),
            accept: None,
            cleanup: Vec::new(),
        });
        self
    }

    /// Back the media side with the unified [`S3Storage`] (feature `s3`).
    /// The same backend can serve static via the `"staticfiles"` instance.
    #[cfg(feature = "s3")]
    pub fn media_s3(mut self, mount: impl Into<String>, s3: S3Storage) -> Self {
        let mount = mount.into();
        self.media = Some(MediaSide {
            dir: PathBuf::from(&mount),
            mount,
            storage: Arc::new(s3),
            max_size: Some(DEFAULT_MAX_UPLOAD_SIZE),
            accept: None,
            cleanup: Vec::new(),
        });
        self
    }

    /// Configure an absolute public base for the default [`FsStorage`]
    /// media backend so resolved URLs are fully-qualified. Only meaningful
    /// for an `FsStorage`-backed media side built by [`Self::media`].
    pub fn public_base(mut self, base: impl Into<String>) -> Self {
        let base = base.into();
        if let Some(media) = self.media.as_mut() {
            media.storage = Arc::new(
                FsStorage::new(media.mount.clone(), media.dir.clone()).with_public_base(base),
            );
        } else {
            tracing::warn!(
                "umbral-storage: public_base() called before a media side was configured; \
                 add .media(..) first"
            );
        }
        self
    }

    /// Enforce a hard upload-size cap on the media side (replacing the
    /// [`DEFAULT_MAX_UPLOAD_SIZE`] default). [`Self::save`] rejects bytes
    /// longer than this; the streaming path enforces it mid-stream.
    /// Restrict uploads to an allow-list of MIME types (gaps3 #51).
    ///
    /// The size cap stops a 20 MB file. It does **not** stop a 2 MB `.exe`
    /// renamed to `avatar.png` from landing in an `ImageField` — nothing did,
    /// before this.
    ///
    /// Enforcement sniffs the **bytes**, not the declared `Content-Type`. A
    /// client-declared type is trivially spoofed, so a policy that only reads the
    /// declaration stops nobody: the declaration must be on the list, the bytes'
    /// real signature must be on the list, and the two must agree.
    ///
    /// ```ignore
    /// StoragePlugin::default().media("/media", fs).accept(&["image/png", "application/pdf"])
    /// ```
    pub fn accept(mut self, types: &[&str]) -> Self {
        let list: Vec<String> = types.iter().map(|t| t.to_string()).collect();
        if let Some(m) = self.media.as_mut() {
            m.accept = Some(list);
        }
        self
    }

    /// [`Self::accept`] with the image set — PNG / JPEG / GIF / WEBP.
    ///
    /// **SVG is deliberately not included.** An SVG is a script-execution vector,
    /// not a picture; an image allow-list should not invite one in.
    pub fn accept_images(self) -> Self {
        self.accept(media::IMAGE_TYPES)
    }

    pub fn max_size(mut self, bytes: u64) -> Self {
        if let Some(media) = self.media.as_mut() {
            media.max_size = Some(bytes);
        } else {
            tracing::warn!(
                "umbral-storage: max_size() called before a media side was configured; \
                 add .media(..) first"
            );
        }
        self
    }

    /// Remove the upload-size cap entirely (the default is
    /// [`DEFAULT_MAX_UPLOAD_SIZE`]). Only reach for this when uploads come
    /// from trusted/authenticated callers or an upstream proxy enforces its
    /// own body limit: an uncapped media side lets any client stream an
    /// arbitrarily large body to storage (disk-exhaustion DoS).
    pub fn max_size_unlimited(mut self) -> Self {
        if let Some(media) = self.media.as_mut() {
            media.max_size = None;
        } else {
            tracing::warn!(
                "umbral-storage: max_size_unlimited() called before a media side was \
                 configured; add .media(..) first"
            );
        }
        self
    }

    /// The effective media upload-size cap: `Some(bytes)` (the
    /// [`DEFAULT_MAX_UPLOAD_SIZE`] default or a [`Self::max_size`]
    /// override), or `None` after [`Self::max_size_unlimited`]. `None`
    /// also when no media side is configured.
    pub fn media_max_size(&self) -> Option<u64> {
        self.media.as_ref().and_then(|m| m.max_size)
    }

    /// Opt model `M` into **file-lifecycle cleanup**, auto-detecting its
    /// file columns (`FileField` / `ImageField`). On per-row delete the
    /// blob behind every non-empty key is removed; on replace (file key
    /// A→B via `save`) the OLD blob is removed (gaps2 #92). Best-effort.
    pub fn cleanup_on_delete<M: Model>(mut self) -> Self {
        let columns = file_columns_of::<M>();
        let Some(media) = self.media.as_mut() else {
            tracing::warn!(
                "umbral-storage: cleanup_on_delete() called before a media side was configured; \
                 add .media(..) first"
            );
            return self;
        };
        if columns.is_empty() {
            tracing::warn!(
                table = M::TABLE,
                "umbral-storage: cleanup_on_delete found no FileField/ImageField columns on \
                 `{}`; nothing will be cleaned up. Name the columns explicitly with \
                 `cleanup_files` if you overrode the file widget.",
                M::TABLE
            );
        } else {
            media.cleanup.push(CleanupSpec {
                table: M::TABLE.to_string(),
                columns,
            });
        }
        self
    }

    /// Opt model `M` into file-lifecycle cleanup for the named file
    /// columns explicitly (when widget-based auto-detection can't see a
    /// file column).
    pub fn cleanup_files<M: Model>(mut self, fields: &[&str]) -> Self {
        let Some(media) = self.media.as_mut() else {
            tracing::warn!(
                "umbral-storage: cleanup_files() called before a media side was configured; \
                 add .media(..) first"
            );
            return self;
        };
        media.cleanup.push(CleanupSpec {
            table: M::TABLE.to_string(),
            columns: fields.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    /// The media mount path, if a media side is configured.
    pub fn media_mount(&self) -> Option<&str> {
        self.media.as_ref().map(|m| m.mount.as_str())
    }

    /// The media on-disk directory, if a media side is configured.
    pub fn media_dir(&self) -> Option<&Path> {
        self.media.as_ref().map(|m| m.dir.as_path())
    }

    /// The media storage backend (before the `MediaTracking` decorator is
    /// applied in `on_ready`). Exposed so callers / tests can resolve a
    /// key's public URL the same way the registered backend will.
    pub fn storage(&self) -> &Arc<dyn Storage> {
        &self
            .media
            .as_ref()
            .expect("storage() requires a media side; add .media(..) / .media_with_storage(..)")
            .storage
    }

    /// Test-only: wrap `inner` in the internal `SizeLimitedStorage`
    /// decorator so the mid-stream cap can be exercised in isolation,
    /// exactly as `on_ready` wires it. Not public API.
    #[doc(hidden)]
    pub fn size_limited_for_test(inner: Arc<dyn Storage>, max_size: u64) -> Arc<dyn Storage> {
        Arc::new(SizeLimitedStorage::new(inner, max_size))
    }

    /// Test-only: wrap `inner` in the internal type-policy decorator, exactly as
    /// `on_ready` wires it (gaps3 #51). Not public API.
    #[doc(hidden)]
    pub fn type_limited_for_test(inner: Arc<dyn Storage>, accept: &[&str]) -> Arc<dyn Storage> {
        Arc::new(media::TypeLimitedStorage::new(
            inner,
            accept.iter().map(|t| t.to_string()).collect(),
        ))
    }

    /// Persist one upload through the media backend and record it in
    /// `media_file` (the old `MediaPlugin::save`).
    pub async fn save(
        &self,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<MediaSaveOutcome, MediaError> {
        let media = self
            .media
            .as_ref()
            .expect("save() requires a media side; add .media(..) / .media_with_storage(..)");
        save_through(
            &media.storage,
            media.max_size,
            filename,
            content_type,
            bytes,
        )
        .await
    }

    /// Streaming counterpart of [`save`](StoragePlugin::save): persist an
    /// upload from a byte-stream WITHOUT buffering, enforcing `max_size`
    /// MID-STREAM (the old `MediaPlugin::save_stream`).
    pub async fn save_stream(
        &self,
        filename: &str,
        content_type: &str,
        body: ByteStream,
    ) -> Result<MediaSaveOutcome, MediaError> {
        let media = self.media.as_ref().expect(
            "save_stream() requires a media side; add .media(..) / .media_with_storage(..)",
        );
        save_stream_through(&media.storage, media.max_size, filename, content_type, body).await
    }

    /// **Deferred-upload** counterpart of [`save`](StoragePlugin::save) (Mode
    /// B): insert the `media_file` row with `status="processing"` and return
    /// IMMEDIATELY with the final, deterministic URL — the bytes are written
    /// to the backend in a background [`tokio::spawn`], so the URL 404s until
    /// that write (and the registered processors) finish. On success the row
    /// flips to `status="ready"`; on any failure (write or processor) it
    /// becomes `status="failed"`.
    ///
    /// The frontend shows a placeholder until the `post_save:media_file` a
    /// status change fires reaches it (forward it over realtime by exposing
    /// the model — no umbral-storage→realtime coupling) or until a poll of the
    /// row's `status` reads `"ready"`. Use [`save`](StoragePlugin::save)
    /// instead when the URL must resolve the instant you return; use this
    /// when the caller mustn't block on a slow backend write.
    pub async fn save_deferred(
        &self,
        filename: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<MediaSaveOutcome, MediaError> {
        let media = self.media.as_ref().expect(
            "save_deferred() requires a media side; add .media(..) / .media_with_storage(..)",
        );
        save_deferred_through(
            &media.storage,
            media.max_size,
            filename,
            content_type,
            bytes,
        )
        .await
    }
}

impl Plugin for StoragePlugin {
    fn name(&self) -> &'static str {
        "storage"
    }

    fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
        // The MediaFile tracking model exists only when a media side is
        // configured.
        if self.media.is_some() {
            vec![umbral::migrate::ModelMeta::for_::<MediaFile>()]
        } else {
            Vec::new()
        }
    }

    fn routes(&self) -> Router {
        let mut router = Router::new();

        // Static side.
        if let Some(side) = &self.static_side {
            router = router.merge(side.routes());
        }

        // Media side — `ServeDir` over the media dir, with a nosniff
        // header, nested so `/media/<key>` maps to `<dir>/<key>`. The
        // same symlink-escape guard the static side uses wraps the
        // `ServeDir` (defence in depth: uploads get sanitised UUID keys,
        // but a media dir writable by another process could contain a
        // symlink pointing outside the root — audit `plugin-storage-tasks`
        // #8).
        if let Some(media) = &self.media {
            if !media.dir.exists() {
                tracing::warn!(
                    "umbral-storage: media directory `{}` does not exist; requests under `{}` \
                     will return 404",
                    media.dir.display(),
                    media.mount
                );
            }
            let mount = media.mount.trim_end_matches('/').to_string();
            let serve = tower::ServiceBuilder::new()
                .map_response(|resp: http::Response<_>| resp.map(axum::body::Body::new))
                .service(ServeDir::new(&media.dir));
            let guarded = static_serve::SymlinkGuardService::new(media.dir.clone(), serve);
            let svc = tower::ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::if_not_present(
                    HeaderName::from_static("x-content-type-options"),
                    HeaderValue::from_static("nosniff"),
                ))
                .service(guarded);
            // Build the media routes in their OWN sub-router so an access-control
            // layer (audit_2 plugin-storage-tasks #3) wraps ONLY media GETs, not
            // the static side. Without a gate this is byte-identical to before.
            let mut media_router = Router::new().nest_service(&mount, svc);
            if let Some(access) = &self.media_access {
                let access = access.clone();
                let mount_prefix = mount.clone();
                media_router = media_router.layer(axum::middleware::from_fn(
                    move |req: axum::extract::Request, next: axum::middleware::Next| {
                        let access = access.clone();
                        let mount_prefix = mount_prefix.clone();
                        async move {
                            // The requested key = path with the mount prefix and
                            // any leading slash stripped (`/media/a/b.jpg` → `a/b.jpg`).
                            let path = req.uri().path();
                            let key = path
                                .strip_prefix(&mount_prefix)
                                .unwrap_or(path)
                                .trim_start_matches('/')
                                .to_string();
                            let allowed = access(req.headers(), &key).await;
                            if allowed {
                                next.run(req).await
                            } else {
                                (
                                    axum::http::StatusCode::FORBIDDEN,
                                    "forbidden: you are not allowed to access this file",
                                )
                                    .into_response()
                            }
                        }
                    },
                ));
            }
            router = router.merge(media_router);
        }

        router
    }

    fn static_root_dirs(&self) -> Vec<PathBuf> {
        self.static_side
            .as_ref()
            .map(StaticServe::static_root_dirs)
            .unwrap_or_default()
    }

    fn commands(&self) -> Vec<Box<dyn umbral::cli::PluginCommand>> {
        // `collectstatic` is contributed only when a static side exists.
        if self.static_side.is_some() {
            vec![Box::new(CollectStaticCommand)]
        } else {
            Vec::new()
        }
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), umbral::plugin::PluginError> {
        // Install the background upload processors ambiently (like the
        // storage seam) so EVERY save path — `save`, `save_deferred`, and the
        // admin/form multipart upload through `MediaTracking` — can trigger
        // them, not just `StoragePlugin::save`.
        if !self.processors.is_empty() {
            media::set_processors(Arc::new(self.processors.clone()));
        }

        // Register the media backend as the ambient "default" instance,
        // wrapped in MediaTracking (so the admin/form upload path records a
        // media_file row) and, when a cap is set, SizeLimitedStorage.
        if let Some(media) = &self.media {
            let mut storage: Arc<dyn Storage> = match media.max_size {
                Some(max_size) => {
                    Arc::new(SizeLimitedStorage::new(media.storage.clone(), max_size))
                }
                None => media.storage.clone(),
            };
            // gaps3 #51: the type policy wraps the size cap, so an oversized
            // upload is refused on size before anything is buffered to sniff it.
            // A decorator (not a handler check) means the admin, forms, REST and
            // any hand-written upload route all inherit the policy — including
            // routes written after this line.
            if let Some(accept) = &media.accept {
                storage = Arc::new(media::TypeLimitedStorage::new(storage, accept.clone()));
            }
            umbral::storage::set_storage_named(DEFAULT, Arc::new(MediaTracking::new(storage)));

            for spec in &media.cleanup {
                tracing::debug!(
                    table = %spec.table,
                    columns = ?spec.columns,
                    "umbral-storage: registering file-lifecycle cleanup on delete"
                );
                media::register_cleanup(spec);
            }
        }

        // Register the static collection backend as the ambient
        // "staticfiles" instance. For a filesystem static side, point an
        // FsStorage at the configured `static_root` so collectstatic's
        // put("css/app.css", …) writes <static_root>/css/app.css and the
        // unified static handler can read it back. Skip for an embedded
        // side (nothing to collect) or when there's no static side.
        if let Some(side) = &self.static_side {
            if side.dir().is_some() {
                let static_root = umbral::settings::get_opt()
                    .map(|s| s.static_root.clone())
                    .unwrap_or_else(|| "staticfiles".to_string());
                let backend: Arc<dyn Storage> =
                    Arc::new(FsStorage::new(String::new(), &static_root));
                umbral::storage::set_storage_named(STATICFILES, backend);
            }
        }

        Ok(())
    }

    fn provides_storage(&self) -> bool {
        // A media side registers the ambient "default" backend that the
        // boot `field.storage_backend` check looks for.
        self.media.is_some()
    }
}
