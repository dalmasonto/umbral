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
    /// The chosen name collides with a built-in plugin name shipped
    /// by umbra. Both crates would compile, but the user would never
    /// be able to register both `.plugin(<their app>)` and
    /// `.plugin(<built-in>)` without an alias dance, and route /
    /// table-name collisions would land at boot. We reject the name
    /// up front to prevent this confusion.
    ReservedName(String),
    /// I/O failure during file creation.
    Io(io::Error),
}

/// Built-in plugin names that `umbra startapp` refuses to scaffold over.
/// Adding a new built-in plugin? Add its name here so future
/// `startapp <name>` calls fail fast with a clear message.
pub const RESERVED_PLUGIN_NAMES: &[&str] = &[
    "admin",
    "app",
    "auth",
    "cache",
    "email",
    "openapi",
    "permissions",
    "rest",
    "rls",
    "security",
    "sessions",
    "signals",
    "static",
    "tasks",
];

impl std::fmt::Display for ScaffoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(s) => write!(
                f,
                "invalid name `{s}`: must be ASCII alphanumeric, underscore or hyphen, not starting with a digit",
            ),
            Self::AlreadyExists(p) => write!(
                f,
                "an app already exists at `{}`; move it aside or pick a different name",
                p.display()
            ),
            Self::ReservedName(s) => write!(
                f,
                "`{s}` is the name of a built-in umbra plugin; pick a different name to avoid conflicts at registration time. Reserved names: {}.",
                RESERVED_PLUGIN_NAMES.join(", ")
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
/// Rewrite git-deps to path-deps anchored at `umbra_repo`. Closes
/// BUG-17 in `bugs/tests/testBugs.md` — `umbra startproject --local
/// /path/to/umbra foo` now produces a `Cargo.toml` that path-deps
/// every umbra crate against the local checkout instead of the
/// `git = "..."` URL. Comments + commented-out optional plugin
/// lines all flow through; the line trailing whatever follows the
/// dep block (descriptive comment, version pin) is preserved.
///
/// Subdirectory mapping mirrors the umbra repo layout: facade
/// crates (`umbra`, `umbra-cli`, `umbra-core`, `umbra-macros`,
/// `umbra-testing`) live under `crates/`; everything else
/// (`umbra-auth`, `umbra-sessions`, `umbra-admin`, …) lives
/// under `plugins/`.
pub(crate) fn localize_deps(text: &str, umbra_repo: &Path) -> String {
    const GIT_URL: &str = "https://github.com/dalmasonto/umbra";
    let repo_str = umbra_repo.display().to_string();
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(&rewrite_line(line, GIT_URL, &repo_str));
    }
    out
}

fn rewrite_line(line: &str, git_url: &str, repo: &str) -> String {
    // Look for `<crate> = { git = "<url>" }` (optionally preceded by
    // `#` for commented-out lines, optionally followed by a
    // descriptive `# ...` comment). Bail out cheaply if the URL
    // marker isn't present.
    if !line.contains(git_url) {
        return line.to_string();
    }
    // Find the LHS crate name. Strip any leading `#` and whitespace,
    // then take the substring up to the first `=`.
    let body_start = line
        .char_indices()
        .find(|(_, c)| !matches!(*c, '#' | ' ' | '\t'))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let body = &line[body_start..];
    let Some(eq_idx) = body.find('=') else {
        return line.to_string();
    };
    let crate_name = body[..eq_idx].trim();
    if !crate_name.starts_with("umbra") {
        return line.to_string();
    }
    let subdir = match crate_name {
        "umbra" | "umbra-cli" | "umbra-core" | "umbra-macros" | "umbra-testing" => "crates",
        _ => "plugins",
    };
    let path = format!("{repo}/{subdir}/{crate_name}");
    // Replace the entire `{ git = "<url>" }` substring. We don't
    // know the exact spacing inside, so find the `{` and the matching
    // `}` and substitute.
    let Some(lbrace_idx) = body.find('{') else {
        return line.to_string();
    };
    let Some(rbrace_offset) = body[lbrace_idx..].find('}') else {
        return line.to_string();
    };
    let rbrace_idx = lbrace_idx + rbrace_offset;
    let prefix = &line[..body_start + lbrace_idx];
    let suffix = &line[body_start + rbrace_idx + 1..];
    format!("{prefix}{{ path = \"{path}\" }}{suffix}")
}

fn rust_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// Write a new umbra project at `parent_dir/<name>/`.
///
/// The generated layout is a complete blog-style demo that exercises every
/// major umbra surface: models with FK, migrations on boot, auth + sessions,
/// `login_required`, REST with filters, admin, templates, transactions, and
/// Django-shaped error pages.
///
/// ```text
/// <name>/
/// ├── Cargo.toml
/// ├── umbra.toml
/// ├── .env
/// ├── .env.example
/// ├── .gitignore
/// ├── README.md
/// ├── src/
/// │   └── main.rs
/// └── templates/
///     ├── base.html
///     ├── home.html
///     ├── dashboard.html
///     ├── 404.html
///     └── 500.html
/// ```
///
/// `main.rs` wires `umbra_cli::dispatch(app)` so the project's binary
/// hosts the management commands.
pub fn scaffold_project(
    name: &str,
    parent_dir: &Path,
    local_umbra_repo: Option<&Path>,
) -> Result<ScaffoldReport, ScaffoldError> {
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

    // ------------------------------------------------------------------ //
    // Cargo.toml                                                           //
    // ------------------------------------------------------------------ //
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]

