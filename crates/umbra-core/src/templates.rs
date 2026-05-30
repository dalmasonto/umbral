//! Server-side HTML rendering via minijinja.
//!
//! Templates live under one or more directories on disk. The default v1
//! shape is a single project-level `templates/` directory (configurable
//! via [`AppBuilder::templates_dir`](crate::app::AppBuilder::templates_dir)).
//! Rendering goes through one ambient accessor, [`render`], which reads
//! the engine the App builder published into an `OnceLock` during build.
//!
//! ```ignore
//! let html = umbra::templates::render("articles_list.html", &context!(articles))?;
//! ```
//!
//! ## v1 scope
//!
//! - One project-level templates directory (default `./templates/`,
//!   relative to the binary's cwd).
//! - Jinja2-compatible syntax via minijinja: `{% extends %}`, `{% block %}`,
//!   `{% if %}`, `{% for %}`, `{{ value }}`, the standard filter set.
//! - Autoescape for any template whose name ends in `.html`. Other
//!   extensions render verbatim (text emails, JSON, etc.). The XSS
//!   guarantee from `arch.md §4.5` lands here: a `<script>` value
//!   rendered into an `.html` template emits `&lt;script&gt;`.
//! - Init is best-effort: if the templates directory doesn't exist,
//!   the engine boots with zero templates. Calls to [`render`] then
//!   return `TemplateError::Missing` with a clear diagnostic.
//!
//! ## Deferred
//!
//! - Per-plugin `templates/` directories with plugin-dependency-ordered
//!   search paths (the natural Django shape; ships with the admin plugin
//!   at M11 since the admin needs override behaviour).
//! - Custom filters and tests registered through `Plugin::on_ready` —
//!   the engine's interior mutability requires a different ambient
//!   pattern than the current `OnceLock<Environment>`; deferred until
//!   the first plugin needs it.
//! - Hot reload in development via `minijinja-autoreload`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use minijinja::{AutoEscape, Environment};
use serde::Serialize;

static ENGINE: OnceLock<Environment<'static>> = OnceLock::new();

/// Publish the template engine into the process-wide ambient handle.
/// Called by `App::build()` after the templates dir has been resolved.
///
/// If the directory doesn't exist, init succeeds with an empty engine
/// (zero templates loaded). This is the right default for binaries
/// that don't render HTML — the absence isn't an error until something
/// actually tries to render.
pub(crate) fn init(templates_dir: &Path) -> Result<(), TemplateError> {
    let mut env = Environment::new();
    env.set_auto_escape_callback(|name| {
        if name.ends_with(".html") || name.ends_with(".htm") {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    if templates_dir.exists() {
        load_directory(&mut env, templates_dir, templates_dir)?;
    }
    ENGINE
        .set(env)
        .map_err(|_| TemplateError::AlreadyInitialised)?;
    Ok(())
}

/// Render a template by name with a serde-serializable context value.
///
/// The name is the path relative to the templates directory, with
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
fn load_directory(
    env: &mut Environment<'static>,
    root: &Path,
    dir: &Path,
) -> Result<(), TemplateError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            load_directory(env, root, &path)?;
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
        let source = std::fs::read_to_string(&path)?;
        env.add_template_owned(name, source)
            .map_err(TemplateError::Render)?;
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
