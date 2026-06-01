//! Server-side HTML rendering via minijinja.
//!
//! Templates live under one or more directories on disk. At boot,
//! `App::build()` assembles an ordered search list:
//!
//! 1. The project-level directory configured via
//!    `AppBuilder::templates_dir` (default `./templates`).
//! 2. Each registered plugin's `Plugin::templates_dirs()` contributions,
//!    in topological dependency order.
//!
//! The first directory that contains a given template name wins. This
//! makes cross-plugin `{% extends "base.html" %}` work automatically —
//! the extends lookup searches every directory the same way a direct
//! render call does. Plugin A can extend `base.html` from plugin B as
//! long as B's directory appears in the search list.
//!
//! When two directories both provide a template with the same name, the
//! first-match-wins policy applies and a `tracing::warn!` is emitted at
//! boot so the collision is visible in the log. This matches Django's
//! `APP_DIRS` loader semantics. Silently-overridden templates are a
//! well-known footgun, so the warning is non-optional.
//!
//! Rendering goes through one ambient accessor, [`render`], which reads
//! the engine the App builder published into an `OnceLock` during build.
//!
//! ```ignore
//! let html = umbra::templates::render("articles_list.html", &context!(articles))?;
//! ```
//!
//! ## Autoescape
//!
//! Any template whose name ends in `.html` or `.htm` renders with
//! autoescape on. Text templates (`.txt`) render verbatim. The autoescape
//! callback extension whitelist MUST stay in sync with the loader's
//! `load_directory` filter (currently `html | htm | txt`).
//!
//! ## v1 scope
//!
//! - One project-level templates directory (default `./templates/`,
//!   relative to the binary's cwd) plus per-plugin directories.
//! - Jinja2-compatible syntax via minijinja: `{% extends %}`, `{% block %}`,
//!   `{% if %}`, `{% for %}`, `{{ value }}`, the standard filter set.
//! - Autoescape for any template whose name ends in `.html` or `.htm`.
//! - Init is best-effort: if no directory exists the engine boots empty.
//!   Calls to [`render`] then return `TemplateError::Missing`.
//!
//! ## Deferred
//!
//! - Custom filters and tests registered through `Plugin::on_ready`.
//! - Hot reload in development via `minijinja-autoreload`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use minijinja::{AutoEscape, Environment};
use serde::Serialize;

static ENGINE: OnceLock<Environment<'static>> = OnceLock::new();

/// Register the built-in default 404/500 templates into an environment.
///
/// Called from `init` before any disk directories are scanned. The names
/// use the `__umbra__/` prefix so they can never collide with a user's
/// `templates/` directory (slashes aren't meaningful to the engine's name
/// lookup — `__umbra__/default_404.html` is just a unique string key).
///
/// Because the user's disk directories are added after this call and
/// first-match-wins is enforced by the `seen` set, a user who places a
/// file named `__umbra__/default_404.html` in their own templates dir will
/// silently replace the built-in — which is the intended escape hatch.
/// (Callers who want a cleaner opt-out should use
/// `App::builder().disable_default_error_pages()` instead.)
fn register_default_templates(
    env: &mut Environment<'static>,
    seen: &mut std::collections::HashSet<String>,
) {
    let entries = [
        (
            crate::errors::DEFAULT_404_TEMPLATE_NAME,
            crate::errors::DEFAULT_404_HTML,
        ),
        (
            crate::errors::DEFAULT_500_TEMPLATE_NAME,
            crate::errors::DEFAULT_500_HTML,
        ),
    ];
    for (name, source) in entries {
        if seen.contains(name) {
            continue; // already provided by user — skip
        }
        // These are compile-time constants so they're `&'static str`; we can
        // add them without cloning via `add_template` (non-owned variant).
        if env.add_template(name, source).is_ok() {
            seen.insert(name.to_string());
        }
    }
}

