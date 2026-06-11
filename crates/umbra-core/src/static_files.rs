//! The unified static-asset pipeline's request → file resolution.
//!
//! This module is the runtime half of [`Plugin::static_dirs`]. At
//! `App::build()` the framework walks every plugin's `static_dirs()`
//! into a [`StaticRegistry`] (`namespace -> source_dir`) and mounts one
//! handler at the configured `static_url` (default `/static/`). A
//! request `/static/<namespace>/<rest>` resolves like so:
//!
//! - **Dev** ([`Environment::Dev`]) — try `<source_dir>/<rest>` from the
//!   registry first (LIVE source serving: drop a rebuilt file, served on
//!   the next request). If the namespace isn't registered OR the file is
//!   missing, fall back to `<static_root>/<namespace>/<rest>`.
//! - **Prod / Test** — serve `<static_root>/<namespace>/<rest>` only.
//!
//! Every resolution runs through [`resolve_under_root`], which rejects
//! `..` escapes, absolute components, and symlink traversal by
//! canonicalising the candidate and verifying it still lives under the
//! intended root. A path that escapes is a 404 (never a 403 that would
//! leak the attempted filename).
//!
//! ## One file-serving implementation
//!
//! [`serve_file`] is the single place the framework reads a file off
//! disk and turns it into a response — Content-Type, ETag, range
//! requests, and `If-Modified-Since` all come from `tower_http`'s
//! `ServeFile`. The unified handler here routes every file response
//! through it, so MIME / range / conditional-request handling lives in
//! one spot. It is re-exported from the facade
//! (`umbra::static_files::serve_file`) so a plugin that needs to serve
//! a single file off disk can reuse it instead of hand-rolling the same
//! logic; the standalone `umbra-static` `StaticPlugin` keeps its own
//! `ServeDir`/`include_dir` paths (a directory tree and an embedded
//! tree are different shapes from a single-file serve) and is not
//! rewired onto this primitive in this slice. The dev `max-age=0` /
//! prod cache behaviour is applied here too.
//!
//! [`Plugin::static_dirs`]: crate::plugin::Plugin::static_dirs
//! [`Environment::Dev`]: crate::settings::Environment

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, Response, StatusCode, header};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::plugin::{Plugin, StaticDir};

/// Maps a plugin's static namespace to its on-disk source directory.
///
/// Built once at `App::build()` from every registered plugin's
/// [`Plugin::static_dirs`]. Cloned into the static handler's axum state
/// so per-request resolution is a cheap `HashMap` lookup.
#[derive(Debug, Clone, Default)]
pub struct StaticRegistry {
    by_namespace: HashMap<&'static str, PathBuf>,
}

/// Two plugins declared the same static namespace. Carries the
/// colliding namespace plus both plugin names so the boot-time error
/// names exactly who collided.
#[derive(Debug, Clone)]
pub struct StaticNamespaceCollision {
    /// The namespace both plugins claimed.
    pub namespace: &'static str,
    /// The plugin that registered the namespace first.
    pub first_plugin: &'static str,
    /// The plugin that tried to register it again.
    pub second_plugin: &'static str,
}

impl StaticRegistry {
    /// Walk every plugin's `static_dirs()` into a `namespace -> source_dir`
    /// map. A namespace claimed by two plugins is a hard error — the
    /// collision must fail the build loudly, never silently shadow one
    /// plugin's assets with another's.
    ///
    /// `plugins` is borrowed in topological order; the first plugin to
    /// claim a namespace owns it, and a later claimant surfaces as
    /// [`StaticNamespaceCollision`] naming both sides.
    pub fn from_plugins(plugins: &[Box<dyn Plugin>]) -> Result<Self, StaticNamespaceCollision> {
        let mut by_namespace: HashMap<&'static str, PathBuf> = HashMap::new();
        // Track which plugin claimed each namespace so a collision can
        // name both sides, not just the loser.
        let mut owner: HashMap<&'static str, &'static str> = HashMap::new();

        for plugin in plugins {
            for dir in plugin.static_dirs() {
                let StaticDir {
                    namespace,
                    source_dir,
                } = dir;
                if let Some(&first_plugin) = owner.get(namespace) {
                    return Err(StaticNamespaceCollision {
                        namespace,
                        first_plugin,
                        second_plugin: plugin.name(),
                    });
                }
                owner.insert(namespace, plugin.name());
                by_namespace.insert(namespace, source_dir);
            }
        }

        Ok(Self { by_namespace })
    }

