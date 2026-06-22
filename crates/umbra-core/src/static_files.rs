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
//! logic; the standalone `umbra-storage` `StoragePlugin` static side keeps its own
//! `ServeDir`/`include_dir` paths (a directory tree and an embedded
//! tree are different shapes from a single-file serve) and is not
//! rewired onto this primitive in this slice. The dev `max-age=0` /
//! prod cache behaviour is applied here too.
//!
//! [`Plugin::static_dirs`]: crate::plugin::Plugin::static_dirs
//! [`Environment::Dev`]: crate::settings::Environment

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, Response, StatusCode, header};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::plugin::{Plugin, StaticDir};

/// The on-disk name of the hashed-asset manifest written into
/// `static_root` by `collectstatic --hashed`. Django calls the same file
/// `staticfiles.json`; we keep the name so the concept ports directly.
pub const MANIFEST_FILENAME: &str = "staticfiles.json";

/// Anything that can go wrong writing an asset through a
/// [`StaticStorage`] backend. Backend-agnostic: a filesystem `put` and an
/// S3 `put_object` both funnel their failure through this enum so
/// `collect_into` has one error type regardless of where assets land.
#[derive(Debug)]
pub enum StaticError {
    /// An IO error writing/reading an asset. Carries the logical path
    /// that failed so the message names the culprit.
    Io {
        path: String,
        source: std::io::Error,
    },
    /// A backend-specific failure (a remote upload rejected, credentials
    /// missing, region unreachable). Carries a human-readable message.
    Backend(String),
}

impl std::fmt::Display for StaticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StaticError::Io { path, source } => {
                write!(f, "static storage io error at `{path}`: {source}")
            }
            StaticError::Backend(msg) => write!(f, "static storage backend error: {msg}"),
        }
    }
}

impl std::error::Error for StaticError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StaticError::Io { source, .. } => Some(source),
            StaticError::Backend(_) => None,
        }
    }
}

/// A swappable destination for collected static assets — Django's
/// `STATICFILES_STORAGE`. `collectstatic` writes every file *through* a
/// `StaticStorage` rather than calling `std::fs` directly, so the same
/// collect path targets the local filesystem ([`LocalStorage`], the
/// default) or a remote object store (the feature-gated S3 backend in
/// `umbra-storage`) without the collect engine knowing which.
///
/// `rel_path` is always the logical path RELATIVE to `static_root`
/// (`"admin/admin.css"`, `"css/app.css"`), forward-slash separated. The
/// backend maps it onto its own addressing (a filesystem join, an S3
/// object key) — the engine never constructs an absolute on-disk path.
pub trait StaticStorage: Send + Sync {
    /// Write `bytes` at the logical `rel_path`, creating any intermediate
    /// structure (directories, key prefixes) the backend needs.
    /// Overwrites an existing object so re-running `collectstatic` is
    /// idempotent.
    fn put(&self, rel_path: &str, bytes: &[u8]) -> Result<(), StaticError>;

    /// Whether an object already exists at `rel_path`.
    fn exists(&self, rel_path: &str) -> Result<bool, StaticError>;
}

/// The default [`StaticStorage`]: writes collected assets onto the local
/// filesystem under `root` (the resolved `static_root`). Reproduces the
/// pre-storage-trait filesystem copy exactly — `put("a/b.css", bytes)`
/// writes `<root>/a/b.css`, creating parent dirs as needed.
#[derive(Debug, Clone)]
pub struct LocalStorage {
    /// The on-disk root every `rel_path` is joined onto.
    pub root: PathBuf,
}

impl LocalStorage {
    /// A filesystem storage rooted at `root` (the resolved `static_root`).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve a logical `rel_path` onto its on-disk path under `root`.
    fn full_path(&self, rel_path: &str) -> PathBuf {
        let mut p = self.root.clone();
        for seg in rel_path.split('/') {
            if !seg.is_empty() {
                p.push(seg);
            }
        }
        p
    }
}

