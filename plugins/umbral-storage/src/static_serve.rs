//! Static-file serving: the filesystem `ServeDir` + symlink-escape guard,
//! the `include_dir!`-embedded in-memory service, ETag / conditional-GET,
//! and dev-mode cache-header logic.
//!
//! Moved verbatim from the former `umbral-static` crate. The two source
//! shapes (filesystem / embedded) build through one path and share the
//! same cache-header logic. The symlink-escape guard and the embedded
//! tree's structural path-traversal immunity are preserved unchanged.

use std::convert::Infallible;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, header};
use bytes::Bytes;
use http::header::{CACHE_CONTROL, HeaderValue};
use include_dir::Dir;
use sha2::{Digest, Sha256};
use tower::Service;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use umbral::prelude::*;

/// Where the static side gets its asset bytes from. Constructors pick the
/// variant — callers don't touch the enum.
#[derive(Debug, Clone)]
pub(crate) enum Source {
    /// Filesystem directory, served via `tower_http::ServeDir`.
    Fs(PathBuf),
    /// `include_dir!()`-embedded asset tree, served by our in-memory
    /// service. The `'static` lifetime is the only one a compile-time
    /// embed can produce.
    Embedded(&'static Dir<'static>),
}

/// The static-serving half of [`crate::StoragePlugin`]: a `mount`, a
/// [`Source`], and an optional cache `max-age`. Builds the same routes the
/// old `StaticPlugin` did.
#[derive(Debug, Clone)]
pub(crate) struct StaticServe {
    pub(crate) mount: String,
    pub(crate) source: Source,
    /// `None` means no Cache-Control header is added.
    /// `Some(0)` means `Cache-Control: public, max-age=0`.
    pub(crate) max_age_secs: Option<u64>,
}

impl StaticServe {
    /// The effective `max-age` in seconds after applying the dev-mode
    /// override (always 0 in `Environment::Dev`). `None` → no header.
    fn effective_max_age(&self) -> Option<u64> {
        let configured = self.max_age_secs?;
        if let Some(settings) = umbral::settings::get_opt()
            && matches!(settings.environment, umbral::Environment::Dev)
        {
            return Some(0);
        }
        Some(configured)
    }

    /// True only for a *filesystem* source mounted AT the configured
    /// `static_url` (running inside a built `App`). In that case the
    /// framework's unified static handler owns `static_url`; this side
    /// contributes its directory via `static_root_dirs` rather than
    /// nesting a second `/static/{*rest}` catch-all (which would collide
    /// with the pipeline mount and panic the build).
    pub(crate) fn defers_to_pipeline(&self) -> bool {
        if !matches!(self.source, Source::Fs(_)) {
            return false;
        }
        let Some(settings) = umbral::settings::get_opt() else {
            return false;
        };
        let norm = |p: &str| p.trim_matches('/').to_string();
        norm(&settings.static_url) == norm(&self.mount)
    }

    /// On-disk directory when this is a filesystem source.
    pub(crate) fn dir(&self) -> Option<&Path> {
        match &self.source {
            Source::Fs(p) => Some(p.as_path()),
            Source::Embedded(_) => None,
        }
    }

    /// Build the static-serving router for this side. Returns an empty
    /// router when it defers to the framework's unified pipeline.
    pub(crate) fn routes(&self) -> Router {
        if self.defers_to_pipeline() {
            return Router::new();
        }

        if let Source::Fs(dir) = &self.source
            && !dir.exists()
        {
            tracing::warn!(
                target: "umbral_storage",
                "static directory `{}` does not exist; requests under `{}` will return 404",
                dir.display(),
                self.mount,
            );
        }

        let cache_layer = self.effective_max_age().map(|secs| {
            let header_value = HeaderValue::from_str(&format!("public, max-age={secs}"))
                .unwrap_or_else(|_| HeaderValue::from_static("public, max-age=0"));
            SetResponseHeaderLayer::overriding(CACHE_CONTROL, header_value)
        });

        match &self.source {
            Source::Fs(dir) => {
                // Wrap ServeDir with a symlink-escape guard: canonicalize the
                // resolved path and reject (404) anything that escapes the
                // configured root. Without this, a symlink inside the root can
                // point to /etc/passwd, and tokio::fs::File would follow it
                // silently because ServeDir only blocks `..`, not post-symlink
                // escapes.
                let inner = tower::ServiceBuilder::new()
                    .map_response(|resp: Response<_>| resp.map(Body::new))
                    .service(ServeDir::new(dir));
                let svc = SymlinkGuardService::new(dir.clone(), inner);
                match cache_layer {
                    None => Router::new().nest_service(&self.mount, svc),
                    Some(layer) => Router::new().nest_service(
                        &self.mount,
                        tower::ServiceBuilder::new().layer(layer).service(svc),
                    ),
                }
            }
            Source::Embedded(dir) => {
                let svc = EmbeddedDirService { dir };
                match cache_layer {
                    None => Router::new().nest_service(&self.mount, svc),
                    Some(layer) => Router::new().nest_service(
                        &self.mount,
                        tower::ServiceBuilder::new().layer(layer).service(svc),
                    ),
                }
            }
        }
    }