    /// The source directory a namespace was registered with, if any.
    pub fn source_dir(&self, namespace: &str) -> Option<&Path> {
        self.by_namespace.get(namespace).map(PathBuf::as_path)
    }

    /// True when no plugin contributed a static dir. The handler is
    /// still mounted (so `static_root` serving works in prod) but this
    /// lets `App::build` skip the mount entirely when there's nothing
    /// to serve AND no static_root convention is wanted.
    pub fn is_empty(&self) -> bool {
        self.by_namespace.is_empty()
    }
}

/// Split a request path that has already had the `static_url` base
/// stripped into `(namespace, rest)`.
///
/// `"admin/admin.css"` → `("admin", "admin.css")`.
/// `"admin/css/site.css"` → `("admin", "css/site.css")`.
/// A path with no `/` (just a namespace, no file) yields `None` — there
/// is nothing to serve at a bare namespace root.
fn split_namespace(rel: &str) -> Option<(&str, &str)> {
    let rel = rel.trim_start_matches('/');
    let (ns, rest) = rel.split_once('/')?;
    if ns.is_empty() || rest.is_empty() {
        return None;
    }
    Some((ns, rest))
}

/// Resolve `rel` against `root`, returning the on-disk path ONLY if it
/// stays inside `root` after canonicalisation.
///
/// The defence is three-layered:
///
/// 1. **Lexical reject** — any `..` (`ParentDir`), absolute prefix
///    (`RootDir` / `Prefix`), is refused before touching the filesystem.
///    This blocks the `../../etc/passwd` family up front.
/// 2. **Canonicalise** — resolve symlinks and `.` segments to a real
///    absolute path. A symlink inside `root` pointing outside it is
///    caught here, where a purely lexical check would miss it.
/// 3. **Containment** — verify the canonical candidate is still prefixed
///    by the canonical root. Anything escaping returns `None`.
///
/// Returns `None` (caller maps to 404) on any failure — a miss and an
/// escape attempt are indistinguishable to the client, so a probe can't
/// learn whether a path exists outside the root.
pub fn resolve_under_root(root: &Path, rel: &str) -> Option<PathBuf> {
    // Layer 1: lexical rejection. Reject before any filesystem access.
    let rel_path = Path::new(rel);
    for component in rel_path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            // ParentDir (`..`), RootDir (`/...`), Prefix (`C:\`) all
            // escape or absolutise — refuse outright.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    let candidate = root.join(rel_path);

    // Layer 2: canonicalise both sides. If the file doesn't exist,
    // `canonicalize` errors -> None (a 404), which is exactly right.
    let canonical_root = root.canonicalize().ok()?;
    let canonical_candidate = candidate.canonicalize().ok()?;

    // Layer 3: containment. The canonical candidate must live under the
    // canonical root — this catches a symlink inside `root` that points
    // out of it (lexical checks alone would let it through).
    if canonical_candidate.starts_with(&canonical_root) {
        Some(canonical_candidate)
    } else {
        None
    }
}

/// Serve a single on-disk file as an HTTP response, reusing
/// `tower_http::ServeFile` for Content-Type, ETag, range, and
/// `If-Modified-Since` handling. This is the framework's ONE
/// file-serving path — the unified static handler and `umbra-static`
/// both route through it.
///
/// `dev` forces `Cache-Control: no-cache` so a rebuilt asset is never
/// masked by a stale cached copy during development; in prod the
/// response carries whatever `ServeFile` set (typically none, leaving
/// the caching decision to a reverse proxy or the browser).
///
/// `req` is forwarded so conditional and range headers (`If-None-Match`,
/// `Range`, `If-Modified-Since`) reach `ServeFile` and produce `304` /
/// `206` as appropriate.
pub async fn serve_file(file_path: &Path, dev: bool, req: Request<Body>) -> Response<Body> {
    // ServeFile is infallible at the Service level — a missing file is a
    // 404 *response*, not an Err — so `oneshot` can't actually fail. The
    // match keeps us honest if tower ever changes that contract.
    let response = match ServeFile::new(file_path).oneshot(req).await {
        Ok(resp) => resp,
        Err(_unreachable) => {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("static file serving failed"))
                .expect("static 500 response is always valid");
        }
    };

    let mut response = response.map(Body::new);

    if dev {
        // Replace any cache header ServeFile may have set with an
        // explicit no-cache so dev edits are always picked up.
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-cache"),
        );
    }

    response
}