impl StaticStorage for LocalStorage {
    fn put(&self, rel_path: &str, bytes: &[u8]) -> Result<(), StaticError> {
        let dest = self.full_path(rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StaticError::Io {
                path: rel_path.to_string(),
                source,
            })?;
        }
        std::fs::write(&dest, bytes).map_err(|source| StaticError::Io {
            path: rel_path.to_string(),
            source,
        })
    }

    fn exists(&self, rel_path: &str) -> Result<bool, StaticError> {
        Ok(self.full_path(rel_path).exists())
    }
}

/// Compute the content-hash filename fragment Django's
/// `ManifestStaticFilesStorage` uses: the first 12 hex chars of the
/// SHA-256 of the file bytes. 48 bits is ample for cache-busting (a
/// collision needs ~16M distinct versions of one asset) while keeping the
/// hashed filename short.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// Insert the content hash before the final extension of a logical path:
/// `"css/app.css"` → `"css/app.<hash>.css"`, `"js/x"` (no extension) →
/// `"js/x.<hash>"`, `"a/b.min.css"` → `"a/b.min.<hash>.css"` (only the
/// LAST `.` segment is treated as the extension, matching Django).
pub fn hashed_name(rel_path: &str, hash: &str) -> String {
    // Split off the final path segment so a `.` in a directory name (rare
    // but possible) never gets mistaken for the file extension.
    let (dir, file) = match rel_path.rfind('/') {
        Some(i) => (&rel_path[..=i], &rel_path[i + 1..]),
        None => ("", rel_path),
    };
    match file.rfind('.') {
        Some(dot) => format!("{dir}{}.{hash}.{}", &file[..dot], &file[dot + 1..]),
        None => format!("{dir}{file}.{hash}"),
    }
}

/// One plugin's namespaced static contribution, flattened so it can be
/// published ambiently for a CLI command that has no access to the
/// plugin list. Carries the plugin name too, so `collectstatic`'s
/// summary and missing-source warnings can name the culprit exactly as
/// the plugin-list path did.
#[derive(Debug, Clone)]
pub struct StaticContribution {
    /// The static namespace this source dir collects under
    /// (`<static_root>/<namespace>/`).
    pub namespace: &'static str,
    /// On-disk source dir whose tree is copied at collect time.
    pub source_dir: PathBuf,
    /// The plugin that declared this contribution (for summaries /
    /// warnings).
    pub plugin: &'static str,
}

impl StaticContribution {
    /// Flatten every plugin's [`Plugin::static_dirs`] into a list of
    /// contributions, capturing each plugin's `name()` so the collect
    /// summary can attribute files and missing-source warnings.
    ///
    /// No collision check here — the caller that publishes this list
    /// (`App::build`) has already run [`StaticRegistry::from_plugins`],
    /// which fails the build on a duplicate namespace before anything is
    /// published. The published list is therefore pre-validated.
    pub fn collect(plugins: &[Box<dyn Plugin>]) -> Vec<StaticContribution> {
        let mut out = Vec::new();
        for plugin in plugins {
            for dir in plugin.static_dirs() {
                let StaticDir {
                    namespace,
                    source_dir,
                } = dir;
                out.push(StaticContribution {
                    namespace,
                    source_dir,
                    plugin: plugin.name(),
                });
            }
        }
        out
    }

    /// Collect every plugin's [`Plugin::static_root_dirs`] into a flat
    /// list of app/site root dirs, copied into `<static_root>/` root at
    /// collect time (Django `STATICFILES_DIRS` parity).
    pub fn collect_root_dirs(plugins: &[Box<dyn Plugin>]) -> Vec<PathBuf> {
        plugins.iter().flat_map(|p| p.static_root_dirs()).collect()
    }
}

