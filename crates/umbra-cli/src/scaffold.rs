//! Project + plugin scaffolding.
//!
//! Two functions:
//!
//! - [`scaffold_project`] writes a complete new project directory.
//!   Maps to `umbra startproject <name>`.
//! - [`scaffold_app`] writes a new plugin crate at
//!   `plugins/<name>/`. Maps to `umbra startapp <name>`.
//!
//! Both are pure: take a target path and the new name, write files,
//! return what was written. The binary's `main.rs` wraps them with
//! CLI parsing + a stdout report.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Error type for scaffolding operations. Wraps I/O and validation
/// failures with enough context for a user-facing message.
#[derive(Debug)]
pub enum ScaffoldError {
    /// The user-provided name isn't valid as a Rust crate / package
    /// identifier (must be ASCII alphanumeric or underscore/hyphen,
    /// can't start with a digit).
    InvalidName(String),
    /// The target directory already exists. We never overwrite —
    /// users move it aside or pick a different name.
    AlreadyExists(PathBuf),
    /// I/O failure during file creation.
    Io(io::Error),
}

impl std::fmt::Display for ScaffoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(s) => write!(
                f,
                "invalid name `{s}`: must be ASCII alphanumeric, underscore or hyphen, not starting with a digit",
            ),
            Self::AlreadyExists(p) => write!(
                f,
                "target `{}` already exists; move it aside or pick a different name",
                p.display()
            ),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ScaffoldError {}

impl From<io::Error> for ScaffoldError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Report returned by both scaffolding functions: the paths written,
/// so the binary can print them to the user.
#[derive(Debug, Clone)]
pub struct ScaffoldReport {
    /// Root directory the scaffold landed in (project dir, or
    /// `plugins/<name>/`).
    pub root: PathBuf,
    /// All files written, relative to `root`.
    pub files: Vec<PathBuf>,
    /// Post-scaffold instructions for the user. The binary prints
    /// these after the file list.
    pub next_steps: Vec<String>,
}