/// The axum handler mounted at the `static_url` base. Resolves the
/// request path (already stripped of the base prefix by the nested
/// mount) per the dev/prod algorithm and serves the file via
/// [`serve_file`].
///
/// State carries the [`StaticRegistry`], the resolved `static_root`
/// (absolute or CWD-relative on-disk dir), and the `dev` flag captured
/// at build time.
pub async fn static_handler(
    State(state): State<StaticHandlerState>,
    req: Request<Body>,
) -> Response<Body> {
    // `nest_service` strips the mount prefix, so `req.uri().path()` is
    // already relative to the static base: `/admin/admin.css`.
    let path = req.uri().path().to_string();
    let Some((namespace, rest)) = split_namespace(&path) else {
        return not_found();
    };

    // Dev: try the plugin's live source dir first.
    if state.dev {
        if let Some(source_dir) = state.registry.source_dir(namespace) {
            if let Some(resolved) = resolve_under_root(source_dir, rest) {
                return serve_file(&resolved, true, req).await;
            }
        }
        // Fall through to the static_root fallback below.
    }

    // Prod, OR dev-fallback: <static_root>/<namespace>/<rest>.
    let ns_root = state.static_root.join(namespace);
    if let Some(resolved) = resolve_under_root(&ns_root, rest) {
        return serve_file(&resolved, state.dev, req).await;
    }

    not_found()
}

/// Immutable state the static handler closes over: the namespace
/// registry, the on-disk collected-assets root, and the dev flag.
#[derive(Debug, Clone)]
pub struct StaticHandlerState {
    /// `namespace -> source_dir`, built from plugins at boot.
    pub registry: StaticRegistry,
    /// On-disk root the collected/prod assets live under
    /// (`settings.static_root`, e.g. `staticfiles/`).
    pub static_root: PathBuf,
    /// Whether the app is running in `Environment::Dev`.
    pub dev: bool,
}

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("not found"))
        .expect("static 404 response is always valid")
}

/// Per-namespace result of a [`collect_static`] run: how many files were
/// copied into `<static_root>/<namespace>/` and where they landed.
#[derive(Debug, Clone)]
pub struct CollectedNamespace {
    /// The plugin namespace these files were collected under.
    pub namespace: &'static str,
    /// The plugin that contributed this namespace.
    pub plugin: &'static str,
    /// Count of files copied (not directories) for this namespace.
    pub files: usize,
    /// The destination directory (`<static_root>/<namespace>`).
    pub destination: PathBuf,
}

/// A plugin declared a `source_dir` that doesn't exist on disk. Recorded
/// (not fatal) so the CLI can surface the misconfiguration to the dev
/// without aborting the whole collect — every other plugin still
/// collects.
#[derive(Debug, Clone)]
pub struct MissingSourceDir {
    /// The namespace whose source dir is missing.
    pub namespace: &'static str,
    /// The plugin that declared the missing dir.
    pub plugin: &'static str,
    /// The path that was declared but isn't present on disk.
    pub source_dir: PathBuf,
}

/// The outcome of a [`collect_static`] run. Carries the per-namespace
/// breakdown, any skipped (missing-source) namespaces, and the resolved
/// `static_root` so the CLI can print a summary.
#[derive(Debug, Clone, Default)]
pub struct CollectSummary {
    /// One entry per namespace that had an on-disk source dir.
    pub collected: Vec<CollectedNamespace>,
    /// Namespaces whose declared source dir was absent (warned, not
    /// fatal).
    pub missing: Vec<MissingSourceDir>,
    /// The destination root every namespace was collected under.
    pub static_root: PathBuf,
}