/// The static contributions published ambiently at `App::build` for CLI
/// commands that can't take the plugin list as an argument.
///
/// This mirrors the `settings` ambient `OnceLock` (see
/// [`crate::settings`]): read-only app config published exactly once at
/// build time. It is NOT a mutable creeping global — nothing mutates it
/// after `publish_static`, and the only reader is `collectstatic`, which
/// runs after `App::build` and so needs every plugin's `static_dirs()`
/// (namespaced) and `static_root_dirs()` (app/site) without the plugin
/// list being threaded through `PluginCommand::run`.
#[derive(Debug, Clone, Default)]
pub struct PublishedStatic {
    /// Every plugin's namespaced static contributions.
    pub contributions: Vec<StaticContribution>,
    /// Every plugin's app/site root dirs (no namespace), copied into the
    /// `<static_root>/` root.
    pub root_dirs: Vec<PathBuf>,
}

/// The one published-static slot, set once at `App::build`. Same family
/// as `settings::SETTINGS` — the single intentional read-only ambient
/// for CLI commands that run outside a request and can't be handed the
/// plugin list directly.
static PUBLISHED: OnceLock<PublishedStatic> = OnceLock::new();

/// Publish the static contributions ambiently. Idempotent: a second
/// call (e.g. a second `App::build` in one test process) is a no-op —
/// the first publish wins, matching the `settings` OnceLock semantics.
pub fn publish_static(p: PublishedStatic) {
    let _ = PUBLISHED.set(p);
}

/// The static contributions published at `App::build`, or `None` if no
/// `App` has been built in this process yet. `collectstatic` reads this
/// to learn every plugin's source dirs without the plugin list.
pub fn published_static() -> Option<&'static PublishedStatic> {
    PUBLISHED.get()
}

/// The loaded hashed-asset manifest: logical path → hashed path
/// (`"css/app.css" -> "css/app.<hash>.css"`). Loaded once from
/// `<static_root>/staticfiles.json` and cached ambiently, the same
/// read-only-at-boot family as `settings::SETTINGS` and [`PUBLISHED`].
///
/// `None` (the `OnceLock` unset, or set to `None`) means no manifest was
/// found — `resolve_static_url` then falls back to today's plain
/// `static_url + path` join. A present manifest means `collectstatic
/// --hashed` ran, so prod serves the content-hashed filenames and can set
/// far-future cache headers on them.
static MANIFEST: OnceLock<Option<HashMap<String, String>>> = OnceLock::new();

/// Load the hashed-asset manifest from `<static_root>/staticfiles.json`
/// into the ambient slot, once. Idempotent: the first load wins (matching
/// `settings`/`published_static`); a second call is a no-op.
///
/// Call at `App::build` after settings resolve. A missing or unparseable
/// manifest is recorded as `None` (no hashing in effect) rather than an
/// error — an app that never ran `collectstatic --hashed` legitimately
/// has no manifest, and `resolve_static_url` must keep working.
pub fn load_manifest(static_root: impl AsRef<Path>) {
    let path = static_root.as_ref().join(MANIFEST_FILENAME);
    let loaded = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HashMap<String, String>>(&bytes).ok());
    let _ = MANIFEST.set(loaded);
}

/// Look up the hashed name for a logical asset path in the loaded
/// manifest. Returns `None` when no manifest is loaded OR the path isn't
/// in it (an asset not collected through `--hashed`); the caller then
/// uses the path unchanged.
///
/// The lookup key is the logical path as the template wrote it
/// (`"css/app.css"`), normalised to drop a leading slash so
/// `static("/css/app.css")` and `static("css/app.css")` hit the same
/// entry — matching `resolve_static_url`'s join, which also trims the
/// leading slash.
pub fn manifest_lookup(path: &str) -> Option<&'static str> {
    let manifest = MANIFEST.get()?.as_ref()?;
    let key = path.trim_start_matches('/');
    manifest.get(key).map(String::as_str)
}

/// Whether a hashed-asset manifest is currently loaded. `resolve_static_url`
/// uses this to decide between hashed and plain URLs.
pub fn manifest_loaded() -> bool {
    matches!(MANIFEST.get(), Some(Some(_)))
}