    /// Contribute this side's filesystem directory as a pipeline root
    /// source when it defers to the unified static handler.
    pub(crate) fn static_root_dirs(&self) -> Vec<PathBuf> {
        match &self.source {
            Source::Fs(dir) if self.defers_to_pipeline() => vec![dir.clone()],
            _ => Vec::new(),
        }
    }

    fn max_age(mut self, duration: Duration) -> Self {
        self.max_age_secs = Some(duration.as_secs());
        self
    }
}

impl StaticServe {
    /// Filesystem source at `mount`.
    pub(crate) fn fs(mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        Self {
            mount: mount.into(),
            source: Source::Fs(dir.as_ref().to_path_buf()),
            max_age_secs: None,
        }
    }

    /// Embedded `include_dir!` source at `mount`.
    pub(crate) fn embedded(mount: impl Into<String>, dir: &'static Dir<'static>) -> Self {
        Self {
            mount: mount.into(),
            source: Source::Embedded(dir),
            max_age_secs: None,
        }
    }

    /// Set the cache `max-age`.
    pub(crate) fn with_max_age(self, duration: Duration) -> Self {
        self.max_age(duration)
    }
}

/// Compute a strong ETag for asset bytes: the first 16 hex chars of the
/// SHA-256 digest, wrapped in double-quotes as the HTTP spec requires.
fn etag_for(contents: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(contents);
    let digest = hasher.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// Tower `Service` that resolves a request path against an embedded `Dir`
/// and returns the file bytes with a content-type guessed from the
/// extension. ETag + conditional-GET supported.
#[derive(Clone)]
struct EmbeddedDirService {
    dir: &'static Dir<'static>,
}

impl Service<Request<Body>> for EmbeddedDirService {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let dir = self.dir;
        Box::pin(async move {
            // BROKEN-11: static assets are read-only — answer only GET/HEAD.
            if !matches!(*req.method(), Method::GET | Method::HEAD) {
                return Ok(method_not_allowed_response());
            }
            let rel = req.uri().path().trim_start_matches('/');
            if rel.is_empty() {
                return Ok(not_found_response());
            }
            let Some(file) = dir.get_file(rel) else {
                return Ok(not_found_response());
            };

            let contents = file.contents();
            let etag = etag_for(contents);

            let if_none_match = req
                .headers()
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if if_none_match == "*"
                || if_none_match
                    .split(',')
                    .map(|t| t.trim())
                    .any(|t| t == etag)
            {
                return Ok(Response::builder()
                    .status(StatusCode::NOT_MODIFIED)
                    .header(header::ETAG, &etag)
                    .body(Body::empty())
                    .expect("304 response is always valid"));
            }

            let content_type = mime_guess::from_path(rel)
                .first_or_octet_stream()
                .to_string();

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ETAG, &etag)
                .body(Body::from(Bytes::from_static(contents)))
                .expect("static response is always valid"))
        })
    }
}

/// Tower `Service` that wraps a `ServeDir` with a symlink-escape guard.
///
/// `ServeDir` blocks `..` traversal but does not canonicalize the resolved
/// path — a symlink inside the served root pointing outside it would be
/// followed silently. This wrapper canonicalizes the candidate and rejects
/// (404) anything that escapes the canonical root.
#[derive(Clone)]
struct SymlinkGuardService<S> {
    canonical_root: Option<PathBuf>,
    root: PathBuf,
    inner: S,
}

impl<S> SymlinkGuardService<S> {
    fn new(root: PathBuf, inner: S) -> Self {
        let canonical_root = std::fs::canonicalize(&root).ok();
        Self {
            canonical_root,
            root,
            inner,
        }
    }

    fn candidate_path(&self, uri_path: &str) -> Option<PathBuf> {
        let decoded = percent_encoding::percent_decode_str(uri_path.trim_start_matches('/'))
            .decode_utf8()
            .ok()?;
        let mut candidate = self.root.clone();
        for component in std::path::Path::new(&*decoded).components() {
            match component {
                std::path::Component::Normal(c) => candidate.push(c),
                std::path::Component::CurDir => {}
                _ => return None,
            }
        }
        Some(candidate)
    }
}

impl<S> Service<Request<Body>> for SymlinkGuardService<S>
where
    S: Service<Request<Body>, Response = Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let Some(canonical_root) = self.canonical_root.clone() else {
            let fut = self.inner.call(req);
            return Box::pin(fut);
        };

        let candidate = match self.candidate_path(req.uri().path()) {
            Some(p) => p,
            None => return Box::pin(std::future::ready(Ok(not_found_response()))),
        };

        match std::fs::canonicalize(&candidate) {
            Ok(canonical_candidate) => {
                if !canonical_candidate.starts_with(&canonical_root) {
                    return Box::pin(std::future::ready(Ok(not_found_response())));
                }
                let fut = self.inner.call(req);
                Box::pin(fut)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let fut = self.inner.call(req);
                Box::pin(fut)
            }
            Err(_) => Box::pin(std::future::ready(Ok(not_found_response()))),
        }
    }
}

fn not_found_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("not found"))
        .expect("static 404 response is always valid")
}

fn method_not_allowed_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(header::ALLOW, "GET, HEAD")
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("method not allowed"))
        .expect("static 405 response is always valid")
}