/// Validate a name is acceptable as a Rust crate identifier.
///
/// Rules: ASCII alphanumeric + `_` + `-`, can't start with a digit,
/// can't be empty. Same rules `cargo new` uses. Crates with hyphens
/// have to use `_` in their Rust identifiers, but Cargo handles the
/// translation transparently — the user can pick either form.
fn validate_name(name: &str) -> Result<(), ScaffoldError> {
    if name.is_empty() {
        return Err(ScaffoldError::InvalidName(String::new()));
    }
    let first = name.chars().next().unwrap();
    if first.is_ascii_digit() {
        return Err(ScaffoldError::InvalidName(name.to_string()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ScaffoldError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// Convert a kebab/snake case name into PascalCase for type names.
///
/// `posts` → `Posts`. `blog-engine` → `BlogEngine`. `task_queue` →
/// `TaskQueue`. Used to generate the `{Name}Plugin` struct name.
fn pascal_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut next_upper = true;
    for c in name.chars() {
        if c == '-' || c == '_' {
            next_upper = true;
        } else if next_upper {
            out.push(c.to_ascii_uppercase());
            next_upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Convert a name to its Rust identifier form (hyphens → underscores).
fn rust_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// Write a new umbra project at `parent_dir/<name>/`.
///
/// The generated layout mirrors umbra's own conventions:
///
/// ```text
/// <name>/
/// ├── Cargo.toml
/// ├── umbra.toml
/// ├── .env.example
/// ├── .gitignore
/// ├── src/
/// │   └── main.rs
/// └── templates/
///     ├── base.html
///     ├── 404.html
///     └── 500.html
/// ```
///
/// `main.rs` wires `umbra_cli::dispatch(app)` so the project's binary
/// hosts the management commands.
pub fn scaffold_project(name: &str, parent_dir: &Path) -> Result<ScaffoldReport, ScaffoldError> {
    validate_name(name)?;

    let root = parent_dir.join(name);
    if root.exists() {
        return Err(ScaffoldError::AlreadyExists(root));
    }

    fs::create_dir_all(&root)?;
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("templates"))?;

    let crate_name = rust_ident(name);
    let mut files = Vec::new();

    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
umbra = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-cli = {{ git = "https://github.com/dalmasonto/umbra" }}
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
tracing-subscriber = {{ version = "0.3", features = ["env-filter"] }}

# Once you `umbra startapp <name>`, add the plugin crate here:
# {crate_name}-plugin = {{ path = "plugins/<name>" }}
"#
    );
    write_file(&root, "Cargo.toml", &cargo_toml, &mut files)?;

    let main_rs = format!(
        r#"//! {name} — generated by `umbra startproject {name}`.
//!
//! The binary hosts umbra's management commands via
//! [`umbra_cli::dispatch`]. Run subcommands with `cargo run --
//! <command>`:
//!
//! ```text
//! cargo run -- serve
//! cargo run -- migrate
//! cargo run -- makemigrations
//! cargo run -- showmigrations
//! ```

use umbra::prelude::*;
use umbra::web::SlashRedirect;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {{
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = Settings::from_env()?;
    let pool = umbra::db::connect(&settings.database_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        // Add your models here as you declare them:
        //   .model::<your_module::Article>()
        // Add your plugins (apps) here once you `umbra startapp <name>`:
        //   .plugin(your_plugin::YourPlugin::default())
        .templates_dir("templates")
        .not_found_template("404.html")
        .server_error_template("500.html")
        .slash_redirect(SlashRedirect::Append)
        .router(Router::new().route("/", get(home)))
        .build()?;

    umbra_cli::dispatch(app).await
}}

async fn home() -> umbra::web::Html<&'static str> {{
    umbra::web::Html("<h1>{name}</h1><p>Generated by <code>umbra startproject</code>.</p>")
}}
"#
    );
    write_file(&root, "src/main.rs", &main_rs, &mut files)?;

    let umbra_toml = format!(
        r#"# umbra settings for {name}. Environment variables (UMBRA_*) override
# these at runtime. See umbra::settings for the full schema.

# Override `database_url` here or via UMBRA_DATABASE_URL.
database_url = "sqlite://{name}.db?mode=rwc"

# Bind address for `cargo run -- serve`. Override via UMBRA_BIND_ADDR
# or the `--addr` flag.
bind_addr = "127.0.0.1:8000"

environment = "dev"

# CHANGE THIS IN PRODUCTION. The framework's boot-time `settings.required`
# check fails when this is left at the dev default in production.
secret_key = "umbra-insecure-dev-key-change-me"
"#
    );
    write_file(&root, "umbra.toml", &umbra_toml, &mut files)?;

    let env_example = r#"# Copy to `.env` and source from your shell, or use a tool like
# direnv. Settings here override the umbra.toml values at runtime.
#
# UMBRA_SECRET_KEY=$(openssl rand -hex 32)
# UMBRA_DATABASE_URL=sqlite://my.db?mode=rwc
# UMBRA_BIND_ADDR=0.0.0.0:8000
# UMBRA_ENVIRONMENT=prod
# RUST_LOG=info,umbra=debug
"#;
    write_file(&root, ".env.example", env_example, &mut files)?;

    let gitignore = format!("/target\n/{name}.db*\n.env\nCargo.lock\n",);
    write_file(&root, ".gitignore", &gitignore, &mut files)?;

    let base_html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>{{% block title %}}{name}{{% endblock %}}</title>
</head>
<body>
  {{% block content %}}{{% endblock %}}
</body>
</html>
"#
    );
    write_file(&root, "templates/base.html", &base_html, &mut files)?;

    let not_found_html = r#"{% extends "base.html" %}
{% block title %}Page not found{% endblock %}
{% block content %}
  <h1>Page not found</h1>
  <p>The page <code>{{ path }}</code> doesn't exist.</p>
  <a href="/">Go home</a>
{% endblock %}
"#;
    write_file(&root, "templates/404.html", not_found_html, &mut files)?;

    let server_error_html = r#"{% extends "base.html" %}
{% block title %}Something went wrong{% endblock %}
{% block content %}
  <h1>Something went wrong</h1>
  <p>We've been notified and are looking into it.</p>
{% endblock %}
"#;
    write_file(&root, "templates/500.html", server_error_html, &mut files)?;

    let next_steps = vec![
        format!("cd {name}"),
        "cargo run -- serve   # boot the HTTP server".to_string(),
        "cargo run -- migrate # run pending migrations".to_string(),
        "umbra startapp <name> # scaffold a plugin (app)".to_string(),
    ];

    Ok(ScaffoldReport {
        root,
        files,
        next_steps,
    })
}

/// Write a new plugin crate at `<project_root>/plugins/<name>/`.
///
/// ```text
/// plugins/<name>/
/// ├── Cargo.toml
/// └── src/
///     └── lib.rs
/// ```
///
/// `lib.rs` declares an empty `{Name}Plugin` struct + `Plugin` impl
/// stub. The user wires it into their App by adding `.plugin(...)`
/// to the builder chain — the next_steps in the returned report spell
/// out the exact lines.
pub fn scaffold_app(name: &str, project_root: &Path) -> Result<ScaffoldReport, ScaffoldError> {
    validate_name(name)?;

    let plugins_dir = project_root.join("plugins");
    let root = plugins_dir.join(name);
    if root.exists() {
        return Err(ScaffoldError::AlreadyExists(root));
    }

    fs::create_dir_all(&root)?;
    fs::create_dir_all(root.join("src"))?;

    let crate_name = rust_ident(name);
    let pascal = pascal_case(name);
    let mut files = Vec::new();

    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
umbra = {{ git = "https://github.com/dalmasonto/umbra" }}
"#
    );
    write_file(&root, "Cargo.toml", &cargo_toml, &mut files)?;

    let lib_rs = format!(
        r#"//! {pascal}Plugin — generated by `umbra startapp {name}`.
//!
//! Wire this into your App by adding to `src/main.rs`:
//!
//! ```ignore
//! .plugin({crate_name}::{pascal}Plugin::default())
//! ```
//!
//! Declare models, routes, and `on_ready` work in the impl below.
//! See `documentation/docs/v0.0.1/plugins/the-plugin-trait.mdx` for
//! what each method does.

use umbra::plugin::{{AppContext, Plugin, PluginError}};
use umbra::web::Router;

#[derive(Debug, Default, Clone)]
pub struct {pascal}Plugin;

impl Plugin for {pascal}Plugin {{
    fn name(&self) -> &'static str {{
        "{name}"
    }}

    fn routes(&self) -> Router {{
        // Add your routes here. The base path is up to you — convention
        // is `/<name>/...` for HTML and `/api/<name>/...` for JSON.
        Router::new()
    }}

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {{
        Ok(())
    }}
}}
"#
    );
    write_file(&root, "src/lib.rs", &lib_rs, &mut files)?;

    let next_steps = vec![
        format!("Add `{name}` to your project's Cargo.toml dependencies:"),
        format!("    {name} = {{ path = \"plugins/{name}\" }}"),
        "Add the plugin to your App::builder chain in src/main.rs:".to_string(),
        format!("    .plugin({crate_name}::{pascal}Plugin::default())"),
    ];

    Ok(ScaffoldReport {
        root,
        files,
        next_steps,
    })
}

