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
use axum::http::{Method, Request, Response, StatusCode, header};
use bytes::Bytes;
use http::header::{CACHE_CONTROL, HeaderValue};
use include_dir::Dir;
use sha2::{Digest, Sha256};
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

    /// Whether this plugin defers to the framework's unified static
    /// pipeline instead of nesting its own route.
    ///
    /// True only for a *filesystem* plugin mounted AT the configured
    /// `static_url` (running inside a built `App`, so ambient settings
    /// exist). In that case the framework's single static handler owns
    /// `static_url`; this plugin contributes its directory as a root
    /// source via [`Plugin::static_root_dirs`] rather than nesting a
    /// second `/static/{*rest}` catch-all (which would collide with the
    /// pipeline mount and panic the build).
    ///
    /// Standalone use / tests have no ambient settings, so this is
    /// `false` and the plugin nests normally — its behaviour outside an
    /// `App` is unchanged. An embedded plugin, or one mounted at a
    /// different path (`/media`), also nests as before.
    fn defers_to_pipeline(&self) -> bool {
        if !matches!(self.source, Source::Fs(_)) {
            return false;
        }
        let Some(settings) = umbra::settings::get_opt() else {
            return false;
        };
        let norm = |p: &str| p.trim_matches('/').to_string();
        norm(&settings.static_url) == norm(&self.mount)
    }
}

impl Plugin for StaticPlugin {
    fn name(&self) -> &'static str {
        "static"
    }

    fn routes(&self) -> Router {
        // When mounted at the configured `static_url`, the framework's
        // unified pipeline owns that path — we contribute our directory
        // through `static_root_dirs()` instead of nesting a second
        // (conflicting) catch-all here. Return an empty router.
        if self.defers_to_pipeline() {
            return Router::new();
        }

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
                // Wrap ServeDir with a symlink-escape guard: canonicalize the
                // resolved path and reject (404) anything that escapes the
                // configured root. Without this, a symlink inside the root can
                // point to /etc/passwd (or anywhere outside), and tokio::fs::File
                // would follow it silently because ServeDir only blocks
                // Component::ParentDir (`..`), not post-symlink escapes.
                //
                // ServeDir returns Response<ServeFileSystemResponseBody>; map that
                // to Response<Body> so SymlinkGuardService has a uniform response
                // type for both the pass-through and the 404 short-circuit paths.
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

    /// When mounted at the configured `static_url`, contribute this
    /// plugin's filesystem directory as a root source for the framework's
    /// unified static handler (see [`Self::defers_to_pipeline`]). The
    /// handler serves `static_url/<file>` from it after trying namespaced
    /// plugin assets, so a project's own CSS / images live at the bare
    /// `/static/...` space without a second catch-all mount.
    fn static_root_dirs(&self) -> Vec<PathBuf> {
        match &self.source {
            Source::Fs(dir) if self.defers_to_pipeline() => vec![dir.clone()],
            _ => Vec::new(),
        }
    }

    /// Provide `collectstatic`, Django's asset-collection command, as a
    /// plugin-contributed CLI subcommand. It lives HERE — not as a
    /// built-in `umbra-cli` subcommand — so it is only available when a
    /// project registers `StaticPlugin`. A REST-free, static-free app
    /// never sees the command, matching the "thin core, plugin-heavy"
    /// rule: serving static assets is a plugin capability, so collecting
    /// them is too.
    fn commands(&self) -> Vec<Box<dyn umbra::cli::PluginCommand>> {
        vec![Box::new(CollectStaticCommand)]
    }
}

/// The `collectstatic` management command. Copies every registered
/// plugin's namespaced `static_dirs()` into
/// `<static_root>/<namespace>/` and every app/site `static_root_dirs()`
/// into the `<static_root>/` root, so prod (which serves only from
/// `static_root`) and CDN uploads have a complete on-disk tree.
///
/// It reads the static contributions from the ambient
/// [`umbra::static_files::published_static`] slot that `App::build`
/// populated — `PluginCommand::run` isn't handed the plugin list, so the
/// list is published at build time the same way `settings` is (a
/// read-only ambient set once at boot).
struct CollectStaticCommand;