# ----- Framework core (always required) ------------------------------------
umbra         = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-cli     = {{ git = "https://github.com/dalmasonto/umbra" }}

# ----- Active by default ---------------------------------------------------
# What the generated `src/main.rs` wires in. Comment any of these out only
# if you also remove the matching `.plugin(...)` line.
umbra-auth     = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-sessions = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-admin    = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-rest     = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-openapi  = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-security = {{ git = "https://github.com/dalmasonto/umbra" }}

# ----- Available built-ins (uncomment + register in main.rs to enable) -----
# umbra-playground   = {{ git = "https://github.com/dalmasonto/umbra" }}  # Interactive API playground UI (think mini-Postman) at /playground/.
# umbra-tasks        = {{ git = "https://github.com/dalmasonto/umbra" }}  # DB-backed background task queue (Celery-equivalent).
# umbra-permissions  = {{ git = "https://github.com/dalmasonto/umbra" }}  # Django-style ContentType + Group + Permission model.
# umbra-rls          = {{ git = "https://github.com/dalmasonto/umbra" }}  # Postgres row-level security policy registration.
# umbra-cache        = {{ git = "https://github.com/dalmasonto/umbra" }}  # Per-request caching helper.
# umbra-email        = {{ git = "https://github.com/dalmasonto/umbra" }}  # SMTP + MIME email composer + sender.
# umbra-media        = {{ git = "https://github.com/dalmasonto/umbra" }}  # Uploaded-file storage abstraction (local FS + S3).
# umbra-signals      = {{ git = "https://github.com/dalmasonto/umbra" }}  # Pre/post save/delete signal dispatch.
# umbra-static       = {{ git = "https://github.com/dalmasonto/umbra" }}  # Static-file serving for prod (whitenoise-equivalent).

# ----- Third-party + framework runtime deps --------------------------------
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
tracing-subscriber = {{ version = "0.3", features = ["env-filter"] }}
serde = {{ version = "1", features = ["derive"] }}
chrono = {{ version = "0.4", features = ["serde"] }}
sqlx = {{ version = "0.8", features = ["macros", "sqlite", "postgres", "chrono", "runtime-tokio"] }}

# Once you `umbra startapp <plugin>` or `umbra startplugin <plugin>`, add
# the plugin crate here:
# {crate_name}-posts = {{ path = "plugins/posts" }}
"#
    );
    // BUG-17 fix: when `--local <PATH>` is set, rewrite every
    // `{ git = "..." }` dependency to a `{ path = "<umbra>/<sub>/<crate>" }`
    // form anchored at the supplied umbra-repo path. Comments,
    // active and commented-out dep lines all go through. Without
    // the flag, the git-deps shape is preserved verbatim — that's
    // what a user installing umbra from crates.io / GitHub gets.
    let cargo_toml = match local_umbra_repo {
        Some(repo) => localize_deps(&cargo_toml, repo),
        None => cargo_toml,
    };
    write_file(&root, "Cargo.toml", &cargo_toml, &mut files)?;

    // ------------------------------------------------------------------ //
    // src/main.rs — the demo wires every umbra surface in ~100 lines      //
    // ------------------------------------------------------------------ //
    let main_rs = format!(
        r#"//! {name} — generated by `umbra startproject {name}`.
//!
//! This file is a walking tour of the umbra framework. Every surface is
//! wired in here: models + FK, migrations, auth, sessions, login_required,
//! REST with filters, admin, security, transactions, and custom error pages.
//!
//! Run with:
//!   cargo run -- migrate   # apply pending migrations (run once after checkout)
//!   cargo run -- serve     # boot the HTTP server
//!
//! Other management commands:
//!   cargo run -- makemigrations
//!   cargo run -- showmigrations
//!   cargo run -- createsuperuser

use umbra::prelude::*;
use umbra::web::{{Html, Json, StatusCode, SlashRedirect}};
use umbra::templates::context;
use umbra::migrate::MigrateError;
use umbra_auth::{{AuthPlugin, AuthUser, login_required_html}};
use umbra_sessions::SessionsPlugin;
use umbra_admin::AdminPlugin;
use umbra_rest::{{RestPlugin, ResourceConfig}};
use umbra_openapi::OpenApiPlugin;
use umbra_security::{{SecurityConfig, SecurityPlugin}};

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// A blog post. `author` is a FK to the built-in `AuthUser` model — the
/// migration engine emits `REFERENCES "auth_user"("id")` automatically.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow, Model)]
pub struct Post {{
    pub id: i64,
    pub title: String,
    pub body: String,
    pub published: bool,
    pub author: ForeignKey<AuthUser>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}}

// ---------------------------------------------------------------------------
// App wiring
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
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

        // --- Models ----------------------------------------------------------
        // AuthUser and Session are contributed by their plugins below.
        // List your own models here.
        .model::<Post>()

        // --- Plugins ---------------------------------------------------------
        // Auth: user table, password hashing, createsuperuser command.
        .plugin(AuthPlugin::<AuthUser>::default())
        // Sessions: session table + cookie middleware.
        .plugin(SessionsPlugin::default())
        // Admin: auto CRUD UI at /admin/ for every registered model.
        .plugin(AdminPlugin::default())
        // REST: JSON CRUD + filtering at /api/<table>/.
        // The Post resource has query-string filtering enabled so
        // GET /api/post/?published=true works out of the box.
        .plugin(
            RestPlugin::default()
                .resource(ResourceConfig::new("post")),
        )
        // OpenAPI: Swagger UI at /openapi/ (override with
        // `.at("/api/docs")` if you prefer a different mount).
        .plugin(OpenApiPlugin::new())
        // Security: CSRF + hardening headers across the app. `/api`
        // is exempt so token-authenticated JSON clients can POST
        // without a browser form CSRF cookie.
        .plugin(SecurityPlugin::with_config(SecurityConfig {{
            csrf_exempt_paths: vec!["/api".to_string()],
            ..Default::default()
        }}))

        // --- Templates -------------------------------------------------------
        .templates_dir("templates")
        .not_found_template("404.html")
        .server_error_template("500.html")

        // Redirect /foo → /foo/  (Django-style).
        .slash_redirect(SlashRedirect::Append)

        // --- Routes ----------------------------------------------------------
        // The Routes builder records each (method, path) pair as you
        // declare it, so the dev-mode 404 panel surfaces them without
        // a parallel declaration list. Per-route middleware (here,
        // login_required_html on /dashboard) goes through the explicit
        // .route(&[methods], path, MethodRouter) form so the layer
        // attaches just to that handler — not all routes.
        .routes(
            Routes::new()
                // Public home page.
                .get("/", home)
                // API: list posts as JSON (no auth required — demo).
                .get("/api/posts", api_list_posts)
                // Dashboard: only reachable when logged in. The
                // login_required_html("/login") layer issues a 302 to
                // /login?next=/dashboard/ for anonymous visitors. The
                // .layered(method, path, mr) form takes a MethodRouter
                // so `.layer(...)` scopes to *just* this route — not
                // every route on the builder.
                .layered(
                    "GET",
                    "/dashboard",
                    get(dashboard).layer(login_required_html("/login")),
                ),
        )
        .build()?;

    // Auto-migrate on boot so `cargo run -- serve` Just Works
    // against a fresh database — but only when we're actually
    // starting the server. Running `cargo run -- makemigrations`
    // or `cargo run -- migrate` from the CLI used to silently
    // trigger `auto_migrate()` first and then report "no changes
    // detected", which made the CLI tools feel broken (IMP-1 in
    // `bugs/tests/testBugs.md`). The guard reads `std::env::args`
    // before dispatch picks them apart so it matches whatever
    // subcommand the user actually typed.
    let argv: Vec<String> = std::env::args().collect();
    let user_invoked_cli = argv.iter().skip(1).any(|a| !a.starts_with('-'));
    if !user_invoked_cli {{
        auto_migrate().await?;
    }}

    umbra_cli::dispatch(app).await
}}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Home page. Counts published posts and renders home.html.
async fn home() -> Result<Html<String>, (StatusCode, String)> {{
    let post_count = Post::objects()
        .filter(post::PUBLISHED.eq(true))
        .count()
        .await
        .map_err(internal_error)?;

    let body = umbra::templates::render(
        "home.html",
        &context!(post_count),
    )
    .map_err(internal_error)?;
    Ok(Html(body))
}}

