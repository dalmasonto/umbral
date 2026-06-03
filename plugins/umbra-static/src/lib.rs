//! umbra-static — static file serving plugin.
//!
//! Django's `staticfiles` app, the small slice that matters for
//! umbra v0: serve a tree of assets at a chosen URL prefix. Two
//! source shapes are supported and mount through the same plugin:
//!
//! ## Filesystem (dev / single-binary deployments)
//!
//! ```ignore
//! App::builder()
//!     .plugin(StaticPlugin::new("/static", "./assets"))
//!     .build()
//!     .await?;
//! ```
//!
//! Wraps `tower_http::ServeDir`. Picks up MIME sniffing, range
//! requests, `If-Modified-Since`, ETags. The directory is resolved
//! relative to the app's CWD at request time, so a relative path
//! follows the binary, not the source tree.
//!
//! ## Embedded (plugin-ships-its-own-UI)
//!
//! ```ignore
//! use include_dir::{Dir, include_dir};
//!
//! static ASSETS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/dist");
//!
//! App::builder()
//!     .plugin(StaticPlugin::embedded("/widget/assets", &ASSETS))
//!     .build()
//!     .await?;
//! ```
//!
//! The asset tree is baked into the binary at compile time. The
//! runtime serves bytes straight from memory — no filesystem read,
//! no path canonicalisation, no risk of a deleted/renamed file
//! orphaning live browser tabs. Path traversal is structurally
//! impossible because lookups are a `Dir::get_file(rel)` tree
//! walk, not a path join.
//!
//! Embedded mode is what an *embeddable plugin* should use: the
//! plugin's UI travels with the plugin's crate, so when a user
//! drops the plugin into their app, the assets come along. No
//! "remember to deploy a `dist/` directory next to the binary"
//! footgun.
//!
//! ## Cache headers (both modes)
//!
//! By default every response carries no cache headers — the browser
//! decides. Call [`StaticPlugin::max_age`] to add
//! `Cache-Control: public, max-age=<seconds>` on every response:
//!
//! ```ignore
//! use std::time::Duration;
//!
//! StaticPlugin::new("/static", "./assets")
//!     .max_age(Duration::from_secs(86400)) // 1 day
//! ```
//!
//! **Dev-mode opt-out.** When [`umbra::settings::get`] returns
//! `Environment::Dev`, the effective `max_age` is forced to `0`
//! regardless of the configured value. This prevents stale CSS/JS
//! from masking changes in dev. In `Prod` and `Test` the configured
//! value is used as-is.
//!
//! The dev-mode check reads the live ambient settings, so it happens
//! at route-build time (when [`Plugin::routes`] is called), not at
//! constructor time. An app that calls `StaticPlugin::new` before
//! `App::build` but builds routes after will always see the correct
//! environment.
//!
//! ## Production note
//!
//! Behind a reverse proxy (nginx, Caddy, Cloudflare), serve static
//! files from the proxy and skip this plugin in prod. It exists for
//! development, single-binary deployments, and apps small enough
//! that the framework serving its own assets is cheaper than the
//! ops overhead of a separate file server. (Embedded mode is the
//! exception — there the assets *are* the binary, so the reverse-
//! proxy story doesn't apply.)

use std::convert::Infallible;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use bytes::Bytes;
use http::header::{CACHE_CONTROL, HeaderValue};
use include_dir::Dir;
use tower::Service;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use umbra::prelude::*;

/// Where this plugin gets its asset bytes from. Constructors pick
/// the variant — callers don't touch the enum.
#[derive(Debug, Clone)]
enum Source {
    /// Filesystem directory, served via `tower_http::ServeDir`.
    Fs(PathBuf),
    /// `include_dir!()`-embedded asset tree, served by our in-memory
    /// service. The `'static` lifetime is the only one a compile-
    /// time embed can produce.
    Embedded(&'static Dir<'static>),
}

/// Serve every file under the configured source at the URL prefix
/// `mount`. Two source shapes — filesystem or embedded — both pass
/// through the same plugin and same cache-header logic.
///
/// See module docs for the two constructor patterns
/// ([`StaticPlugin::new`] for filesystem, [`StaticPlugin::embedded`]
/// for `include_dir!`-baked assets).
#[derive(Debug, Clone)]
pub struct StaticPlugin {
    mount: String,
    source: Source,
    /// `None` means no Cache-Control header is added.
    /// `Some(0)` means `Cache-Control: public, max-age=0` (disable caching).
    max_age_secs: Option<u64>,
}