#[async_trait::async_trait]
impl umbra::cli::PluginCommand for CollectStaticCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("collectstatic")
            .about(
                "Collect every plugin's static_dirs() (namespaced) and static_root_dirs() \
                 (site dirs) into settings.static_root. Django's collectstatic.",
            )
            .arg(
                clap::Arg::new("clear")
                    .long("clear")
                    .help(
                        "Empty static_root before collecting, dropping stale assets no plugin \
                         ships any more. Like Django's --clear. No confirmation prompt.",
                    )
                    .action(clap::ArgAction::SetTrue),
            )
    }

    async fn run(&self, matches: &clap::ArgMatches) -> Result<(), umbra::cli::CliError> {
        let clear = matches.get_flag("clear");
        let static_root = umbra::settings::get().static_root.clone();

        // The published contributions come from `App::build`. If they're
        // absent, the command was invoked without a built App — surface
        // that clearly rather than silently collecting nothing.
        let published = umbra::static_files::published_static().ok_or_else(|| -> umbra::cli::CliError {
            "collectstatic requires a built App; ensure App::build() ran before dispatching the \
             command (umbra-cli::dispatch is called with the built App)."
                .into()
        })?;

        let summary = umbra::static_files::collect_into(
            &published.contributions,
            &published.root_dirs,
            &static_root,
            clear,
        )?;

        // Warn about every declared-but-absent namespaced source dir.
        // These are misconfigurations (a plugin promised assets that
        // aren't on disk); surface them rather than swallowing silently.
        for missing in &summary.missing {
            eprintln!(
                "warning: collectstatic: plugin `{}` declares static namespace `{}` with source \
                 dir `{}`, which does not exist on disk — skipped.",
                missing.plugin,
                missing.namespace,
                missing.source_dir.display(),
            );
        }

        if summary.collected.is_empty() && summary.root_files == 0 {
            println!(
                "No static assets to collect (no plugin contributed an on-disk source or site dir)."
            );
            return Ok(());
        }

        for collected in &summary.collected {
            println!(
                "{} file(s) -> {}",
                collected.files,
                collected.destination.display(),
            );
        }
        if summary.root_files > 0 {
            println!(
                "{} site file(s) -> {}",
                summary.root_files,
                summary.static_root.display(),
            );
        }
        println!(
            "Collected {} file(s) into {}",
            summary.total_files() + summary.root_files,
            summary.static_root.display(),
        );
        Ok(())
    }
}