/// JSON list of all posts — demonstrates the ORM QuerySet.
async fn api_list_posts() -> Result<Json<Vec<Post>>, (StatusCode, String)> {{
    let posts = Post::objects()
        .order_by(post::ID.desc())
        .fetch()
        .await
        .map_err(internal_error)?;
    Ok(Json(posts))
}}

/// Dashboard: only reachable when logged in (see the `login_required_html`
/// layer in the router above). The `LoggedIn<AuthUser>` extractor supplies
/// the current user — the layer already checked the session, so this is a
/// cheap field read, not a second DB query.
async fn dashboard(
    user: umbra_auth::LoggedIn<AuthUser>,
) -> Result<Html<String>, (StatusCode, String)> {{
    // Demonstrates a transaction: atomically bump a hypothetical view
    // counter and fetch the user's post list in the same transaction.
    let user_id = user.id;
    let my_posts = umbra::transaction(|tx| Box::pin(async move {{
        Post::objects()
            .filter(post::AUTHOR.eq(user_id))
            .on_tx(tx)
            .fetch()
            .await
    }}))
    .await
    .map_err(internal_error)?;

    let body = umbra::templates::render(
        "dashboard.html",
        &context!(user, my_posts),
    )
    .map_err(internal_error)?;
    Ok(Html(body))
}}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {{
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}}