/// Test-only: install a manifest directly, bypassing the on-disk load.
/// Used by `resolve_static_url` tests that need a known manifest without
/// staging a `staticfiles.json` on disk.
#[doc(hidden)]
pub fn set_manifest_for_tests(manifest: Option<HashMap<String, String>>) {
    let _ = MANIFEST.set(manifest);
}

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
/// file-serving path — the unified static handler and `umbra-storage`
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
    // already relative to the static base: `/admin/admin.css`,
    // `/css/site.css`.
    let path = req.uri().path().to_string();
    let rel = path.trim_start_matches('/');

    // Step 1 — dev live source for a *registered* namespace. Lets a
    // rebuilt plugin asset be served straight off its source dir without
    // a recompile or a collect step. Only a namespace a plugin actually
    // declared takes this path; an unregistered first segment (e.g.
    // `css`) is not a namespace and flows to the steps below.
    if state.dev {
        if let Some((namespace, rest)) = split_namespace(rel) {
            if let Some(source_dir) = state.registry.source_dir(namespace) {
                if let Some(resolved) = resolve_under_root(source_dir, rest) {
                    return serve_file(&resolved, true, req).await;
                }
            }
        }
    }

    // Step 2 — the collected/prod tree: `<static_root>/<full path>`. This
    // is the general path that serves every collected namespace
    // (`<static_root>/admin/admin.css`) in prod, and the dev fallback when
    // a live source missed. A missing `static_root` (no collect run yet)
    // canonicalises to `None` here and flows on to the root dirs.
    if let Some(resolved) = resolve_under_root(&state.static_root, rel) {
        return serve_file(&resolved, state.dev, req).await;
    }

    // Step 3 — app/site root dirs (no namespace), the full request path.
    // Real on-disk directories (a project's `./static`), served the same
    // in dev and prod. This is what a `StoragePlugin` static side at `static_url`
    // contributes, so site CSS / images live at the bare `/static/...`.
    for root in &state.root_dirs {
        if let Some(resolved) = resolve_under_root(root, rel) {
            return serve_file(&resolved, state.dev, req).await;
        }
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
    /// App/site-level static directories served at the bare
    /// `static_url` root (no namespace), from every plugin's
    /// [`Plugin::static_root_dirs`]. Tried after namespaces, with the
    /// full request path. Typically a `StoragePlugin` static side pointed at
    /// `static_url` contributes its directory here so the framework owns
    /// `static_url` as one mount instead of a second catch-all colliding
    /// with the pipeline.
    ///
    /// [`Plugin::static_root_dirs`]: crate::plugin::Plugin::static_root_dirs
    pub root_dirs: Vec<PathBuf>,
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
    /// Count of files copied from app/site root dirs
    /// ([`Plugin::static_root_dirs`]) into the `<static_root>/` ROOT
    /// (no namespace) — Django `STATICFILES_DIRS` parity. Counted
    /// separately from namespaced files so the CLI can report both.
    ///
    /// [`Plugin::static_root_dirs`]: crate::plugin::Plugin::static_root_dirs
    pub root_files: usize,
    /// The app/site root dirs that were collected (those that existed on
    /// disk). A declared-but-absent root dir is skipped silently — unlike
    /// a namespaced source, a root dir is a project convention dir
    /// (`./static`) that legitimately may not exist yet.
    pub root_dirs: Vec<PathBuf>,
}

impl CollectSummary {
    /// Total files copied across every namespace (not counting root-dir
    /// files; use [`Self::root_files`] for those).
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
    /// A [`StaticStorage`] backend rejected a write (a failed S3 upload,
    /// a permission error from the local filesystem put). Carries the
    /// backend error so the CLI can surface which destination failed.
    Static(StaticError),
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
            CollectError::Static(e) => write!(f, "umbra collect_static: {e}"),
        }
    }
}

impl std::error::Error for CollectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CollectError::Io { source, .. } => Some(source),
            CollectError::Static(e) => Some(e),
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
    // Detect collisions BEFORE writing anything. `from_plugins` is the
    // single source of truth for the "namespace -> source_dir" map and
    // the collision rule; running it here keeps collect_static and the
    // runtime handler in lockstep. The flattened contributions below
    // carry each plugin's `name()`, which the summary needs and the
    // registry doesn't keep.
    StaticRegistry::from_plugins(plugins).map_err(CollectError::Collision)?;

    let contributions = StaticContribution::collect(plugins);
    let root_dirs = StaticContribution::collect_root_dirs(plugins);
    collect_into(&contributions, &root_dirs, static_root, clear)
}