impl StaticPlugin {
    /// Serve a *filesystem* directory at `mount`.
    ///
    /// `mount` is the URL path prefix (e.g. `"/static"`); requests to
    /// `/<mount>/<rest>` look for `<dir>/<rest>` on disk and return
    /// 404 if it's missing. `dir` is any `AsRef<Path>` and is
    /// resolved at route-build time, not boot time, so a relative
    /// path is relative to the app's CWD when the request fires.
    pub fn new(mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        Self {
            mount: mount.into(),
            source: Source::Fs(dir.as_ref().to_path_buf()),
            max_age_secs: None,
        }
    }

    /// Serve a compile-time-embedded asset tree at `mount`.
    ///
    /// `dir` is a `&'static Dir<'static>` produced by
    /// [`include_dir::include_dir!`]. Bytes come out of the binary
    /// directly. MIME types are inferred from the file extension via
    /// `mime_guess`; path traversal is structurally impossible
    /// (lookups are a tree walk against in-memory keys, not a path
    /// join against the filesystem).
    ///
    /// ```ignore
    /// use include_dir::{Dir, include_dir};
    /// use umbra_static::StaticPlugin;
    ///
    /// static ASSETS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/dist");
    ///
    /// StaticPlugin::embedded("/widget/assets", &ASSETS)
    /// ```
    pub fn embedded(mount: impl Into<String>, dir: &'static Dir<'static>) -> Self {
        Self {
            mount: mount.into(),
            source: Source::Embedded(dir),
            max_age_secs: None,
        }
    }

    /// Set a `Cache-Control: public, max-age=<duration>` header on every
    /// static response.
    ///
    /// In `Environment::Dev` the effective max-age is forced to `0`,
    /// preventing stale assets from masking changes during development.
    /// Pass `Duration::ZERO` explicitly to opt into that behaviour
    /// in all environments.
    pub fn max_age(mut self, duration: Duration) -> Self {
        self.max_age_secs = Some(duration.as_secs());
        self
    }

    /// Mount path this plugin will serve from.
    pub fn mount(&self) -> &str {
        &self.mount
    }

    /// On-disk directory this plugin will read from, when the plugin
    /// was constructed with [`Self::new`]. Returns `None` for
    /// embedded sources.
    pub fn dir(&self) -> Option<&Path> {
        match &self.source {
            Source::Fs(p) => Some(p.as_path()),
            Source::Embedded(_) => None,
        }
    }

    /// The effective `max-age` in seconds after applying the dev-mode
    /// override. Returns `None` when no cache header should be added.
    fn effective_max_age(&self) -> Option<u64> {
        let configured = self.max_age_secs?;

        // Dev-mode override: always 0, regardless of the configured value.
        // We read the ambient settings; if they haven't been initialised
        // yet (test environment without a full App::build), fall back to
        // using the configured value as-is.
        if let Some(settings) = umbra::settings::get_opt()
            && matches!(settings.environment, umbra::Environment::Dev)
        {
            return Some(0);
        }

        Some(configured)
    }
}

impl Plugin for StaticPlugin {
    fn name(&self) -> &'static str {
        "static"
    }

    fn routes(&self) -> Router {
        // Warn for filesystem mode when the directory doesn't exist.
        // Embedded mode can't have this misconfig (the macro would
        // have failed at compile time), so the warning is skipped.
        if let Source::Fs(dir) = &self.source
            && !dir.exists()
        {
            tracing::warn!(
                target: "umbra_static",
                "directory `{}` does not exist; requests under `{}` will return 404",
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
                let serve_dir = ServeDir::new(dir);
                match cache_layer {
                    None => Router::new().nest_service(&self.mount, serve_dir),
                    Some(layer) => Router::new().nest_service(
                        &self.mount,
                        tower::ServiceBuilder::new().layer(layer).service(serve_dir),
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
}

/// Tower `Service` that resolves a request path against an embedded
/// `Dir` and returns the file bytes with a content-type guessed from
/// the extension. `Infallible` because every code path produces a
/// `Response` (a miss is a 404 response, not an `Err`).
///
/// `nest_service` strips the mount prefix before calling us, so the
/// path we see is already relative to the embedded root. A leading
/// `/` (axum normalises to one) is trimmed before the lookup.
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
            let rel = req.uri().path().trim_start_matches('/');
            // Empty path = root of the mount. We don't synthesise an
            // index.html; callers that want one should serve it via
            // their own handler (see how umbra-playground renders its
            // shell.html separately and only delegates `assets/*`).
            if rel.is_empty() {
                return Ok(not_found_response());
            }
            let Some(file) = dir.get_file(rel) else {
                return Ok(not_found_response());
            };

            let content_type = mime_guess::from_path(rel)
                .first_or_octet_stream()
                .to_string();

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(Bytes::from_static(file.contents())))
                .expect("static response is always valid"))
        })
    }
}

fn not_found_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("not found"))
        .expect("static 404 response is always valid")
}