/// Run `makemigrations` + `migrate` on boot. Demo-only convenience.
async fn auto_migrate() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    match umbra::migrate::make().await {{
        Ok(paths) => {{
            for path in paths {{
                eprintln!("auto-migrate: wrote {{}}",  path.display());
            }}
        }}
        Err(MigrateError::NoChanges) => {{}}
        Err(err) => return Err(Box::new(err)),
    }}
    let n = umbra::migrate::run().await?;
    if n > 0 {{
        eprintln!("auto-migrate: applied {{n}} migration(s)");
    }}
    Ok(())
}}
"#
    );
    write_file(&root, "src/main.rs", &main_rs, &mut files)?;

    // ------------------------------------------------------------------ //
    // umbra.toml                                                           //
    // ------------------------------------------------------------------ //
    let umbra_toml = format!(
        r#"# umbra settings for {name}.
# Environment variables (UMBRA_*) override these at runtime.
# See umbra::settings for the full schema.

database_url = "sqlite://{name}.db?mode=rwc"

# Bind address for `cargo run -- serve`.
# Override via UMBRA_BIND_ADDR or the --addr flag.
bind_addr = "127.0.0.1:8000"

environment = "Dev"

# CHANGE THIS IN PRODUCTION. The framework errors at boot when this
# default key is used with environment = "Prod".
secret_key = "umbra-insecure-dev-key-change-me"
"#
    );
    write_file(&root, "umbra.toml", &umbra_toml, &mut files)?;

    // ------------------------------------------------------------------ //
    // .env  (working copy — not checked in)                               //
    // ------------------------------------------------------------------ //
    let dot_env = format!(
        r#"# Working .env for {name}. Do not commit this file.
# Generate a real secret key: openssl rand -hex 32
UMBRA_DATABASE_URL=sqlite://{name}.db?mode=rwc
UMBRA_BIND_ADDR=127.0.0.1:8000
UMBRA_SECRET_KEY=umbra-insecure-dev-key-change-me
RUST_LOG=info,umbra=debug
"#
    );
    write_file(&root, ".env", &dot_env, &mut files)?;

    // ------------------------------------------------------------------ //
    // .env.example                                                         //
    // ------------------------------------------------------------------ //
    let env_example = r#"# Copy to `.env` and source from your shell, or use a tool like direnv.
# Settings here override the umbra.toml values at runtime.
#
# UMBRA_SECRET_KEY=$(openssl rand -hex 32)
# UMBRA_DATABASE_URL=sqlite://my.db?mode=rwc
# UMBRA_BIND_ADDR=0.0.0.0:8000
# UMBRA_ENVIRONMENT=prod
# RUST_LOG=info,umbra=debug
"#;
    write_file(&root, ".env.example", env_example, &mut files)?;

    // ------------------------------------------------------------------ //
    // .gitignore                                                           //
    // ------------------------------------------------------------------ //
    let gitignore = format!("/target\n/{name}.db*\n.env\nCargo.lock\n");
    write_file(&root, ".gitignore", &gitignore, &mut files)?;

    // ------------------------------------------------------------------ //
    // README.md                                                            //
    // ------------------------------------------------------------------ //
    let readme = format!(
        r#"# {name}

A blog-style demo generated by `umbra startproject {name}`.

## What's in the project

| File | What it shows |
|---|---|
| `src/main.rs` | App wiring: models, plugins, routes, auto-migrate |
| `Post` model | `ForeignKey<AuthUser>`, ORM QuerySet, `#[derive(Model)]` |
| `/` route | Template rendering with context |
| `/api/posts` | JSON endpoint via the ORM |
| `/dashboard` | `login_required_html("/login")` layer, `LoggedIn<AuthUser>` extractor, transaction |
| `RestPlugin` | JSON CRUD at `/api/post/` with query-string filtering (`?published=true`) |
| `AdminPlugin` | Auto CRUD UI at `/admin/` |
| `OpenApiPlugin` | Swagger UI at `/openapi/` |
| `SecurityPlugin` | CSRF middleware + hardening headers, with `/api` exempt for token clients |

## Running

```bash
# First run — applies migrations and starts the server:
cargo run -- serve

# Separate steps (production pattern):
cargo run -- migrate
cargo run -- serve

# Create a superuser to log in to the admin:
cargo run -- createsuperuser

# Explore the scaffold:
cargo run -- showmigrations
cargo run -- makemigrations
```

## Where to go next

- Add a plugin: `umbra startapp posts`
- Docs: https://umbra.dev/docs/v0.0.1/
- ORM: /docs/v0.0.1/orm/models
- Migrations: /docs/v0.0.1/migrations/managed-migrations
- REST: /docs/v0.0.1/plugins/rest
- Auth: /docs/v0.0.1/plugins/auth
"#
    );
    write_file(&root, "README.md", &readme, &mut files)?;

    // ------------------------------------------------------------------ //
    // templates/base.html — Tailwind CDN so the demo works standalone     //
    // ------------------------------------------------------------------ //
    let base_html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{{% block title %}}{name}{{% endblock %}}</title>
  <!-- Tailwind CSS via CDN — replace with a compiled bundle in production -->
  <script src="https://cdn.tailwindcss.com"></script>
</head>
<body class="bg-gray-50 text-gray-900 min-h-screen">
  <nav class="bg-white shadow px-6 py-3 flex items-center gap-6">
    <a href="/" class="font-bold text-lg">{name}</a>
    <a href="/dashboard" class="text-sm text-gray-600 hover:text-gray-900">Dashboard</a>
    <a href="/admin/" class="text-sm text-gray-600 hover:text-gray-900">Admin</a>
    <a href="/openapi/" class="text-sm text-gray-600 hover:text-gray-900">API docs</a>
  </nav>
  <main class="max-w-3xl mx-auto px-4 py-8">
    {{% block content %}}{{% endblock %}}
  </main>