/// The single core copy routine, shared by the plugin-list path
/// ([`collect_static`]) and the published-contributions path (the
/// `collectstatic` plugin command).
///
/// Copies each `StaticContribution`'s `source_dir` tree into
/// `<static_root>/<namespace>/`, and each app/site `root_dir` into the
/// `<static_root>/` ROOT (no namespace) — Django `STATICFILES_DIRS`
/// parity, so `static_root` is a complete CDN-servable tree.
///
/// No collision check: the contributions are pre-validated (either by
/// [`collect_static`]'s `from_plugins` call, or at `App::build` before
/// they were published). The same guarantees as [`collect_static`]
/// apply: collisions never reach here, missing namespaced sources are
/// warned-not-fatal, re-runs are idempotent, and `clear` empties
/// `static_root` first.
///
/// This is filesystem infrastructure (copying asset files), so
/// `std::fs` is the correct tool — the ORM-only rule governs database
/// rows, not files.
pub fn collect_into(
    contributions: &[StaticContribution],
    root_dirs: &[PathBuf],
    static_root: impl Into<PathBuf>,
    clear: bool,
) -> Result<CollectSummary, CollectError> {
    let static_root = static_root.into();
    let storage = LocalStorage::new(static_root.clone());

    // Local-filesystem convention: ensure the root dir exists even when
    // nothing is collected (an app may point a reverse proxy at it
    // regardless). The storage-backed path creates parents per-file, but
    // an empty collect would otherwise leave no root dir at all. `clear`
    // is handled inside `collect_into_with`.
    if !(clear && static_root.exists()) {
        std::fs::create_dir_all(&static_root).map_err(|source| CollectError::Io {
            path: static_root.clone(),
            source,
        })?;
    }

    collect_into_with(contributions, root_dirs, &static_root, &storage, clear, false)
}

