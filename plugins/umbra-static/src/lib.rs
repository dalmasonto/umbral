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
//! The plugin is a thin wrapper around `tower_http::services::ServeDir`.
//! That crate already handles MIME sniffing, range requests, and
//! `If-Modified-Since`. The plugin's job is to expose it through the
//! Plugin trait so the app builder, plugin ordering, and
//! `system_checks()` apply uniformly.
//!
//! ## Production note
//!
//! Behind a reverse proxy (nginx, Caddy, Cloudflare), serve static
//! files from the proxy and skip this plugin in prod. It exists for
//! development, single-binary deployments, and apps small enough
//! that the framework serving its own assets is cheaper than the
//! ops overhead of a separate file server.

use std::path::{Path, PathBuf};

use tower_http::services::ServeDir;
use umbra::prelude::*;

/// Serve every file under `dir` at the URL prefix `mount`.
///
/// `mount` is the URL path prefix (e.g. `"/static"`); requests to
/// `/<mount>/<rest>` look for `<dir>/<rest>` on disk and return 404
/// if it's missing. `dir` is any `AsRef<Path>` and is resolved at
/// route-build time, not boot time, so a relative path is relative
/// to the app's CWD when the request fires.
#[derive(Debug, Clone)]
pub struct StaticPlugin {
    mount: String,
    dir: PathBuf,
}

impl StaticPlugin {
    pub fn new(mount: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        Self {
            mount: mount.into(),
            dir: dir.as_ref().to_path_buf(),
        }
    }

    /// Mount path this plugin will serve from.
    pub fn mount(&self) -> &str {
        &self.mount
    }

    /// On-disk directory this plugin will read from.
    pub fn dir(&self) -> &Path {
        &self.dir
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
        // ServeDir is a tower Service, not an axum Handler. Mounting
        // via nest_service avoids axum's handler-trait coercion and
        // forwards the full path remainder to the service so files
        // in nested directories resolve correctly.
        Router::new().nest_service(&self.mount, ServeDir::new(&self.dir))
    }
}
