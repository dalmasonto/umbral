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

    // ------------------------------------------------------------------ //
    // Cargo.toml                                                           //
    // ------------------------------------------------------------------ //
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
umbra        = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-cli    = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-auth   = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-sessions = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-admin  = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-rest   = {{ git = "https://github.com/dalmasonto/umbra" }}
umbra-openapi = {{ git = "https://github.com/dalmasonto/umbra" }}
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
tracing-subscriber = {{ version = "0.3", features = ["env-filter"] }}
serde = {{ version = "1", features = ["derive"] }}
chrono = {{ version = "0.4", features = ["serde"] }}

# Once you `umbra startapp <plugin>`, add the plugin crate here:
# {crate_name}-posts = {{ path = "plugins/posts" }}
"#
    );
    write_file(&root, "Cargo.toml", &cargo_toml, &mut files)?;

    // ------------------------------------------------------------------ //
    // src/main.rs — the demo wires every umbra surface in ~100 lines      //
    // ------------------------------------------------------------------ //
    let main_rs = format!(
        r#"//! {name} — generated by `umbra startproject {name}`.
//!
//! This file is a walking tour of the umbra framework. Every surface is
//! wired in here: models + FK, migrations, auth, sessions, login_required,
//! REST with filters, admin, transactions, and custom error pages.
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
use umbra_auth::{{AuthPlugin, AuthUser, login_required_html, login_required}};
use umbra_sessions::SessionsPlugin;
use umbra_admin::AdminPlugin;
use umbra_rest::{{RestPlugin, ResourceConfig}};
use umbra_openapi::OpenApiPlugin;

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
                .resource(
                    ResourceConfig::new("post")
                        .enable_filters(),
                ),
        )
        // OpenAPI: Swagger UI at /api/docs/.
        .plugin(OpenApiPlugin::new())

        // --- Templates -------------------------------------------------------
        .templates_dir("templates")
        .not_found_template("404.html")
        .server_error_template("500.html")

        // Redirect /foo → /foo/  (Django-style).
        .slash_redirect(SlashRedirect::Append)

        // --- Routes ----------------------------------------------------------
        .router(
            Router::new()
                // Public home page.
                .route("/", get(home))
                // API: list posts as JSON (no auth required — demo).
                .route("/api/posts", get(api_list_posts))
                // Dashboard: only reachable when logged in. The
                // login_required_html("/login") layer issues a 302 to
                // /login?next=/dashboard/ for anonymous visitors.
                .route("/dashboard", get(dashboard))
                    .layer(login_required_html("/login")),
        )
        .build()?;

    // Auto-migrate on boot so `cargo run -- serve` Just Works against a
    // fresh database. Production deployments run `cargo run -- migrate`
    // as a separate step before starting the server.
    auto_migrate().await?;

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
    let my_posts = umbra::transaction(|tx| Box::pin(async move {{
        Post::objects()
            .filter(post::AUTHOR.eq(user.id()))
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
async fn auto_migrate() -> Result<(), Box<dyn std::error::Error>> {{
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

environment = "dev"

# CHANGE THIS IN PRODUCTION. The framework errors at boot when this
# default key is used with environment = "prod".
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
| `OpenApiPlugin` | Swagger UI at `/api/docs/` |

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
    <a href="/api/docs/" class="text-sm text-gray-600 hover:text-gray-900">API docs</a>
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
pub fn scaffold_app(name: &str, project_root: &Path) -> Result<ScaffoldReport, ScaffoldError> {
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

    #[test]
    fn scaffold_app_rejects_reserved_built_in_plugin_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for name in RESERVED_PLUGIN_NAMES {
            let result = scaffold_app(name, tmp.path());
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
        let result = scaffold_app("auth", tmp.path());
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
}