/// The storage-backed core collect routine. Writes every collected file
/// *through* `storage` (the [`StaticStorage`] seam) instead of `std::fs`
/// directly, so the same engine targets the local filesystem or a remote
/// object store. [`collect_into`] is the convenience wrapper that
/// constructs a [`LocalStorage`] and never hashes.
///
/// `static_root` is still passed alongside `storage` because it is the
/// logical destination recorded in the [`CollectSummary`] (and where the
/// manifest is written for [`LocalStorage`]); the bytes themselves go
/// through `storage.put(rel_path, ..)`.
///
/// When `hashed` is true (Django's `ManifestStaticFilesStorage`), each
/// file is *also* written under a content-hashed name
/// (`app.<hash>.css`), and a `<logical path> -> <hashed path>` mapping is
/// recorded into a `staticfiles.json` manifest written at the
/// `static_root` root. The original (un-hashed) copy is kept too, so an
/// old deploy referencing the plain name still resolves.
///
/// No collision check: the contributions are pre-validated. The same
/// guarantees as [`collect_static`] apply.
///
/// This is filesystem/asset infrastructure (copying asset files), so
/// `std::fs` for the *source* read is the correct tool — the ORM-only
/// rule governs database rows, not files. The *destination* write is the
/// one routed through `storage`.
pub fn collect_into_with(
    contributions: &[StaticContribution],
    root_dirs: &[PathBuf],
    static_root: impl Into<PathBuf>,
    storage: &dyn StaticStorage,
    clear: bool,
    hashed: bool,
) -> Result<CollectSummary, CollectError> {
    let static_root = static_root.into();

    // `clear` only makes sense for the local filesystem (a remote bucket
    // is cleared by its own lifecycle policy). When the local root
    // exists, empty it before collecting. For non-local backends the dir
    // simply doesn't exist and this is a no-op.
    if clear && static_root.exists() {
        std::fs::remove_dir_all(&static_root).map_err(|source| CollectError::Io {
            path: static_root.clone(),
            source,
        })?;
    }

    let mut summary = CollectSummary {
        static_root: static_root.clone(),
        ..Default::default()
    };

    // Logical-path → hashed-path manifest, accumulated across every file
    // when `hashed` is set. BTreeMap so the written JSON is deterministic
    // (sorted keys) — easier to diff between collect runs.
    let mut manifest: BTreeMap<String, String> = BTreeMap::new();

    // Namespaced contributions → <static_root>/<namespace>/.
    for contribution in contributions {
        let StaticContribution {
            namespace,
            source_dir,
            plugin,
        } = contribution;

        if !source_dir.exists() {
            // A declared-but-absent source dir is a real
            // misconfiguration. Record it so the CLI warns; don't
            // swallow it silently (fix-don't-patch), and don't abort
            // the whole run — the other contributions still collect.
            summary.missing.push(MissingSourceDir {
                namespace,
                plugin,
                source_dir: source_dir.clone(),
            });
            continue;
        }

        let files = copy_tree(
            source_dir,
            namespace,
            storage,
            hashed,
            &mut manifest,
        )?;

        summary.collected.push(CollectedNamespace {
            namespace,
            plugin,
            files,
            destination: static_root.join(namespace),
        });
    }

    // App/site root dirs → <static_root>/ root (no namespace). A root dir
    // is a project convention dir (`./static`) that may legitimately not
    // exist yet, so an absent one is skipped silently rather than warned
    // — it is not the "plugin promised assets that aren't there"
    // misconfiguration a namespaced source is.
    for root in root_dirs {
        if !root.exists() {
            continue;
        }
        let files = copy_tree(root, "", storage, hashed, &mut manifest)?;
        summary.root_files += files;
        summary.root_dirs.push(root.clone());
    }

    // Write the manifest once, after every file is hashed. Keyed by the
    // logical path the template uses (`css/app.css`), valued by the
    // hashed path (`css/app.<hash>.css`) — exactly what
    // `manifest_lookup` reads back.
    if hashed {
        let json = serde_json::to_vec_pretty(&manifest).map_err(|e| CollectError::Io {
            path: static_root.join(MANIFEST_FILENAME),
            source: std::io::Error::other(e),
        })?;
        storage
            .put(MANIFEST_FILENAME, &json)
            .map_err(CollectError::Static)?;
    }

    Ok(summary)
}