/// Compute a strong ETag for asset bytes: the first 16 hex chars of the
/// SHA-256 digest, wrapped in double-quotes as the HTTP spec requires.
/// 64 bits of content hash is plenty for cache-busting purposes while
/// keeping the header short.
fn etag_for(contents: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(contents);
    let digest = hasher.finalize();
    // Format first 8 bytes (16 hex chars) as the tag value.
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// Tower `Service` that resolves a request path against an embedded
/// `Dir` and returns the file bytes with a content-type guessed from
/// the extension. `Infallible` because every code path produces a
/// `Response` (a miss is a 404 response, not an `Err`).
///
/// `nest_service` strips the mount prefix before calling us, so the
/// path we see is already relative to the embedded root. A leading
/// `/` (axum normalises to one) is trimmed before the lookup.
///
/// ETag support: a strong ETag derived from the first 16 hex chars of
/// the SHA-256 of the asset bytes is emitted on every 200 response.
/// If the request carries `If-None-Match` with a matching ETag value
/// a `304 Not Modified` is returned with no body, letting the browser
/// reuse its cached copy without a full re-download.
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
            // The filesystem ServeDir path already 405s other methods; the
            // embedded path used to return 200 + the body for POST/PUT/etc.
            if !matches!(*req.method(), Method::GET | Method::HEAD) {
                return Ok(method_not_allowed_response());
            }
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

            let contents = file.contents();
            let etag = etag_for(contents);

            // Conditional GET: if the client's If-None-Match matches our ETag,
            // return 304 with no body. The comparison is exact (strong ETag):
            // the spec allows the value to be a comma-separated list of quoted
            // tags or `*`; we check for `*` and for the exact quoted string.
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
/// `ServeDir` blocks path traversal via `..` components but does not
/// canonicalize the resolved path — a symlink inside the served root that
/// points to a file outside the root (e.g. `/etc/passwd`) will be followed
/// silently by `tokio::fs::File::open`. This wrapper:
///
/// 1. Builds the candidate path the same way `ServeDir` would (root + decoded
///    normal components).
/// 2. Canonicalizes that candidate with `std::fs::canonicalize` (resolves all
///    symlinks).
/// 3. Rejects with 404 if the canonical path is NOT a prefix of the canonical
///    root, indicating the symlink pointed outside the root.
///
/// If `std::fs::canonicalize` fails (e.g. the file doesn't exist — a 404
/// that ServeDir would return anyway), we let ServeDir handle it normally.
/// Paths that contain `..` or are otherwise syntactically invalid return
/// 404 immediately without consulting ServeDir.
#[derive(Clone)]
struct SymlinkGuardService<S> {
    /// Canonical absolute path of the served root directory.
    canonical_root: Option<PathBuf>,
    /// Configured root path (used when canonical_root is unavailable).
    root: PathBuf,
    inner: S,
}

impl<S> SymlinkGuardService<S> {
    fn new(root: PathBuf, inner: S) -> Self {
        // Canonicalize at construction time. If the directory doesn't exist
        // yet, canonical_root is None and we skip the guard (ServeDir will 404
        // on every request anyway).
        let canonical_root = std::fs::canonicalize(&root).ok();
        Self {
            canonical_root,
            root,
            inner,
        }
    }

    /// Decode the request URI path into a candidate on-disk path relative to
    /// the root, applying the same component filtering as ServeDir
    /// (Normal only; reject Prefix/RootDir/ParentDir).
    fn candidate_path(&self, uri_path: &str) -> Option<PathBuf> {
        let decoded = percent_encoding::percent_decode_str(uri_path.trim_start_matches('/'))
            .decode_utf8()
            .ok()?;
        let mut candidate = self.root.clone();
        for component in std::path::Path::new(&*decoded).components() {
            match component {
                std::path::Component::Normal(c) => candidate.push(c),
                std::path::Component::CurDir => {}
                _ => return None, // Prefix / RootDir / ParentDir — reject
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
        // If canonical_root is None the directory doesn't exist; skip guard
        // and let ServeDir return 404 as normal.
        let Some(canonical_root) = self.canonical_root.clone() else {
            let fut = self.inner.call(req);
            return Box::pin(fut);
        };

        // Build the candidate path. A None return means a syntactically
        // invalid path (contains `..` etc.) — 404 immediately.
        let candidate = match self.candidate_path(req.uri().path()) {
            Some(p) => p,
            None => return Box::pin(std::future::ready(Ok(not_found_response()))),
        };

        // Canonicalize the candidate to resolve all symlinks.
        //
        // Three cases:
        //   Ok(canonical) — file exists; accept only if it's within the root.
        //   Err(NotFound) — file doesn't exist; let ServeDir return its own 404.
        //   Err(_)        — any other error (ELOOP for a symlink loop, EACCES,
        //                   etc.): return 404 ourselves so the caller gets a
        //                   clean "not found", not a 500. ServeDir would turn
        //                   most of these into 500; we want 404 for safety.
        match std::fs::canonicalize(&candidate) {
            Ok(canonical_candidate) => {
                // Reject if the canonical candidate escapes the canonical root.
                if !canonical_candidate.starts_with(&canonical_root) {
                    return Box::pin(std::future::ready(Ok(not_found_response())));
                }
                // Within root — proceed.
                let fut = self.inner.call(req);
                Box::pin(fut)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Non-existent file: pass to ServeDir which returns 404.
                let fut = self.inner.call(req);
                Box::pin(fut)
            }
            // Symlink loop, permission denied, or any other IO error: 404.
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