/// Publish the template engine into the process-wide ambient handle.
///
/// `dirs` is the ordered list of directories to search — the first
/// entry is searched first (highest priority). Typically this is:
/// `[app_templates_dir, plugin_a_dir, plugin_b_dir, ...]`.
///
/// For each directory in order, every `.html` / `.htm` / `.txt` file is
/// registered under its path-relative-to-that-dir name. If a name was
/// already registered by an earlier directory, the later file is skipped
/// and a `tracing::warn!` is emitted so the collision is visible.
///
/// If none of the directories exist, init succeeds with an empty engine.
/// This is the right default for binaries that don't render HTML.
///
/// Returns the list of template names that collided (appeared in more
/// than one directory). The caller (`App::build`) logs these via tracing.
/// Tests can inspect the returned list to assert collision detection
/// without needing a tracing subscriber.
pub fn init(dirs: &[PathBuf]) -> Result<Vec<String>, TemplateError> {
    let mut env = Environment::new();
    // Autoescape extensions MUST stay in sync with the loader
    // whitelist in `load_directory` (currently `html | htm | txt`).
    // If you add `.svg` or `.xml` to the loader, add them HERE too
    // — `.svg` carries inline-script XSS risk and `.xml` is generally
    // parsed by something downstream that wants attribute escaping.
    // `.txt` stays `None` because plaintext rendering shouldn't HTML-
    // escape (would replace `<` with `&lt;` in plain email bodies).
    env.set_auto_escape_callback(|name| {
        if name.ends_with(".html") || name.ends_with(".htm") {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });

    let mut seen: HashSet<String> = HashSet::new();
    let mut collisions: Vec<String> = Vec::new();

    // Register the built-in default error templates before scanning disk
    // directories. Because disk directories are first-match-wins and are
    // scanned after this call, a user template with the same name (unlikely,
    // since the `__umbra__/` prefix is reserved) would silently replace the
    // built-in. Callers who want a clean opt-out should use
    // `App::builder().disable_default_error_pages()`.
    register_default_templates(&mut env, &mut seen);

    for dir in dirs {
        if dir.exists() {
            load_directory(&mut env, dir, dir, &mut seen, &mut collisions)?;
        }
    }

    for name in &collisions {
        tracing::warn!(
            template = %name,
            "umbra templates: template `{name}` is provided by multiple directories; \
             the first-registered copy wins"
        );
    }

    ENGINE
        .set(env)
        .map_err(|_| TemplateError::AlreadyInitialised)?;
    Ok(collisions)
}

/// Render a template by name with a serde-serializable context value.
///
/// The name is the path relative to its templates directory, with
/// forward slashes regardless of host OS. `articles_list.html`,
/// `admin/base.html`, etc.
///
/// Returns `TemplateError::NotInitialised` if `App::build()` hasn't
/// run yet, `TemplateError::Missing` if the name doesn't match a
/// loaded template, and `TemplateError::Render` for any minijinja-
/// reported issue (syntax error, missing variable when strict undefined
/// is on, etc.).
pub fn render<C: Serialize>(name: &str, ctx: &C) -> Result<String, TemplateError> {
    let env = ENGINE.get().ok_or(TemplateError::NotInitialised)?;
    let tmpl = env.get_template(name).map_err(|e| match e.kind() {
        minijinja::ErrorKind::TemplateNotFound => TemplateError::Missing(name.to_string()),
        _ => TemplateError::Render(e),
    })?;
    tmpl.render(ctx).map_err(TemplateError::Render)
}

/// Walk a directory recursively and register every `.html` / `.htm` /
/// `.txt` file as a template under its path-relative-to-root name.
/// Subdirectories are reachable via forward-slash names: `admin/base.html`.
///
/// `seen` tracks which names have already been registered across all
/// directories. When a name collision is detected (a later directory
/// ships a template with the same relative name as an earlier one),
/// the duplicate is skipped and the name is appended to `collisions`.
/// First-match-wins.
fn load_directory(
    env: &mut Environment<'static>,
    root: &Path,
    dir: &Path,
    seen: &mut HashSet<String>,
    collisions: &mut Vec<String>,
) -> Result<(), TemplateError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            load_directory(env, root, &path, seen, collisions)?;
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !matches!(ext, "html" | "htm" | "txt") {
            continue;
        }
        let rel: PathBuf = path
            .strip_prefix(root)
            .expect("walked path is rooted at the templates dir")
            .to_path_buf();
        // minijinja template names are forward-slashed regardless of OS;
        // the path display would emit `\` on Windows, so build the name
        // explicitly.
        let name: String = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");

        if seen.contains(&name) {
            // Collision: a higher-priority directory already registered
            // this name. Record it and skip; init will log after all
            // dirs are processed.
            if !collisions.contains(&name) {
                collisions.push(name.clone());
            }
            continue;
        }

        let source = std::fs::read_to_string(&path)?;
        env.add_template_owned(name.clone(), source)
            .map_err(TemplateError::Render)?;
        seen.insert(name);
    }
    Ok(())
}

/// Errors the template engine can produce. Narrow at v1: load-time IO,
/// engine-not-ready, missing template, render-time minijinja error.
#[derive(Debug)]
pub enum TemplateError {
    /// `App::build()` hasn't run yet, so the ambient engine isn't set.
    NotInitialised,
    /// `init` was called twice — a programming error in the framework
    /// itself, not the user. Surfaced as a `BuildError` if it ever fires.
    AlreadyInitialised,
    /// IO error reading a template file at boot.
    Io(std::io::Error),
    /// The requested template name isn't loaded.
    Missing(String),
    /// Any other minijinja error (syntax, render-time, etc.). The
    /// inner `minijinja::Error` carries the diagnostic (line / col /
    /// undefined name) so the caller can pass it through `Display`.
    Render(minijinja::Error),
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateError::NotInitialised => write!(
                f,
                "umbra templates: engine not initialised — call App::build() first"
            ),
            TemplateError::AlreadyInitialised => {
                write!(f, "umbra templates: init called more than once")
            }
            TemplateError::Io(e) => write!(f, "umbra templates: io: {e}"),
            TemplateError::Missing(name) => write!(
                f,
                "umbra templates: no template named `{name}`; check the templates directory"
            ),
            TemplateError::Render(e) => write!(f, "umbra templates: {e}"),
        }
    }
}

impl std::error::Error for TemplateError {}

impl From<std::io::Error> for TemplateError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