/// Recursively walk every file under `src`, writing each through
/// `storage` at the logical path `<prefix>/<relative path>` (prefix is
/// the namespace, or `""` for root dirs). Returns the count of files
/// (not directories) written.
///
/// When `hashed` is set, each file is additionally written under its
/// content-hashed name and the `<logical> -> <hashed>` pair recorded in
/// `manifest`. Re-runs overwrite (storage `put` replaces), keeping the
/// collect idempotent.
fn copy_tree(
    src: &Path,
    prefix: &str,
    storage: &dyn StaticStorage,
    hashed: bool,
    manifest: &mut BTreeMap<String, String>,
) -> Result<usize, CollectError> {
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
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_prefix = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };

        if file_type.is_dir() {
            count += copy_tree(&src_path, &child_prefix, storage, hashed, manifest)?;
        } else {
            // Covers regular files and symlinks-to-files alike:
            // `std::fs::read` follows symlinks and reads the target
            // bytes, which is what a collected asset should be.
            let bytes = std::fs::read(&src_path).map_err(|source| CollectError::Io {
                path: src_path.clone(),
                source,
            })?;
            storage
                .put(&child_prefix, &bytes)
                .map_err(CollectError::Static)?;
            count += 1;

            if hashed {
                let hash = content_hash(&bytes);
                let hashed_path = hashed_name(&child_prefix, &hash);
                // Write the hashed copy alongside the original. The
                // manifest never points at itself, and the original is
                // kept so an old deploy referencing the plain name still
                // resolves.
                storage
                    .put(&hashed_path, &bytes)
                    .map_err(CollectError::Static)?;
                manifest.insert(child_prefix.clone(), hashed_path);
            }
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
            root_dirs: Vec::new(),
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
            root_dirs: Vec::new(),
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

    #[test]
    fn collect_into_copies_root_dirs_into_static_root_root() {
        let ns_src = tempfile::tempdir().expect("ns src");
        let root_a = tempfile::tempdir().expect("root a");
        let root_b = tempfile::tempdir().expect("root b");
        let static_root = tempfile::tempdir().expect("static root");

        // A namespaced contribution lands under <root>/<ns>/...
        write_at(ns_src.path(), "admin.css", b"ADMIN");
        // Root dirs land at the bare <static_root>/... root, preserving
        // their tree shape.
        write_at(root_a.path(), "site.css", b"SITE_CSS");
        write_at(root_a.path(), "img/logo.png", b"LOGO");
        write_at(root_b.path(), "app.js", b"APP_JS");

        let contributions = vec![StaticContribution {
            namespace: "admin",
            source_dir: ns_src.path().to_path_buf(),
            plugin: "admin-plugin",
        }];
        let root_dirs = vec![root_a.path().to_path_buf(), root_b.path().to_path_buf()];

        let summary = collect_into(&contributions, &root_dirs, static_root.path(), false)
            .expect("collect_into succeeds");

        // Namespaced file under its namespace.
        assert_eq!(read_at(static_root.path(), "admin/admin.css"), b"ADMIN");
        // Root-dir files at the bare root, bytes intact, tree preserved.
        assert_eq!(read_at(static_root.path(), "site.css"), b"SITE_CSS");
        assert_eq!(read_at(static_root.path(), "img/logo.png"), b"LOGO");
        assert_eq!(read_at(static_root.path(), "app.js"), b"APP_JS");

        // Summary tracks both counts separately.
        assert_eq!(summary.total_files(), 1, "1 namespaced file");
        assert_eq!(summary.root_files, 3, "3 root-dir files across two dirs");
        assert_eq!(summary.root_dirs.len(), 2);
    }

    #[test]
    fn collect_into_skips_absent_root_dir_silently() {
        let static_root = tempfile::tempdir().expect("static root");
        let present = tempfile::tempdir().expect("present root");
        write_at(present.path(), "x.css", b"X");

        let absent = present.path().join("does-not-exist");
        assert!(!absent.exists());

        let root_dirs = vec![present.path().to_path_buf(), absent];
        let summary = collect_into(&[], &root_dirs, static_root.path(), false)
            .expect("collect_into succeeds");

        // Present root collected; absent one skipped, NOT recorded as a
        // missing-source warning (those are namespace-only).
        assert_eq!(read_at(static_root.path(), "x.css"), b"X");
        assert_eq!(summary.root_files, 1);
        assert_eq!(summary.root_dirs.len(), 1);
        assert!(summary.missing.is_empty());
    }

    #[test]
    fn hashed_name_inserts_hash_before_extension() {
        assert_eq!(
            hashed_name("css/app.css", "abc123"),
            "css/app.abc123.css"
        );
        // No extension: hash appended.
        assert_eq!(hashed_name("js/bundle", "deadbe"), "js/bundle.deadbe");
        // Only the LAST dot is the extension (Django parity).
        assert_eq!(
            hashed_name("a/b.min.css", "0f0f0f"),
            "a/b.min.0f0f0f.css"
        );
        // Top-level file, no directory.
        assert_eq!(hashed_name("favicon.ico", "112233"), "favicon.112233.ico");
    }

    #[test]
    fn content_hash_is_stable_and_12_hex() {
        let h = content_hash(b"body{}");
        assert_eq!(h.len(), 12);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        // Same bytes -> same hash; different bytes -> different hash.
        assert_eq!(content_hash(b"body{}"), h);
        assert_ne!(content_hash(b"body{ }"), h);
    }

    #[test]
    fn local_storage_put_writes_through_root() {
        let root = tempfile::tempdir().expect("root");
        let storage = LocalStorage::new(root.path());
        storage
            .put("css/app.css", b"BODY")
            .expect("put writes the file");
        assert_eq!(read_at(root.path(), "css/app.css"), b"BODY");
        assert!(storage.exists("css/app.css").expect("exists"));
        assert!(!storage.exists("css/missing.css").expect("exists"));
    }

    #[test]
    fn collect_hashed_writes_copies_and_manifest() {
        let admin_src = tempfile::tempdir().expect("admin src");
        let root_src = tempfile::tempdir().expect("root src");
        let static_root = tempfile::tempdir().expect("static root");

        write_at(admin_src.path(), "admin.css", b"ADMIN_CSS");
        write_at(root_src.path(), "css/app.css", b"APP_CSS");

        let contributions = vec![StaticContribution {
            namespace: "admin",
            source_dir: admin_src.path().to_path_buf(),
            plugin: "admin-plugin",
        }];
        let root_dirs = vec![root_src.path().to_path_buf()];

        let storage = LocalStorage::new(static_root.path());
        let summary = collect_into_with(
            &contributions,
            &root_dirs,
            static_root.path(),
            &storage,
            false,
            true,
        )
        .expect("hashed collect succeeds");

        // Originals are kept.
        assert_eq!(read_at(static_root.path(), "admin/admin.css"), b"ADMIN_CSS");
        assert_eq!(read_at(static_root.path(), "css/app.css"), b"APP_CSS");
        assert_eq!(summary.total_files(), 1);
        assert_eq!(summary.root_files, 1);

        // Manifest exists and maps logical -> hashed for BOTH the
        // namespaced and root-dir files, keyed by the template's logical
        // path.
        let manifest_bytes = read_at(static_root.path(), MANIFEST_FILENAME);
        let manifest: HashMap<String, String> =
            serde_json::from_slice(&manifest_bytes).expect("manifest parses");

        let admin_hashed = manifest
            .get("admin/admin.css")
            .expect("admin entry present");
        let app_hashed = manifest.get("css/app.css").expect("app entry present");

        // The hashed names carry the content hash and keep the extension.
        let admin_hash = content_hash(b"ADMIN_CSS");
        assert_eq!(admin_hashed, &format!("admin/admin.{admin_hash}.css"));
        let app_hash = content_hash(b"APP_CSS");
        assert_eq!(app_hashed, &format!("css/app.{app_hash}.css"));

        // The hashed COPIES were actually written alongside the originals.
        assert_eq!(read_at(static_root.path(), admin_hashed), b"ADMIN_CSS");
        assert_eq!(read_at(static_root.path(), app_hashed), b"APP_CSS");
    }

    #[test]
    fn collect_without_hashed_writes_no_manifest() {
        let src = tempfile::tempdir().expect("src");
        let static_root = tempfile::tempdir().expect("static root");
        write_at(src.path(), "x.css", b"X");

        let contributions = vec![StaticContribution {
            namespace: "ns",
            source_dir: src.path().to_path_buf(),
            plugin: "p",
        }];
        let storage = LocalStorage::new(static_root.path());
        collect_into_with(&contributions, &[], static_root.path(), &storage, false, false)
            .expect("plain collect");

        assert_eq!(read_at(static_root.path(), "ns/x.css"), b"X");
        // No manifest, no hashed copy.
        assert!(!static_root.path().join(MANIFEST_FILENAME).exists());
    }

    #[tokio::test]
    async fn handler_blocks_path_traversal() {
        let static_root = tempfile::tempdir().expect("static root");
        std::fs::create_dir_all(static_root.path().join("admin")).expect("mkdir ns");
        std::fs::write(static_root.path().join("admin").join("ok.css"), b"OK").expect("write");

        let state = StaticHandlerState {
            registry: StaticRegistry::default(),
            static_root: static_root.path().to_path_buf(),
            root_dirs: Vec::new(),
            dev: false,
        };

        let resp = static_handler(State(state), req("/admin/../../etc/passwd")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