/// Write a file under `root` at the given relative path. Records the
/// relative path in `files` for the user-facing report.
fn write_file(
    root: &Path,
    rel_path: &str,
    contents: &str,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let full = root.join(rel_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&full, contents)?;
    files.push(PathBuf::from(rel_path));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_simple_identifiers() {
        assert!(validate_name("posts").is_ok());
        assert!(validate_name("blog_engine").is_ok());
        assert!(validate_name("blog-engine").is_ok());
        assert!(validate_name("api2").is_ok());
    }

    #[test]
    fn validate_name_rejects_empty() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn validate_name_rejects_leading_digit() {
        assert!(validate_name("2cool").is_err());
    }

    #[test]
    fn validate_name_rejects_special_chars() {
        assert!(validate_name("foo bar").is_err());
        assert!(validate_name("foo!bar").is_err());
        assert!(validate_name("foo/bar").is_err());
    }

    #[test]
    fn pascal_case_handles_kebab_and_snake() {
        assert_eq!(pascal_case("posts"), "Posts");
        assert_eq!(pascal_case("blog_engine"), "BlogEngine");
        assert_eq!(pascal_case("blog-engine"), "BlogEngine");
        assert_eq!(pascal_case("api2"), "Api2");
    }

    #[test]
    fn rust_ident_replaces_hyphens() {
        assert_eq!(rust_ident("blog-engine"), "blog_engine");
        assert_eq!(rust_ident("posts"), "posts");
    }
}