impl CollectSummary {
    /// Total files copied across every namespace.
    pub fn total_files(&self) -> usize {
        self.collected.iter().map(|c| c.files).sum()
    }
}

/// Anything that can go wrong collecting static assets. A namespace
/// collision is detected up front (before any copying) so a misconfigured
/// app never half-writes its `static_root`.
#[derive(Debug)]
pub enum CollectError {
    /// Two plugins claimed the same namespace. Collected NOTHING — the
    /// collision is detected before any file is touched.
    Collision(StaticNamespaceCollision),
    /// An IO error creating a directory or copying a file. Carries the
    /// path that failed so the message names the culprit.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for CollectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectError::Collision(c) => write!(
                f,
                "umbra collect_static: duplicate static namespace `{}` — claimed by both \
                 `{}` and `{}`; nothing was copied. Rename one plugin's namespace.",
                c.namespace, c.first_plugin, c.second_plugin
            ),
            CollectError::Io { path, source } => write!(
                f,
                "umbra collect_static: io error at `{}`: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CollectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CollectError::Io { source, .. } => Some(source),
            CollectError::Collision(_) => None,
        }
    }
}

/// Collect every registered plugin's `static_dirs()` into `static_root`.
///
/// Django's `collectstatic`. For each `StaticDir { namespace, source_dir }`,
/// the entire `source_dir` tree is recursively copied into
/// `<static_root>/<namespace>/`, preserving each file's path RELATIVE to
/// its `source_dir`: `source_dir/assets/index.js` lands at
/// `<static_root>/<namespace>/assets/index.js`.
///
/// Guarantees:
///
/// - **Collisions abort up front.** Namespace collisions are detected via
///   [`StaticRegistry::from_plugins`] *before* any file is written, so a
///   misconfigured app never leaves a half-populated `static_root`.
/// - **Idempotent.** Re-running overwrites existing files (changed source
///   bytes propagate). Destination dirs are created as needed.
/// - **Missing source is warned, not fatal.** A plugin whose `source_dir`
///   doesn't exist is recorded in [`CollectSummary::missing`] and skipped;
///   every other plugin still collects. The caller surfaces the warning.
/// - **`clear` empties `static_root` first.** When `true`, the destination
///   root's contents are removed before collecting (the root dir itself is
///   recreated). Use to drop stale assets that no plugin ships any more.
///
/// This is filesystem infrastructure (copying asset files), so `std::fs`
/// is the correct tool — the ORM-only rule governs database rows, not
/// files.
pub fn collect_static(
    plugins: &[Box<dyn Plugin>],
    static_root: impl Into<PathBuf>,
    clear: bool,
) -> Result<CollectSummary, CollectError> {
    let static_root = static_root.into();

    // Detect collisions BEFORE writing anything. `from_plugins` is the
    // single source of truth for the "namespace -> source_dir" map and
    // the collision rule; running it here keeps collect_static and the
    // runtime handler in lockstep. We discard the returned registry and
    // re-walk plugins below because the per-plugin loop needs each
    // plugin's `name()` for the summary, which the flattened registry
    // doesn't carry.
    StaticRegistry::from_plugins(plugins).map_err(CollectError::Collision)?;

    if clear && static_root.exists() {
        std::fs::remove_dir_all(&static_root).map_err(|source| CollectError::Io {
            path: static_root.clone(),
            source,
        })?;
    }

    std::fs::create_dir_all(&static_root).map_err(|source| CollectError::Io {
        path: static_root.clone(),
        source,
    })?;

    let mut summary = CollectSummary {
        static_root: static_root.clone(),
        ..Default::default()
    };

    for plugin in plugins {
        for dir in plugin.static_dirs() {
            let StaticDir {
                namespace,
                source_dir,
            } = dir;

            if !source_dir.exists() {
                // A declared-but-absent source dir is a real
                // misconfiguration. Record it so the CLI warns; don't
                // swallow it silently (fix-don't-patch), and don't abort
                // the whole run — the other plugins still collect.
                summary.missing.push(MissingSourceDir {
                    namespace,
                    plugin: plugin.name(),
                    source_dir,
                });
                continue;
            }

            let dest = static_root.join(namespace);
            let files = copy_tree(&source_dir, &dest)?;

            summary.collected.push(CollectedNamespace {
                namespace,
                plugin: plugin.name(),
                files,
                destination: dest,
            });
        }
    }

    Ok(summary)
}

