//! umbra-static — static file serving plugin.
//!
//! Django's `staticfiles` app, the small slice that matters for
//! umbra v0: serve a directory of assets at a chosen URL prefix.
//! Mount it on the app builder, point it at a folder, and every
//! file under that folder shows up at the prefix.
//!
//! ```ignore
//! App::builder()
//!     .plugin(StaticPlugin::new("/static", "./assets"))
//!     .build()
//!     .await?;
//! ```
//!
//! ## Cache headers
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
//! regardless of the configured value. This means browsers re-validate
//! every asset on every request in development, which prevents stale
//! CSS/JS from masking changes. In `Prod` and `Test` the configured
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
//! ops overhead of a separate file server.
//!
//! tower-http's [`ServeDir`] already handles MIME sniffing, range
//! requests, `If-Modified-Since`, and ETags — the plugin's job is to
//! expose it through the `Plugin` trait so the app builder, plugin
//! ordering, and `system_checks()` apply uniformly.

use std::path::{Path, PathBuf};
use std::time::Duration;

use http::header::{CACHE_CONTROL, HeaderValue};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use umbra::prelude::*;

/// Serve every file under `dir` at the URL prefix `mount`.
///
/// `mount` is the URL path prefix (e.g. `"/static"`); requests to
/// `/<mount>/<rest>` look for `<dir>/<rest>` on disk and return 404
/// if it's missing. `dir` is any `AsRef<Path>` and is resolved at
/// route-build time, not boot time, so a relative path is relative
/// to the app's CWD when the request fires.
///
/// Call `.max_age(duration)` to add `Cache-Control: public,
/// max-age=<seconds>` headers. Omit it (or pass `None`) to serve
/// with no cache directives. In `Environment::Dev` the effective
/// `max-age` is always 0 regardless of the configured value.
#[derive(Debug, Clone)]
pub struct StaticPlugin {
    mount: String,
    dir: PathBuf,
    /// `None` means no Cache-Control header is added.
    /// `Some(0)` means `Cache-Control: public, max-age=0` (disable caching).
    max_age_secs: Option<u64>,
}

impl StaticPlugin {
    pub fn new(mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        Self {
            mount: mount.into(),
            dir: dir.as_ref().to_path_buf(),
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

    /// On-disk directory this plugin will read from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The effective `max-age` in seconds after applying the dev-mode
    /// override. Returns `None` when no cache header should be added.
    fn effective_max_age(&self) -> Option<u64> {
        let configured = self.max_age_secs?;

        // Dev-mode override: always 0, regardless of the configured value.
        // We read the ambient settings; if they haven't been initialised
        // yet (test environment without a full App::build), fall back to
        // using the configured value as-is.
        if let Some(settings) = umbra::settings::get_opt() {
            if matches!(settings.environment, umbra::Environment::Dev) {
                return Some(0);
            }
        }

        Some(configured)
    }
}

impl Plugin for StaticPlugin {
    fn name(&self) -> &'static str {
        "static"
    }

    fn routes(&self) -> Router {
        // Warn at route-build time rather than via Plugin::system_checks().
        // The system-check API takes a function pointer (no closure
        // over `self`), so the per-instance directory path can't be
        // checked through that surface. ServeDir already returns
        // 404 for missing files; the warning just makes the misconfig
        // visible in dev.
        if !self.dir.exists() {
            tracing::warn!(
                target: "umbra_static",
                "directory `{}` does not exist; requests under `{}` will return 404",
                self.dir.display(),
                self.mount,
            );
        }

        let serve_dir = ServeDir::new(&self.dir);

        // Conditionally add Cache-Control based on effective max_age.
        match self.effective_max_age() {
            None => {
                // No cache header configured — serve as-is.
                Router::new().nest_service(&self.mount, serve_dir)
            }
            Some(secs) => {
                let header_value = HeaderValue::from_str(&format!("public, max-age={secs}"))
                    .unwrap_or_else(|_| HeaderValue::from_static("public, max-age=0"));
                let cache_layer = SetResponseHeaderLayer::overriding(CACHE_CONTROL, header_value);
                Router::new().nest_service(
                    &self.mount,
                    tower::ServiceBuilder::new()
                        .layer(cache_layer)
                        .service(serve_dir),
                )
            }
        }
    }
}