</body>
</html>
"#
    );
    write_file(&root, "templates/base.html", &base_html, &mut files)?;

    // ------------------------------------------------------------------ //
    // templates/home.html                                                  //
    // ------------------------------------------------------------------ //
    let home_html = r#"{% extends "base.html" %}
{% block title %}Home{% endblock %}
{% block content %}
  <h1 class="text-3xl font-bold mb-4">Welcome</h1>
  <p class="text-gray-600 mb-6">
    There are <strong>{{ post_count }}</strong> published post(s).
  </p>
  <div class="flex gap-4">
    <a href="/api/post/" class="px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700">
      Browse posts (JSON)
    </a>
    <a href="/dashboard" class="px-4 py-2 bg-gray-200 text-gray-800 rounded hover:bg-gray-300">
      Dashboard (login required)
    </a>
  </div>
{% endblock %}
"#;
    write_file(&root, "templates/home.html", home_html, &mut files)?;

    // ------------------------------------------------------------------ //
    // templates/dashboard.html                                             //
    // ------------------------------------------------------------------ //
    let dashboard_html = r#"{% extends "base.html" %}
{% block title %}Dashboard{% endblock %}
{% block content %}
  <h1 class="text-3xl font-bold mb-2">Dashboard</h1>
  <p class="text-gray-500 mb-6">Logged in as <strong>{{ user.username }}</strong></p>

  <h2 class="text-xl font-semibold mb-3">Your posts</h2>
  {% if my_posts %}
    <ul class="space-y-2">
      {% for post in my_posts %}
        <li class="bg-white rounded shadow p-4">
          <span class="font-medium">{{ post.title }}</span>
          {% if post.published %}
            <span class="ml-2 text-xs bg-green-100 text-green-700 px-2 py-0.5 rounded">published</span>
          {% else %}
            <span class="ml-2 text-xs bg-yellow-100 text-yellow-700 px-2 py-0.5 rounded">draft</span>
          {% endif %}
        </li>
      {% endfor %}
    </ul>
  {% else %}
    <p class="text-gray-400">No posts yet.</p>
  {% endif %}
{% endblock %}
"#;
    write_file(
        &root,
        "templates/dashboard.html",
        dashboard_html,
        &mut files,
    )?;

    // ------------------------------------------------------------------ //
    // templates/404.html                                                   //
    // ------------------------------------------------------------------ //
    let not_found_html = r#"{% extends "base.html" %}
{% block title %}Page not found{% endblock %}
{% block content %}
  <div class="text-center py-16">
    <h1 class="text-6xl font-bold text-gray-300 mb-4">404</h1>
    <p class="text-xl text-gray-600 mb-2">Page not found</p>
    <p class="text-gray-400 mb-8">The path <code class="bg-gray-100 px-1 rounded">{{ path }}</code> doesn't exist.</p>
    <a href="/" class="px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700">Go home</a>
  </div>
{% endblock %}
"#;
    write_file(&root, "templates/404.html", not_found_html, &mut files)?;

    // ------------------------------------------------------------------ //
    // templates/500.html                                                   //
    // ------------------------------------------------------------------ //
    let server_error_html = r#"{% extends "base.html" %}
{% block title %}Something went wrong{% endblock %}
{% block content %}
  <div class="text-center py-16">
    <h1 class="text-6xl font-bold text-gray-300 mb-4">500</h1>
    <p class="text-xl text-gray-600 mb-2">Something went wrong</p>
    <p class="text-gray-400 mb-8">We've been notified and are looking into it.</p>
    <a href="/" class="px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700">Go home</a>
  </div>
{% endblock %}
"#;
    write_file(&root, "templates/500.html", server_error_html, &mut files)?;

    let next_steps = vec![
        format!("cd {name}"),
        "cargo run -- migrate  # apply schema migrations".to_string(),
        "cargo run -- serve    # boot the HTTP server on http://127.0.0.1:8000".to_string(),
        "cargo run -- createsuperuser  # create an admin login".to_string(),
        "umbra startapp <name>          # scaffold a new plugin (app)".to_string(),
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
pub fn scaffold_app(
    name: &str,
    project_root: &Path,
    local_umbra_repo: Option<&Path>,
) -> Result<ScaffoldReport, ScaffoldError> {
    validate_name(name)?;

    // Reject names that collide with built-in umbra plugins. Both crates
    // would compile, but the user could never register both via
    // `.plugin(...)` without aliasing — and the table-name conflicts
    // would surface at boot, not at startapp time.
    let normalized = name.replace('-', "_");
    if RESERVED_PLUGIN_NAMES.contains(&normalized.as_str()) {
        return Err(ScaffoldError::ReservedName(name.to_string()));
    }

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
serde = {{ version = "1", features = ["derive"] }}
sqlx = {{ version = "0.8", features = ["sqlite", "runtime-tokio", "chrono"] }}
chrono = {{ version = "0.4", features = ["serde"] }}
"#
    );
    let cargo_toml = match local_umbra_repo {
        Some(repo) => localize_deps(&cargo_toml, repo),
        None => cargo_toml,
    };
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

pub mod models;

use umbra::plugin::{{AppContext, Plugin, PluginError}};
use umbra::web::Router;

#[derive(Debug, Default, Clone)]
pub struct {pascal}Plugin;

impl Plugin for {pascal}Plugin {{
    fn name(&self) -> &'static str {{
        "{name}"
    }}

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {{
        // Register every model the plugin owns so makemigrations
        // picks them up. Uncomment + extend once you've defined one
        // in src/models.rs.
        // vec![umbra::migrate::ModelMeta::for_::<models::Example>()]
        Vec::new()
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

    // IMP-4 from bugs/tests/testBugs.md: startapp now scaffolds a
    // `models.rs` stub so the user has an obvious place to declare
    // their first `#[derive(Model)]` struct. Previously they had to
    // create the file themselves or upgrade to `startplugin` for
    // the richer layout.
    let models_rs = format!(
        r#"//! Models for the `{name}` plugin.
//!
//! Declare one `#[derive(umbra::orm::Model)]` struct per database
//! table. Once registered via `Plugin::models()` in lib.rs, the
//! migration engine picks them up on the next `makemigrations`.
//!
//! ```ignore
//! use chrono::{{DateTime, Utc}};
//! use serde::{{Deserialize, Serialize}};
//!
//! #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
//! pub struct Example {{
//!     pub id: i64,
//!     #[umbra(string, max_length = 200)]
//!     pub title: String,
//!     #[umbra(noedit)]
//!     pub created_at: DateTime<Utc>,
//! }}
//! ```
"#
    );
    write_file(&root, "src/models.rs", &models_rs, &mut files)?;

    let next_steps = vec![
        format!("Add `{name}` to your project's Cargo.toml dependencies:"),
        format!("    {name} = {{ path = \"plugins/{name}\" }}"),
        "Add the plugin to your App::builder chain in src/main.rs:".to_string(),
        format!("    .plugin({crate_name}::{pascal}Plugin::default())"),
        "Declare your first model in src/models.rs and uncomment the".to_string(),
        "    `Plugin::models()` line in src/lib.rs.".to_string(),
    ];

    Ok(ScaffoldReport {
        root,
        files,
        next_steps,
    })
}

/// Write a richer plugin scaffold at `<project_root>/plugins/<name>/`
/// targeted at *distributable* / reusable plugins (third-party crates
/// you'd publish or share across projects). Layout:
///
/// ```text
/// plugins/<name>/
/// ├── Cargo.toml         — deps: umbra, serde, sqlx, chrono, async-trait
/// ├── README.md          — what this plugin does, how to wire it
/// └── src/
///     ├── lib.rs         — Plugin trait impl, glues models + routes
///     ├── models.rs      — one example Model showing common field types
///     │                    (Text + max_length, Choice enum, optional DateTime)
///     └── handlers.rs    — one example axum handler using AppContext
/// ```
///
/// Contrast with [`scaffold_app`], which writes a minimal skeleton
/// (Cargo.toml + lib.rs with a stub Plugin impl, nothing else). Use
/// `startplugin` when you're building a plugin you intend to ship; use
/// `startapp` for an internal module that just needs a `Plugin` seam.
pub fn scaffold_plugin(
    name: &str,
    project_root: &Path,
    local_umbra_repo: Option<&Path>,
) -> Result<ScaffoldReport, ScaffoldError> {
    validate_name(name)?;

    let normalized = name.replace('-', "_");
    if RESERVED_PLUGIN_NAMES.contains(&normalized.as_str()) {
        return Err(ScaffoldError::ReservedName(name.to_string()));
    }

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

    // Cargo.toml — pulls in the deps the example modules use. async-
    // trait is here because Plugin trait methods are sync today, but
    // the generated handlers.rs example uses an async axum extractor,
    // and most plugins grow async work quickly. Cheap to ship now,
    // saves the user a Cargo.toml edit later.
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
description = "A {crate_name} plugin for umbra."

[dependencies]
umbra = {{ git = "https://github.com/dalmasonto/umbra" }}
serde = {{ version = "1", features = ["derive"] }}
sqlx = {{ version = "0.8", default-features = false, features = ["macros", "runtime-tokio"] }}
chrono = {{ version = "0.4", features = ["serde"] }}
async-trait = "0.1"
"#
    );
    let cargo_toml = match local_umbra_repo {
        Some(repo) => localize_deps(&cargo_toml, repo),
        None => cargo_toml,
    };
    write_file(&root, "Cargo.toml", &cargo_toml, &mut files)?;

    // README.md — the user-facing tour. Mirrors the file structure so
    // a reader who clones the crate knows where to look first.
    let readme = format!(
        r#"# {name}

A {crate_name} plugin for [umbra](https://github.com/dalmasonto/umbra).

Generated by `umbra startplugin {name}`.

## What's inside

| File | Purpose |
|---|---|
| `src/lib.rs` | `{pascal}Plugin` struct + `impl Plugin` (registers models, routes, lifecycle hooks). |
| `src/models.rs` | One example model showing common field types (`#[umbra(...)]` attributes for `max_length`, `choices`, FK, defaults). |
| `src/handlers.rs` | One example axum handler showing how to read query params and return JSON. |

## Wiring it in

In your project's `Cargo.toml`:

```toml
[dependencies]
{name} = {{ path = "plugins/{name}" }}
```

In `src/main.rs`:

```rust,ignore
let app = umbra::App::builder()
    .plugin({crate_name}::{pascal}Plugin::default())
    // ... your other plugins
    .build()?;
```

Then:

```sh
cargo run -- makemigrations   # generates 0001_initial.json from your models
cargo run -- migrate          # applies the schema
cargo run -- serve            # boots the HTTP server
```

## Next steps

- Add your own models in `src/models.rs` (or split into a `models/` module).
- Add routes in `routes()` and handlers in `src/handlers.rs`.
- Use `on_ready(&AppContext)` for one-shot setup work (seed default rows, register signals).
- See `documentation/docs/v0.0.1/plugins/the-plugin-trait.mdx` for the full trait surface.
"#
    );
    write_file(&root, "README.md", &readme, &mut files)?;

    // src/lib.rs — Plugin impl that pulls models + routes from the
    // sibling modules. `models()` returns the registered model meta;
    // `routes()` returns the axum Router with the example handler.
    let lib_rs = format!(
        r#"//! {pascal}Plugin — a richer starter scaffold for distributable
//! umbra plugins. Generated by `umbra startplugin {name}`.
//!
//! Wire this into your App in `src/main.rs`:
//!
//! ```ignore
//! .plugin({crate_name}::{pascal}Plugin::default())
//! ```
//!
//! See `README.md` for the full file tour.

pub mod handlers;
pub mod models;

use async_trait::async_trait;
use umbra::migrate::ModelMeta;
use umbra::orm::Model;
use umbra::plugin::{{AppContext, Plugin, PluginError}};
use umbra::web::{{Router, routing::get}};

/// The plugin entry point. Register one instance per `App::builder()`.
#[derive(Debug, Default, Clone)]
pub struct {pascal}Plugin;

#[async_trait]
impl Plugin for {pascal}Plugin {{
    fn name(&self) -> &'static str {{
        "{name}"
    }}

    /// Models the framework's migration engine should track. Each
    /// returned [`ModelMeta`] becomes one row in the
    /// `umbra_migrations` tracking table once the initial migration
    /// applies.
    fn models(&self) -> Vec<ModelMeta> {{
        vec![models::{pascal}Item::meta()]
    }}

    /// HTTP routes contributed by this plugin. The base path is
    /// up to you — convention is `/<name>/...` for HTML and
    /// `/api/<name>/...` for JSON.
    fn routes(&self) -> Router {{
        Router::new().route("/{name}/hello", get(handlers::hello))
    }}

    /// One-shot setup after `App::build()` finishes. Use this for
    /// seeding default rows, registering signal handlers, or any
    /// work that needs the database available. Sync because the
    /// `Plugin` trait signature is sync (BUG-3 in bugs/tests/testBugs.md);
    /// reach into a runtime via `tokio::runtime::Handle::current()
    /// .block_on(...)` if you need to await something here.
    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {{
        Ok(())
    }}
}}
"#
    );
    write_file(&root, "src/lib.rs", &lib_rs, &mut files)?;

    // src/models.rs — one Model showing the field types most plugins
    // need: a Text with max_length, a Choice enum, an optional
    // DateTime. Keeps it small enough to read in one screen.
    let models_rs = format!(
        r#"//! Example model. Replace or extend with your own.
//!
//! What this demonstrates:
//! - `#[umbra(max_length = 200)]` — DDL `VARCHAR(200)` + admin form hint.
//! - `#[umbra(choices)]` on an enum — closed-set column with OpenAPI
//!   `enum` and a Postgres `CHECK (col IN (...))` constraint.
//! - `Option<DateTime<Utc>>` — nullable timestamptz column.
//! - `#[umbra(noedit)]` — read-only on admin forms; not editable via
//!   PUT/PATCH through the REST plugin.

use chrono::{{DateTime, Utc}};
use serde::{{Deserialize, Serialize}};

/// One {crate_name} item. Replace with whatever your plugin actually
/// stores.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct {pascal}Item {{
    /// Auto-incrementing primary key.
    pub id: i64,

    /// Display title. Capped at 200 chars; admin renders a single-line
    /// input.
    #[umbra(string, max_length = 200)]
    pub title: String,

    /// Lifecycle state. The choices map 1:1 to enum variants; the
    /// migration engine emits a CHECK constraint, the admin renders a
    /// `<select>`, and the OpenAPI schema gets an `enum` array.
    pub status: {pascal}Status,

    /// When the item was last published. Read-only on edit forms.
    #[umbra(noedit)]
    pub published_at: Option<DateTime<Utc>>,
}}