/// Recursively copy every file under `src` into `dest`, preserving the
/// tree shape, and return the count of files (not directories) copied.
/// Existing files are overwritten (`std::fs::copy` replaces), making
/// re-runs idempotent.
fn copy_tree(src: &Path, dest: &Path) -> Result<usize, CollectError> {
    std::fs::create_dir_all(dest).map_err(|source| CollectError::Io {
        path: dest.to_path_buf(),
        source,
    })?;

    let mut count = 0;
    let entries = std::fs::read_dir(src).map_err(|source| CollectError::Io {
        path: src.to_path_buf(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| CollectError::Io {
            path: src.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| CollectError::Io {
            path: entry.path(),
            source,
        })?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());

        if file_type.is_dir() {
            count += copy_tree(&src_path, &dest_path)?;
        } else {
            // Covers regular files and symlinks-to-files alike:
            // `std::fs::copy` follows symlinks and copies the target
            // bytes, which is what a collected asset should be.
            std::fs::copy(&src_path, &dest_path).map_err(|source| CollectError::Io {
                path: src_path.clone(),
                source,
            })?;
            count += 1;
        }
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    /// Build a `Request` whose path is `path` (already base-stripped),
    /// matching what `nest_service` hands the handler.
    fn req(path: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("test request is valid")
    }

    /// A minimal plugin that contributes a fixed set of static dirs.
    struct FakeStaticPlugin {
        name: &'static str,
        dirs: Vec<StaticDir>,
    }

    impl Plugin for FakeStaticPlugin {
        fn name(&self) -> &'static str {
            self.name
        }
        fn static_dirs(&self) -> Vec<StaticDir> {
            self.dirs.clone()
        }
    }

    /// A plugin with no static dirs — proves the trait default is empty
    /// and that it contributes nothing to the registry.
    struct NoStaticPlugin;
    impl Plugin for NoStaticPlugin {
        fn name(&self) -> &'static str {
            "no-static"
        }
    }

    #[test]
    fn static_dirs_default_is_empty() {
        assert!(NoStaticPlugin.static_dirs().is_empty());
    }

    #[test]
    fn registry_collects_static_dirs_from_plugins() {
        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(FakeStaticPlugin {
                name: "admin",
                dirs: vec![StaticDir::new("admin", "/src/admin/static")],
            }),
            Box::new(NoStaticPlugin),
            Box::new(FakeStaticPlugin {
                name: "playground",
                dirs: vec![StaticDir::new("playground", "/src/playground/static")],
            }),
        ];
        let registry = StaticRegistry::from_plugins(&plugins).expect("no collision");
        assert_eq!(
            registry.source_dir("admin"),
            Some(Path::new("/src/admin/static"))
        );
        assert_eq!(
            registry.source_dir("playground"),
            Some(Path::new("/src/playground/static"))
        );
        assert_eq!(registry.source_dir("nonexistent"), None);
    }

    #[test]
    fn duplicate_namespace_fails_loudly_naming_both_plugins() {
        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(FakeStaticPlugin {
                name: "first",
                dirs: vec![StaticDir::new("shared", "/a")],
            }),
            Box::new(FakeStaticPlugin {
                name: "second",
                dirs: vec![StaticDir::new("shared", "/b")],
            }),
        ];
        let err = StaticRegistry::from_plugins(&plugins).expect_err("must collide");
        assert_eq!(err.namespace, "shared");
        assert_eq!(err.first_plugin, "first");
        assert_eq!(err.second_plugin, "second");
    }

    #[test]
    fn split_namespace_splits_first_segment() {
        assert_eq!(
            split_namespace("admin/admin.css"),
            Some(("admin", "admin.css"))
        );
        assert_eq!(
            split_namespace("/admin/css/site.css"),
            Some(("admin", "css/site.css"))
        );
        // Bare namespace, no file -> nothing to serve.
        assert_eq!(split_namespace("admin"), None);
        assert_eq!(split_namespace("admin/"), None);
        assert_eq!(split_namespace(""), None);
    }

    #[test]
    fn resolve_under_root_blocks_parent_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("ok.css"), b"body{}").expect("write file");

        // A legitimate file resolves.
        assert!(resolve_under_root(dir.path(), "ok.css").is_some());

        // `..` escapes are refused lexically, before any FS access.
        assert!(resolve_under_root(dir.path(), "../../etc/passwd").is_none());
        assert!(resolve_under_root(dir.path(), "../secret").is_none());
        assert!(resolve_under_root(dir.path(), "a/../../b").is_none());

        // Absolute paths are refused.
        assert!(resolve_under_root(dir.path(), "/etc/passwd").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_under_root_blocks_symlink_escape() {
        let root = tempfile::tempdir().expect("root tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::fs::write(outside.path().join("secret"), b"top secret").expect("write secret");

        // A symlink *inside* root pointing to a file *outside* root.
        let link = root.path().join("escape");
        std::os::unix::fs::symlink(outside.path().join("secret"), &link).expect("symlink");

        // Lexically clean ("escape" is a Normal component), but
        // canonicalisation + containment catches the escape.
        assert!(resolve_under_root(root.path(), "escape").is_none());
    }

    #[tokio::test]
    async fn dev_serves_live_source_then_falls_back_to_static_root() {
        let source = tempfile::tempdir().expect("source dir");
        let static_root = tempfile::tempdir().expect("static root");

        // Live source has admin.css; static_root has only legacy.css.
        std::fs::write(source.path().join("admin.css"), b"SOURCE").expect("write source");
        std::fs::create_dir_all(static_root.path().join("admin")).expect("mkdir ns");
        std::fs::write(
            static_root.path().join("admin").join("legacy.css"),
            b"COLLECTED",
        )
        .expect("write collected");

        let mut by_namespace = HashMap::new();
        by_namespace.insert("admin", source.path().to_path_buf());
        let registry = StaticRegistry { by_namespace };

        let state = StaticHandlerState {
            registry,
            static_root: static_root.path().to_path_buf(),
            dev: true,
        };

        // Live source wins for admin.css.
        let resp = static_handler(State(state.clone()), req("/admin/admin.css")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(&body[..], b"SOURCE");

        // legacy.css isn't in the live source -> dev falls back to
        // static_root/admin/legacy.css.
        let resp = static_handler(State(state.clone()), req("/admin/legacy.css")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(&body[..], b"COLLECTED");

        // A namespace with no registered source dir still serves from
        // static_root in dev.
        std::fs::create_dir_all(static_root.path().join("other")).expect("mkdir other");
        std::fs::write(static_root.path().join("other").join("x.js"), b"OTHER").expect("write");
        let resp = static_handler(State(state), req("/other/x.js")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(&body[..], b"OTHER");
    }

    #[tokio::test]
    async fn prod_serves_only_from_static_root() {
        let source = tempfile::tempdir().expect("source dir");
        let static_root = tempfile::tempdir().expect("static root");

        // Source has a file that is NOT collected into static_root.
        std::fs::write(source.path().join("only-source.css"), b"SOURCE").expect("write source");
        std::fs::create_dir_all(static_root.path().join("admin")).expect("mkdir ns");
        std::fs::write(
            static_root.path().join("admin").join("admin.css"),
            b"COLLECTED",
        )
        .expect("write collected");

        let mut by_namespace = HashMap::new();
        by_namespace.insert("admin", source.path().to_path_buf());
        let registry = StaticRegistry { by_namespace };

        let state = StaticHandlerState {
            registry,
            static_root: static_root.path().to_path_buf(),
            dev: false,
        };

        // Collected file is served.
        let resp = static_handler(State(state.clone()), req("/admin/admin.css")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(&body[..], b"COLLECTED");

        // The source-only file is NOT reachable in prod (live serving is
        // dev-only).
        let resp = static_handler(State(state), req("/admin/only-source.css")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Write `bytes` to `dir/relpath`, creating parent dirs as needed.
    fn write_at(dir: &Path, relpath: &str, bytes: &[u8]) {
        let full = dir.join(relpath);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parents");
        }
        std::fs::write(full, bytes).expect("write file");
    }

    /// Read `dir/relpath` as bytes, panicking if absent.
    fn read_at(dir: &Path, relpath: &str) -> Vec<u8> {
        std::fs::read(dir.join(relpath)).expect("read collected file")
    }

    #[test]
    fn collect_copies_every_file_preserving_the_tree() {
        let admin_src = tempfile::tempdir().expect("admin src");
        let pg_src = tempfile::tempdir().expect("playground src");
        let static_root = tempfile::tempdir().expect("static root");

        // Nested source trees: a top-level file and a file under assets/.
        write_at(admin_src.path(), "admin.css", b"ADMIN_CSS");
        write_at(admin_src.path(), "js/admin.js", b"ADMIN_JS");
        write_at(pg_src.path(), "dist/assets/index.js", b"PG_INDEX");

        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(FakeStaticPlugin {
                name: "admin-plugin",
                dirs: vec![StaticDir::new("admin", admin_src.path())],
            }),
            Box::new(FakeStaticPlugin {
                name: "pg-plugin",
                dirs: vec![StaticDir::new("playground", pg_src.path())],
            }),
        ];

        let summary =
            collect_static(&plugins, static_root.path(), false).expect("collect succeeds");

        // Every file landed at <static_root>/<ns>/<relpath> with bytes intact.
        assert_eq!(read_at(static_root.path(), "admin/admin.css"), b"ADMIN_CSS");
        assert_eq!(
            read_at(static_root.path(), "admin/js/admin.js"),
            b"ADMIN_JS"
        );
        assert_eq!(
            read_at(static_root.path(), "playground/dist/assets/index.js"),
            b"PG_INDEX"
        );

        // Summary reflects the per-namespace counts and the total.
        assert_eq!(summary.total_files(), 3);
        assert!(summary.missing.is_empty());
        let admin = summary
            .collected
            .iter()
            .find(|c| c.namespace == "admin")
            .expect("admin collected");
        assert_eq!(admin.files, 2);
        assert_eq!(admin.plugin, "admin-plugin");
        assert_eq!(admin.destination, static_root.path().join("admin"));
        let pg = summary
            .collected
            .iter()
            .find(|c| c.namespace == "playground")
            .expect("playground collected");
        assert_eq!(pg.files, 1);
    }

    #[test]
    fn collect_is_idempotent_and_propagates_changed_bytes() {
        let src = tempfile::tempdir().expect("src");
        let static_root = tempfile::tempdir().expect("static root");
        write_at(src.path(), "app.js", b"V1");

        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(FakeStaticPlugin {
            name: "p",
            dirs: vec![StaticDir::new("app", src.path())],
        })];

        // First run.
        collect_static(&plugins, static_root.path(), false).expect("first collect");
        assert_eq!(read_at(static_root.path(), "app/app.js"), b"V1");

        // Change the source bytes and re-run; the new bytes propagate.
        write_at(src.path(), "app.js", b"V2_CHANGED");
        let summary = collect_static(&plugins, static_root.path(), false).expect("second collect");
        assert_eq!(read_at(static_root.path(), "app/app.js"), b"V2_CHANGED");
        assert_eq!(summary.total_files(), 1);
    }

    #[test]
    fn duplicate_namespace_aborts_and_copies_nothing() {
        let a = tempfile::tempdir().expect("a");
        let b = tempfile::tempdir().expect("b");
        let static_root = tempfile::tempdir().expect("static root");
        write_at(a.path(), "a.css", b"A");
        write_at(b.path(), "b.css", b"B");

        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(FakeStaticPlugin {
                name: "first",
                dirs: vec![StaticDir::new("shared", a.path())],
            }),
            Box::new(FakeStaticPlugin {
                name: "second",
                dirs: vec![StaticDir::new("shared", b.path())],
            }),
        ];

        let err =
            collect_static(&plugins, static_root.path(), false).expect_err("collision aborts");
        match err {
            CollectError::Collision(c) => {
                assert_eq!(c.namespace, "shared");
                assert_eq!(c.first_plugin, "first");
                assert_eq!(c.second_plugin, "second");
            }
            other => panic!("expected Collision, got {other:?}"),
        }

        // Nothing was copied — the static_root has no namespace dirs.
        assert!(!static_root.path().join("shared").exists());
        let entries: Vec<_> = std::fs::read_dir(static_root.path())
            .expect("read static_root")
            .collect();
        assert!(
            entries.is_empty(),
            "static_root must be untouched on collision"
        );
    }

    #[test]
    fn missing_source_dir_warns_but_others_still_collect() {
        let present = tempfile::tempdir().expect("present");
        let static_root = tempfile::tempdir().expect("static root");
        write_at(present.path(), "ok.css", b"OK");

        // `missing_src` points at a path we never create.
        let missing_src = present.path().join("does-not-exist");
        assert!(!missing_src.exists());

        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(FakeStaticPlugin {
                name: "broken",
                dirs: vec![StaticDir::new("ghost", missing_src.clone())],
            }),
            Box::new(FakeStaticPlugin {
                name: "good",
                dirs: vec![StaticDir::new("real", present.path())],
            }),
        ];

        let summary =
            collect_static(&plugins, static_root.path(), false).expect("missing src is not fatal");

        // The good plugin collected.
        assert_eq!(read_at(static_root.path(), "real/ok.css"), b"OK");
        // The broken plugin is recorded as missing, not collected.
        assert_eq!(summary.missing.len(), 1);
        assert_eq!(summary.missing[0].namespace, "ghost");
        assert_eq!(summary.missing[0].plugin, "broken");
        assert_eq!(summary.missing[0].source_dir, missing_src);
        // No ghost dir was created.
        assert!(!static_root.path().join("ghost").exists());
    }

    #[test]
    fn clear_removes_stale_files_before_collect() {
        let src = tempfile::tempdir().expect("src");
        let static_root = tempfile::tempdir().expect("static root");
        write_at(src.path(), "current.css", b"CURRENT");

        // Pre-seed static_root with a stale namespace no plugin ships.
        write_at(static_root.path(), "stale/old.css", b"STALE");

        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(FakeStaticPlugin {
            name: "p",
            dirs: vec![StaticDir::new("app", src.path())],
        })];

        // Without --clear, stale survives alongside the fresh collect.
        collect_static(&plugins, static_root.path(), false).expect("no-clear collect");
        assert!(static_root.path().join("stale/old.css").exists());
        assert_eq!(read_at(static_root.path(), "app/current.css"), b"CURRENT");

        // With --clear, the stale file is gone and only fresh assets remain.
        collect_static(&plugins, static_root.path(), true).expect("clear collect");
        assert!(!static_root.path().join("stale").exists());
        assert_eq!(read_at(static_root.path(), "app/current.css"), b"CURRENT");
    }

    #[test]
    fn collect_creates_static_root_when_absent() {
        let src = tempfile::tempdir().expect("src");
        let parent = tempfile::tempdir().expect("parent");
        // static_root doesn't exist yet — collect must create it.
        let static_root = parent.path().join("staticfiles");
        assert!(!static_root.exists());
        write_at(src.path(), "x.css", b"X");

        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(FakeStaticPlugin {
            name: "p",
            dirs: vec![StaticDir::new("ns", src.path())],
        })];

        collect_static(&plugins, &static_root, false).expect("collect creates root");
        assert_eq!(read_at(&static_root, "ns/x.css"), b"X");
    }

    #[tokio::test]
    async fn handler_blocks_path_traversal() {
        let static_root = tempfile::tempdir().expect("static root");
        std::fs::create_dir_all(static_root.path().join("admin")).expect("mkdir ns");
        std::fs::write(static_root.path().join("admin").join("ok.css"), b"OK").expect("write");

        let state = StaticHandlerState {
            registry: StaticRegistry::default(),
            static_root: static_root.path().to_path_buf(),
            dev: false,
        };

        let resp = static_handler(State(state), req("/admin/../../etc/passwd")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