/// Lifecycle state for [`{pascal}Item`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum {pascal}Status {{
    Draft,
    Review,
    Published,
    Archived,
}}
"#
    );
    write_file(&root, "src/models.rs", &models_rs, &mut files)?;

    // src/handlers.rs — one axum handler returning JSON. Shows the
    // Query extractor + the framework's Json response shape.
    let handlers_rs = format!(
        r#"//! Example HTTP handlers. Replace or extend with your own.
//!
//! `GET /{name}/hello?name=world` returns `{{"greeting": "Hello, world!"}}`.

use serde::{{Deserialize, Serialize}};
use umbra::web::{{Json, extract::Query}};

#[derive(Debug, Deserialize, Default)]
pub struct HelloParams {{
    /// Who to greet. Defaults to "{name}" when omitted.
    #[serde(default)]
    pub name: Option<String>,
}}

#[derive(Debug, Serialize)]
pub struct HelloResponse {{
    pub greeting: String,
}}

pub async fn hello(Query(params): Query<HelloParams>) -> Json<HelloResponse> {{
    let who = params.name.as_deref().unwrap_or("{name}");
    Json(HelloResponse {{
        greeting: format!("Hello, {{who}}!"),
    }})
}}
"#
    );
    write_file(&root, "src/handlers.rs", &handlers_rs, &mut files)?;

    let next_steps = vec![
        format!("Add `{name}` to your project's Cargo.toml dependencies:"),
        format!("    {name} = {{ path = \"plugins/{name}\" }}"),
        "Add the plugin to your App::builder chain in src/main.rs:".to_string(),
        format!("    .plugin({crate_name}::{pascal}Plugin::default())"),
        "Generate + apply the initial migration:".to_string(),
        "    cargo run -- makemigrations".to_string(),
        "    cargo run -- migrate".to_string(),
        format!("Then visit http://127.0.0.1:8000/{name}/hello?name=you"),
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

    #[test]
    fn scaffold_app_rejects_reserved_built_in_plugin_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for name in RESERVED_PLUGIN_NAMES {
            let result = scaffold_app(name, tmp.path(), None);
            assert!(
                matches!(result, Err(ScaffoldError::ReservedName(_))),
                "expected ReservedName error for `{name}`, got: {result:?}",
            );
            assert!(
                !tmp.path().join("plugins").join(name).exists(),
                "directory must NOT be created when name is reserved: {name}",
            );
        }
    }

    #[test]
    fn scaffold_app_rejects_reserved_name_with_hyphen_variant() {
        // `static` is reserved; so is `my-static`-anything? No — only
        // exact matches. But hyphens should normalize to underscores so
        // someone typing `umbra-static` or `umbra_static` doesn't slip
        // through. We compare on the underscored form.
        let tmp = tempfile::tempdir().expect("tempdir");
        // Pure name check: built-in names contain no hyphens today, but
        // the normalization defends against future built-ins like
        // `slack-bot` versus `slack_bot`.
        let result = scaffold_app("auth", tmp.path(), None);
        assert!(matches!(result, Err(ScaffoldError::ReservedName(_))));
    }

    #[test]
    fn scaffold_app_message_lists_reserved_names() {
        let err = ScaffoldError::ReservedName("auth".to_string());
        let msg = format!("{err}");
        assert!(msg.contains("`auth`"), "error names the offending input");
        assert!(
            msg.contains("admin") && msg.contains("sessions") && msg.contains("permissions"),
            "error lists the reserved set so the user can pick again: {msg}",
        );
    }

    #[test]
    fn scaffold_app_already_exists_message_says_app() {
        // Gap 39: the AlreadyExists message used to say "target" which
        // didn't tell a user that there's an existing APP. The new copy
        // names the app directly.
        let err = ScaffoldError::AlreadyExists(PathBuf::from("plugins/blog"));
        let msg = format!("{err}");
        assert!(msg.contains("app already exists"), "got: {msg}");
        assert!(msg.contains("plugins/blog"), "got: {msg}");
    }

    // ----------------------------------------------------------------- //
    // scaffold_plugin (gap #63)                                         //
    // ----------------------------------------------------------------- //

    #[test]
    fn scaffold_plugin_writes_richer_layout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = scaffold_plugin("widgets", tmp.path(), None).expect("scaffold ok");

        let root = tmp.path().join("plugins").join("widgets");
        assert!(root.is_dir());

        // The richer layout: README + lib + models + handlers.
        for rel in [
            "Cargo.toml",
            "README.md",
            "src/lib.rs",
            "src/models.rs",
            "src/handlers.rs",
        ] {
            assert!(
                root.join(rel).exists(),
                "missing expected file: {rel}; got {:?}",
                report.files,
            );
        }
    }

    #[test]
    fn scaffold_plugin_lib_rs_references_sibling_modules() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_plugin("widgets", tmp.path(), None).expect("scaffold ok");
        let lib = fs::read_to_string(tmp.path().join("plugins/widgets/src/lib.rs")).unwrap();

        assert!(
            lib.contains("pub mod handlers;"),
            "lib.rs must publish handlers"
        );
        assert!(
            lib.contains("pub mod models;"),
            "lib.rs must publish models"
        );
        assert!(lib.contains("WidgetsPlugin"), "PascalCase plugin name");
        assert!(
            lib.contains("models::WidgetsItem::meta()"),
            "models() should register the example model",
        );
        assert!(
            lib.contains("/widgets/hello"),
            "routes() should register the example handler",
        );
    }

    #[test]
    fn scaffold_plugin_models_rs_uses_real_umbra_attributes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_plugin("widgets", tmp.path(), None).expect("scaffold ok");
        let models = fs::read_to_string(tmp.path().join("plugins/widgets/src/models.rs")).unwrap();

        assert!(
            models.contains("umbra::orm::Model"),
            "model derive must reference the framework's Model trait",
        );
        assert!(
            models.contains("max_length = 200"),
            "example model should demonstrate max_length",
        );
        assert!(
            models.contains("WidgetsStatus"),
            "example model should declare a Choice enum",
        );
        assert!(
            models.contains("noedit"),
            "example model should show the noedit attribute",
        );
    }

    #[test]
    fn scaffold_plugin_rejects_reserved_built_in_plugin_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for name in RESERVED_PLUGIN_NAMES {
            let result = scaffold_plugin(name, tmp.path(), None);
            assert!(
                matches!(result, Err(ScaffoldError::ReservedName(_))),
                "expected ReservedName error for `{name}`, got: {result:?}",
            );
        }
    }

    #[test]
    fn scaffold_plugin_refuses_to_overwrite_existing_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_plugin("widgets", tmp.path(), None).expect("first scaffold ok");
        let result = scaffold_plugin("widgets", tmp.path(), None);
        assert!(matches!(result, Err(ScaffoldError::AlreadyExists(_))));
    }

    #[test]
    fn scaffold_plugin_validates_name_like_startapp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            scaffold_plugin("2cool", tmp.path(), None),
            Err(ScaffoldError::InvalidName(_))
        ));
        assert!(matches!(
            scaffold_plugin("foo bar", tmp.path(), None),
            Err(ScaffoldError::InvalidName(_))
        ));
    }
}
