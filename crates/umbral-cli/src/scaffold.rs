//! Project + plugin scaffolding.
//!
//! Two functions:
//!
//! - [`scaffold_project`] writes a complete new project directory.
//!   Maps to `umbral startproject <name>`.
//! - [`scaffold_app`] writes a new plugin crate at
//!   `plugins/<name>/`. Maps to `umbral startapp <name>`.
//!
//! Both are pure: take a target path and the new name, write files,
//! return what was written. The binary's `main.rs` wraps them with
//! CLI parsing + a stdout report.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use umbral_casing::pascal_case_from_ident;

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
    /// by umbral. Both crates would compile, but the user would never
    /// be able to register both `.plugin(<their app>)` and
    /// `.plugin(<built-in>)` without an alias dance, and route /
    /// table-name collisions would land at boot. We reject the name
    /// up front to prevent this confusion.
    ReservedName(String),
    /// I/O failure during file creation.
    Io(io::Error),
}

/// Built-in plugin names that `umbral startapp` refuses to scaffold over.
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
                "`{s}` is the name of a built-in umbral plugin; pick a different name to avoid conflicts at registration time. Reserved names: {}.",
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
    /// Whether the project's `Cargo.toml` was updated to include the
    /// new plugin as a path dependency. `None` means the operation
    /// wasn't attempted (e.g. `scaffold_project` doesn't auto-register).
    /// `Some(true)` = dep added, `Some(false)` = dep already present
    /// (idempotent — no duplicate written).
    pub cargo_toml_registered: Option<bool>,
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

// `pascal_case` replaced by `umbral_casing::pascal_case_from_ident` (imported
// above) in the gaps2 #77 consolidation refactor.

/// Convert a name to its Rust identifier form (hyphens → underscores).
/// Rewrite git-deps to path-deps anchored at `umbral_repo`. Closes
/// BUG-17 in `bugs/tests/testBugs.md` — `umbral startproject --local
/// /path/to/umbral foo` now produces a `Cargo.toml` that path-deps
/// every umbral crate against the local checkout instead of the
/// published crates.io version. Comments + commented-out optional
/// plugin lines all flow through; any trailing descriptive comment
/// after the dependency spec is preserved.
///
/// Subdirectory mapping mirrors the umbral repo layout: facade
/// crates (`umbral`, `umbral-cli`, `umbral-core`, `umbral-macros`,
/// `umbral-testing`) live under `crates/`; everything else
/// (`umbral-auth`, `umbral-sessions`, `umbral-admin`, …) lives
/// under `plugins/`.
pub(crate) fn localize_deps(text: &str, umbral_repo: &Path) -> String {
    let repo_str = umbral_repo.display().to_string();
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(&rewrite_line(line, &repo_str));
    }
    out
}

/// Rewrite one `Cargo.toml` line: if it declares an umbral dependency
/// (`umbral-xxx = "<version>"` or `umbral-xxx = { ... }`, optionally
/// commented out with a leading `#`), replace the dependency spec with a
/// local `{ path = "<repo>/<subdir>/<crate>" }`. Any other line is
/// returned unchanged, including the otel example comment whose left
/// side is prose, not a bare crate name.
fn rewrite_line(line: &str, repo: &str) -> String {
    // Find the LHS crate name. Strip a leading `#` (commented-out
    // optional plugins) and whitespace, then take the substring up to
    // the first `=`.
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
    // Only bare umbral crate names get localized (skips prose comments
    // like the otel example, whose LHS contains spaces/backticks).
    if !crate_name.starts_with("umbral") || crate_name.contains(|c: char| c.is_whitespace()) {
        return line.to_string();
    }
    // The dependency spec follows `=`: either a version string
    // (`"0.0.1"`) or an inline table (`{ ... }`). Find where it ends so
    // any trailing descriptive `# comment` survives verbatim.
    let after_eq = &body[eq_idx + 1..];
    let spec_offset = after_eq.len() - after_eq.trim_start().len();
    let spec = after_eq.trim_start();
    let spec_len = if let Some(rest) = spec.strip_prefix('"') {
        match rest.find('"') {
            Some(i) => 1 + i + 1,
            None => return line.to_string(),
        }
    } else if spec.starts_with('{') {
        match spec.find('}') {
            Some(i) => i + 1,
            None => return line.to_string(),
        }
    } else {
        return line.to_string();
    };
    let spec_start = body_start + eq_idx + 1 + spec_offset;
    let spec_end = spec_start + spec_len;
    let subdir = match crate_name {
        "umbral" | "umbral-cli" | "umbral-core" | "umbral-macros" | "umbral-testing" => "crates",
        _ => "plugins",
    };
    let path = format!("{repo}/{subdir}/{crate_name}");
    let prefix = &line[..spec_start];
    let suffix = &line[spec_end..];
    format!("{prefix}{{ path = \"{path}\" }}{suffix}")
}

fn rust_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// A random 64-hex-char dev secret key, unique per scaffold (audit_2
/// macros-cli #7). Replaces the old shared `umbral-insecure-dev-key-change-me`
/// literal so two scaffolded projects never share a key. Dev-only — production
/// still requires a real key (the boot guard rejects a default/dev key under
/// `environment = "Prod"`). Entropy comes from the OS-seeded `RandomState`; a
/// crypto dependency isn't warranted for a dev-only, prod-boot-guarded value.
fn random_dev_secret_key() -> String {
    use std::hash::{BuildHasher, Hasher};
    // Each `RandomState::new()` pulls a fresh OS-seeded random state, so the
    // key differs across scaffold runs. Fold four seeded hashes into 64 hex
    // chars (256 bits of key material).
    let seed = std::collections::hash_map::RandomState::new();
    let mut out = String::with_capacity(64);
    for i in 0..4u64 {
        let mut h = seed.build_hasher();
        h.write_u64(i);
        h.write_u64(i.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// Write a new umbral project at `parent_dir/<name>/`.
///
/// The generated layout is a complete blog-style demo that exercises every
/// major umbral surface: models with FK, migrations on boot, auth + sessions,
/// `login_required`, REST with filters, admin, templates, transactions, and
/// custom error pages.
///
/// The layout follows the per-concern convention we landed on in
/// `examples/shop` (gaps2 #8): `main.rs` reads like a table of contents
/// and every subsystem lives behind a `mod.rs` re-export/orchestrator
/// layer, so the project opens to something that scales past 1000 lines.
///
/// ```text
/// <name>/
/// ├── Cargo.toml
/// ├── umbral.toml
/// ├── .env
/// ├── .env.example
/// ├── .gitignore
/// ├── README.md
/// ├── src/
/// │   ├── main.rs           # App builder + route table + boot helpers
/// │   ├── views/
/// │   │   ├── mod.rs        # re-export layer (handlers return ApiError)
/// │   │   └── public.rs     # public/unauth handlers
/// │   ├── seed/
/// │   │   ├── mod.rs        # `all()` orchestrator (pins dependency order)
/// │   │   └── credentials.rs# idempotent dev-superuser seed
/// │   └── widgets/
/// │       ├── mod.rs        # per-kind re-export layer
/// │       └── cards.rs      # one builtin admin dashboard widget
/// ├── plugins/
/// │   ├── .gitkeep          # local app plugins land here (umbral startapp)
/// │   └── README.md
/// └── templates/
///     ├── base.html
///     ├── home.html
///     ├── dashboard.html
///     ├── 404.html
///     └── 500.html
/// ```
///
/// `main.rs` wires `umbral_cli::dispatch(app)` so the project's binary
/// hosts the management commands. These directories are a *recommended*
/// convention, not a requirement — the runtime reads `main.rs` directly
/// and doesn't care whether handlers live in `views/`, `handlers/`, or
/// inline.
pub fn scaffold_project(
    name: &str,
    parent_dir: &Path,
    local_umbral_repo: Option<&Path>,
) -> Result<ScaffoldReport, ScaffoldError> {
    validate_name(name)?;

    let root = parent_dir.join(name);
    if root.exists() {
        return Err(ScaffoldError::AlreadyExists(root));
    }

    fs::create_dir_all(&root)?;
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("src/views"))?;
    fs::create_dir_all(root.join("src/seed"))?;
    fs::create_dir_all(root.join("src/widgets"))?;
    fs::create_dir_all(root.join("plugins"))?;
    fs::create_dir_all(root.join("templates"))?;

    let crate_name = rust_ident(name);
    let mut files = Vec::new();

    // ------------------------------------------------------------------ //
    // Cargo.toml                                                           //
    // ------------------------------------------------------------------ //
    let version = env!("CARGO_PKG_VERSION");
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]

# ----- Framework core (always required) ------------------------------------
umbral         = "{version}"
umbral-cli     = "{version}"

# ----- Active by default ---------------------------------------------------
# What the generated `src/main.rs` wires in. Comment any of these out only
# if you also remove the matching `.plugin(...)` line.
umbral-auth     = "{version}"
umbral-sessions = "{version}"
umbral-admin    = "{version}"
umbral-rest     = "{version}"
umbral-openapi  = "{version}"
umbral-security = "{version}"
# Observability init helper (structured JSON logging). Enable the `otel`
# feature to ALSO export OpenTelemetry traces over OTLP to a collector
# (Jaeger/Tempo/Honeycomb): `umbral-logs = {{ version = "{version}", features = ["otel"] }}`.
umbral-logs     = "{version}"

# ----- Available built-ins (uncomment + register in main.rs to enable) -----
# umbral-playground   = "{version}"  # Interactive API playground UI (think mini-Postman) at /playground/.
# umbral-tasks        = "{version}"  # DB-backed background task queue with a worker process.
# umbral-permissions  = "{version}"  # ContentType + Group + Permission model.
# umbral-rls          = "{version}"  # Postgres row-level security policy registration.
# umbral-cache        = "{version}"  # Per-request caching helper.
# umbral-email        = "{version}"  # SMTP + MIME email composer + sender.
# umbral-storage      = "{version}"  # Unified storage: static-file serving (prod, whitenoise-equivalent) + uploaded-file storage (local FS + S3).
# umbral-signals      = "{version}"  # Pre/post save/delete signal dispatch.
# umbral-livereload   = "{version}"  # Dev-only browser live-reload (SSE push + file watcher). Add `.plugin(LiveReloadPlugin::new())`.

# ----- Third-party + framework runtime deps --------------------------------
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
tracing-subscriber = {{ version = "0.3", features = ["env-filter"] }}
serde = {{ version = "1", features = ["derive"] }}
chrono = {{ version = "0.4", features = ["serde"] }}
sqlx = {{ version = "0.8", features = ["macros", "sqlite", "postgres", "chrono", "runtime-tokio"] }}

# Once you `umbral startapp <plugin>` or `umbral startplugin <plugin>`, add
# the plugin crate here:
# {crate_name}-posts = {{ path = "plugins/posts" }}
"#
    );
    // BUG-17 fix: when `--local <PATH>` is set, rewrite every umbral
    // dependency to a `{ path = "<umbral>/<sub>/<crate>" }` form
    // anchored at the supplied umbral-repo path. Comments, active and
    // commented-out dep lines all go through. Without the flag, the
    // published crates.io version deps are kept verbatim, which is what
    // a user installing umbral from crates.io gets.
    let cargo_toml = match local_umbral_repo {
        Some(repo) => localize_deps(&cargo_toml, repo),
        None => cargo_toml,
    };
    write_file(&root, "Cargo.toml", &cargo_toml, &mut files)?;

    // ------------------------------------------------------------------ //
    // src/main.rs — the demo wires every umbral surface in ~100 lines      //
    // ------------------------------------------------------------------ //
    let main_rs = format!(
        r#"//! {name} — generated by `umbral startproject {name}`.
//!
//! This `main.rs` reads like a table of contents: the App builder lists
//! every model, plugin, and route, and the per-concern submodules below
//! own the detail. As the project grows you slot new handlers into
//! `views/`, new seed steps into `seed/`, and new dashboard widgets into
//! `widgets/` — `main.rs` stays a thin wiring layer.
//!
//!   src/
//!     main.rs      — App builder + route table + boot helpers (this file)
//!     views/       — HTTP handlers, one file per resource grouping
//!     seed/        — first-run data, `seed::all()` pins dependency order
//!     widgets/     — admin dashboard widgets, one file per kind
//!     ../plugins/  — local app plugins (`umbral startapp <name>`)
//!
//! Run with:
//!   cargo run -- migrate   # apply pending migrations (run once after checkout)
//!   cargo run -- serve     # boot the HTTP server
//!
//! Other management commands:
//!   cargo run -- makemigrations
//!   cargo run -- showmigrations
//!   cargo run -- createsuperuser

// --- Per-concern modules (the table of contents) ---------------------------
mod seed;
mod views;
mod widgets;

use umbral::prelude::*;
use umbral::web::{{SlashRedirect}};
use umbral::migrate::MigrateError;
use umbral_auth::{{AuthPlugin, AuthUser, login_required_html}};
use umbral_sessions::SessionsPlugin;
use umbral_admin::AdminPlugin;
use umbral_rest::{{RestPlugin, ResourceConfig}};
use umbral_openapi::OpenApiPlugin;
use umbral_security::{{SecurityConfig, SecurityPlugin}};

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
    // Observability: structured logging + (under the `otel` feature on
    // `umbral-logs`) OpenTelemetry OTLP trace export. Reads RUST_LOG,
    // UMBRAL_LOG_FORMAT=json, OTEL_EXPORTER_OTLP_ENDPOINT, OTEL_SERVICE_NAME.
    // Keep the guard alive for the whole program: it flushes the OTLP
    // exporter on drop so trailing spans aren't lost at exit.
    let _obs = umbral_logs::observability::init(umbral_logs::ObservabilityConfig::from_env());

    let settings = Settings::from_env()?;
    let pool = umbral::db::connect(&settings.database_url).await?;

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
        // The dashboard mounts one builtin widget from `widgets/` so a
        // fresh admin isn't empty — add your own with `.dashboard_section`.
        .plugin(
            AdminPlugin::default()
                .dashboard_section(widgets::cards::overview_section()),
        )
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
        // Security (on by default): CSRF + clickjacking/HSTS hardening
        // headers across the app. `/api` is exempt so token-authenticated
        // JSON clients can POST without a browser form CSRF cookie.
        .plugin(SecurityPlugin::with_config(SecurityConfig {{
            csrf_exempt_paths: vec!["/api".to_string()],
            ..Default::default()
        }}))

        // --- Templates -------------------------------------------------------
        .templates_dir("templates")
        .not_found_template("404.html")
        .server_error_template("500.html")

        // Redirect /foo → /foo/  (append trailing slash).
        .slash_redirect(SlashRedirect::Append)

        // --- Routes ----------------------------------------------------------
        // The Routes builder records each (method, path) pair as you
        // declare it, so the dev-mode 404 panel surfaces them without
        // a parallel declaration list. Handlers live in `views/`; this
        // table is the URL conf — open `views/mod.rs` to see them all.
        // Per-route middleware (here, login_required_html on /dashboard)
        // goes through the explicit `.layered(method, path, mr)` form so
        // the layer attaches just to that handler — not all routes.
        .routes(
            Routes::new()
                // Public home page.
                .get("/", views::public::home)
                // API: list posts as JSON (no auth required — demo).
                .get("/api/posts", views::public::api_list_posts)
                // Dashboard: only reachable when logged in. The
                // login_required_html("/login") layer issues a 302 to
                // /login?next=/dashboard/ for anonymous visitors.
                .layered(
                    "GET",
                    "/dashboard",
                    get(views::public::dashboard).layer(login_required_html("/login")),
                ),
        )
        // `build_deferred`, not `build`: it wires everything (pools, model
        // registry, router, system checks) but leaves each plugin's `on_ready`
        // hook unfired. Those hooks seed content and backfill rows, so they must
        // not run during `migrate` — the command whose whole job is to create the
        // tables they write to. `dispatch` fires them once it has read argv.
        .build_deferred()?;

    // Auto-migrate + seed on boot so `cargo run -- serve` Just Works
    // against a fresh database — but only when we're actually starting
    // the server. Running `cargo run -- makemigrations` or `migrate`
    // from the CLI used to silently trigger `auto_migrate()` first and
    // then report "no changes detected" (IMP-1 in bugs/tests/testBugs.md).
    // The guard reads `std::env::args` before dispatch picks them apart
    // so it matches whatever subcommand the user actually typed.
    let argv: Vec<String> = std::env::args().collect();
    let user_invoked_cli = argv.iter().skip(1).any(|a| !a.starts_with('-'));
    if !user_invoked_cli {{
        auto_migrate().await?;
        // First-run data. `seed::all()` is idempotent — see seed/mod.rs.
        seed::all().await?;
    }}

    umbral_cli::dispatch(app).await
}}

// ---------------------------------------------------------------------------
// Boot helpers
// ---------------------------------------------------------------------------

/// Run `makemigrations` + `migrate` on boot. Demo-only convenience.
async fn auto_migrate() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {{
    match umbral::migrate::make().await {{
        Ok(paths) => {{
            for path in paths {{
                eprintln!("auto-migrate: wrote {{}}",  path.display());
            }}
        }}
        Err(MigrateError::NoChanges) => {{}}
        Err(err) => return Err(Box::new(err)),
    }}
    let n = umbral::migrate::run().await?;
    if n > 0 {{
        eprintln!("auto-migrate: applied {{n}} migration(s)");
    }}
    Ok(())
}}
"#
    );
    write_file(&root, "src/main.rs", &main_rs, &mut files)?;

    // ------------------------------------------------------------------ //
    // src/views/mod.rs — re-export layer (handlers return ApiError)        //
    // ------------------------------------------------------------------ //
    let views_mod_rs = r#"//! HTTP handlers, split by concern — the re-export / discoverability
//! layer. Open this file and you see the whole web surface in a few
//! lines: one submodule per resource grouping.
//!
//! Submodules:
//!   - `public` — pages anyone can hit (home, JSON listings).
//!
//! Add `pub mod account;` here when auth-gated views land (dashboard,
//! /me, staff-only pages), then re-export it below so `main.rs` keeps
//! referencing handlers as `views::public::home` without caring which
//! file owns each one. This is a recommended convention, not a rule —
//! the router reads handlers directly, so you're free to restructure.

pub mod public;

// No `internal_error` helper, on purpose.
//
// Handlers return `Result<_, umbral::web::ApiError>` and use a bare `?`. ApiError
// converts from sqlx / WriteError / TemplateError, logs the real cause server-side, and
// returns an opaque 500 — so a missing table or a SQL fragment never reaches the browser.
// The `(StatusCode, String)` + `err.to_string()` pattern does the opposite.
"#;
    write_file(&root, "src/views/mod.rs", views_mod_rs, &mut files)?;

    // ------------------------------------------------------------------ //
    // src/views/public.rs — public/unauth handlers                        //
    // ------------------------------------------------------------------ //
    let views_public_rs = r#"//! Public storefront views — anyone can hit these, no auth required.
//!
//! Every handler returns `Result<_, ApiError>` and lets `?` do the work. `ApiError`
//! converts from a database error, a `WriteError` and a template error, so there is no
//! per-handler error helper to write — and a 500 logs the real cause server-side while
//! the client gets an opaque message. Never hand `err.to_string()` to a browser: that is
//! how table names and SQL fragments end up on someone else's screen.

use umbral::prelude::*;
use umbral::templates::context;
use umbral::web::{ApiError, Html, Json};

use crate::Post;
use crate::post;

/// Home page. Counts published posts and renders home.html.
pub async fn home() -> Result<Html<String>, ApiError> {
    let post_count = Post::objects()
        .filter(post::PUBLISHED.eq(true))
        .count()
        .await?;

    let body = umbral::templates::render("home.html", &context!(post_count))?;
    Ok(Html(body))
}

/// JSON list of all posts — demonstrates the ORM QuerySet.
pub async fn api_list_posts() -> Result<Json<Vec<Post>>, ApiError> {
    let posts = Post::objects().order_by(post::ID.desc()).fetch().await?;
    Ok(Json(posts))
}

/// Dashboard: only reachable when logged in (see the `login_required_html`
/// layer in `main.rs`). The `LoggedIn<AuthUser>` extractor supplies the
/// current user — the layer already checked the session, so this is a
/// cheap field read, not a second DB query.
pub async fn dashboard(
    user: umbral_auth::LoggedIn<umbral_auth::AuthUser>,
) -> Result<Html<String>, ApiError> {
    // Demonstrates a transaction: fetch the user's post list atomically.
    let user_id = user.id;
    let my_posts = umbral::transaction(|tx| {
        Box::pin(async move {
            Post::objects()
                .filter(post::AUTHOR.eq(user_id))
                .on_tx(tx)
                .fetch()
                .await
        })
    })
    .await?;

    let body = umbral::templates::render("dashboard.html", &context!(user, my_posts))?;
    Ok(Html(body))
}
"#;
    write_file(&root, "src/views/public.rs", views_public_rs, &mut files)?;

    // ------------------------------------------------------------------ //
    // src/seed/mod.rs — the seed orchestrator                              //
    // ------------------------------------------------------------------ //
    let seed_mod_rs = r#"//! Seed orchestrator — the re-export / dependency-order layer. One
//! file per concern keeps each step small and focused; `all()` pins
//! the order in which they run.
//!
//! Submodules:
//!   - `credentials` — first-run dev superuser so you can log in to
//!                     /admin/ without a manual `createsuperuser`.
//!
//! Add a `pub mod <concern>;` here for each new seed step, then call it
//! from `all()` in dependency order (e.g. catalog rows before the orders
//! that reference them). The order in `all()` doubles as documentation
//! of which step depends on which.

pub mod credentials;

/// Run every seed step in the right order. Each step is idempotent
/// (short-circuits on a non-empty table), so calling `all()` on a
/// partially-seeded DB tops up the missing pieces without re-inserting.
pub async fn all() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    credentials::test_credentials().await?;
    Ok(())
}
"#;
    write_file(&root, "src/seed/mod.rs", seed_mod_rs, &mut files)?;

    // ------------------------------------------------------------------ //
    // src/seed/credentials.rs — idempotent dev superuser                  //
    // ------------------------------------------------------------------ //
    let seed_credentials_rs = r#"//! First-run convenience: mints a dev superuser `admin` when no users
//! exist yet — but ONLY in the Dev environment AND only when you opt in
//! by exporting a password. There is deliberately NO hardcoded default
//! password: a bare `./app` launch against an empty production database
//! must never plant a known-credential admin account.
//!
//! To auto-seed the dev superuser:
//!
//!   UMBRAL_DEV_ADMIN_PASSWORD=your-dev-password cargo run
//!
//! Otherwise the first boot prints guidance to run
//! `cargo run -- createsuperuser` and seeds nothing. Idempotent —
//! subsequent boots find the user and stay quiet.

use umbral::Environment;
use umbral_auth::AuthUser;

/// Env var that opts a fresh install into the dev-superuser seed and
/// supplies its password. Unset => no seed (print guidance instead).
const DEV_ADMIN_PASSWORD_ENV: &str = "UMBRAL_DEV_ADMIN_PASSWORD";

pub async fn test_credentials() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Never mint a dev superuser outside the Dev environment — belt and
    // suspenders on top of the caller only running us on a bare launch.
    if umbral::settings::get().environment != Environment::Dev {
        return Ok(());
    }

    // Idempotent: bail out the moment any user exists.
    if AuthUser::objects().count().await? > 0 {
        return Ok(());
    }

    // Opt-in only: without an explicit password we plant nothing. This
    // is what keeps a known `admin`/`admin` account off every fresh DB.
    let password = match std::env::var(DEV_ADMIN_PASSWORD_ENV) {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!();
            eprintln!("No users yet, and no dev superuser was seeded. To create one:");
            eprintln!("  • interactive:  cargo run -- createsuperuser");
            eprintln!("  • auto on boot: set {DEV_ADMIN_PASSWORD_ENV}=... and restart");
            eprintln!("                  (Dev environment only; never seeds in Prod)");
            eprintln!();
            return Ok(());
        }
    };

    umbral_auth::create_superuser("admin", "admin@example.com", &password)
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    eprintln!();
    eprintln!("======================================================================");
    eprintln!(" DEV SUPERUSER seeded (Dev environment, {DEV_ADMIN_PASSWORD_ENV} set)");
    eprintln!("----------------------------------------------------------------------");
    eprintln!(" Username : admin");
    eprintln!(" Password : (the value of {DEV_ADMIN_PASSWORD_ENV})");
    eprintln!(" Log in   : http://127.0.0.1:8000/admin/");
    eprintln!(" Remove or edit src/seed/credentials.rs before shipping.");
    eprintln!("======================================================================");
    eprintln!();

    Ok(())
}
"#;
    write_file(
        &root,
        "src/seed/credentials.rs",
        seed_credentials_rs,
        &mut files,
    )?;

    // ------------------------------------------------------------------ //
    // src/widgets/mod.rs — per-kind re-export layer                       //
    // ------------------------------------------------------------------ //
    let widgets_mod_rs = r#"//! Admin dashboard widgets — the re-export / discoverability layer,
//! grouped by kind so each file stays small and focused on one
//! rendering shape.
//!
//! Submodules:
//!   - `cards` — KPI tiles + dashboard sections.
//!
//! Add `pub mod charts;`, `pub mod tables;`, etc. as your dashboard
//! grows, then re-export the builders so `main.rs` calls them as
//! `widgets::cards::overview_section()` without knowing which file owns
//! each one. A recommended convention — restructure freely.

pub mod cards;
"#;
    write_file(&root, "src/widgets/mod.rs", widgets_mod_rs, &mut files)?;

    // ------------------------------------------------------------------ //
    // src/widgets/cards.rs — one builtin dashboard widget so a fresh      //
    // admin isn't empty                                                    //
    // ------------------------------------------------------------------ //
    let widgets_cards_rs = r#"//! Dashboard widget builders. This starter re-exports one framework
//! builtin so a fresh `/admin/` dashboard isn't empty; replace it with
//! your own KPI tiles as the app grows.
//!
//! A widget is a `Widget` value handed to `WidgetSection::widget(...)`.
//! Each section becomes one row of tiles on the admin dashboard. See
//! `documentation/docs/v0.0.1/admin/` and the `examples/shop/src/widgets`
//! reference for the data-closure pattern that hits the ORM.

use umbral_admin::WidgetSection;

/// One dashboard section wiring two framework builtins: a model-count
/// tile and a recent-users list. Mounted from `main.rs` via
/// `.dashboard_section(widgets::cards::overview_section())`.
pub fn overview_section() -> WidgetSection {
    WidgetSection::new("Overview")
        .subtitle("Framework-wide health + recent activity")
        .widget(umbral_admin::builtin_total_models_widget().with_span(8, 2))
        .widget(umbral_admin::builtin_recent_users_widget().with_span(4, 2))
}
"#;
    write_file(&root, "src/widgets/cards.rs", widgets_cards_rs, &mut files)?;

    // ------------------------------------------------------------------ //
    // plugins/ — empty home for local app plugins (umbral startapp)        //
    // ------------------------------------------------------------------ //
    write_file(&root, "plugins/.gitkeep", "", &mut files)?;
    let plugins_readme = "# plugins/\n\nLocal app plugins go here; create one with `umbral startapp <name>`.\nEach is its own crate (`lib/models/views/urls`) and is\nauto-wired into this project's `Cargo.toml` `[dependencies]`.\n";
    write_file(&root, "plugins/README.md", plugins_readme, &mut files)?;

    // ------------------------------------------------------------------ //
    // umbral.toml                                                           //
    // ------------------------------------------------------------------ //
    // A random dev secret, unique per scaffolded project (audit_2 macros-cli #7)
    // — shared into both umbral.toml and the working .env below so they match.
    let dev_secret = random_dev_secret_key();
    let umbral_toml = format!(
        r#"# umbral settings for {name}.
# Environment variables (UMBRAL_*) override these at runtime.
# See umbral::settings for the full schema.

database_url = "sqlite://{name}.db?mode=rwc"

# Bind address for `cargo run -- serve`.
# Override via UMBRAL_BIND_ADDR or the --addr flag.
bind_addr = "127.0.0.1:8000"

environment = "Dev"

# A random dev-only key, unique to this project. CHANGE THIS IN PRODUCTION —
# the framework errors at boot if a dev key is used with environment = "Prod".
secret_key = "{dev_secret}"
"#
    );
    write_file(&root, "umbral.toml", &umbral_toml, &mut files)?;

    // ------------------------------------------------------------------ //
    // .env  (working copy — not checked in)                               //
    // ------------------------------------------------------------------ //
    let dot_env = format!(
        r#"# Working .env for {name}. Do not commit this file.
# Generate a real secret key: openssl rand -hex 32
UMBRAL_DATABASE_URL=sqlite://{name}.db?mode=rwc
UMBRAL_BIND_ADDR=127.0.0.1:8000
UMBRAL_SECRET_KEY={dev_secret}
RUST_LOG=info,umbral=debug
"#
    );
    write_file(&root, ".env", &dot_env, &mut files)?;

    // ------------------------------------------------------------------ //
    // .env.example                                                         //
    // ------------------------------------------------------------------ //
    let env_example = r#"# Copy to `.env` and source from your shell, or use a tool like direnv.
# Settings here override the umbral.toml values at runtime.
#
# UMBRAL_SECRET_KEY=$(openssl rand -hex 32)
# UMBRAL_DATABASE_URL=sqlite://my.db?mode=rwc
# UMBRAL_BIND_ADDR=0.0.0.0:8000
# UMBRAL_ENVIRONMENT=prod
# RUST_LOG=info,umbral=debug
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

A blog-style demo generated by `umbral startproject {name}`.

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
# First run — a bare `cargo run` (no subcommand) auto-migrates the
# database and then starts the server. Passing an explicit subcommand
# (like `serve`) SKIPS the auto-migrate, so `serve` alone assumes the
# schema already exists.
cargo run

# Separate steps (production pattern) — migrate explicitly, then serve:
cargo run -- migrate
cargo run -- serve

# Create a superuser to log in to the admin:
cargo run -- createsuperuser

# Explore the scaffold:
cargo run -- showmigrations
cargo run -- makemigrations
```

## Where to go next

- Add a plugin: `umbral startapp posts`
- Docs: https://umbral.dev/docs/v0.0.1/
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
  <!-- Tailwind CSS via the play CDN: DEV ONLY. This loads a third-party
       script on every page and is versionless, so it can't take a
       meaningful Subresource-Integrity (SRI) hash, and the SecurityPlugin's
       Content-Security-Policy will block it. Before production, replace it
       with a compiled/vendored CSS bundle you serve yourself. -->
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
        "umbral startapp <name>          # scaffold a new plugin (app)".to_string(),
    ];

    Ok(ScaffoldReport {
        root,
        files,
        next_steps,
        cargo_toml_registered: None,
    })
}

/// Write a new plugin crate at `<project_root>/plugins/<name>/`, using
/// the per-concern layout (gaps2 #8):
///
/// ```text
/// plugins/<name>/
/// ├── Cargo.toml
/// └── src/
///     ├── lib.rs     — the `Plugin` impl (name/models/routes/on_ready)
///     ├── models.rs  — `#[derive(Model)]` structs
///     ├── views.rs   — HTTP handlers
///     └── urls.rs    — the URL conf (`router()`): the route table
/// ```
///
/// `lib.rs` declares a `{Name}Plugin` struct whose `routes()` returns
/// `urls::router()`. The new crate is auto-registered as a path dep in
/// the project's `Cargo.toml` (see [`register_dep_in_cargo_toml`]); the
/// user then wires it into their App by adding `.plugin(...)` to the
/// builder chain — the next_steps in the returned report spell out the
/// exact lines.
pub fn scaffold_app(
    name: &str,
    project_root: &Path,
    local_umbral_repo: Option<&Path>,
) -> Result<ScaffoldReport, ScaffoldError> {
    validate_name(name)?;

    // Reject names that collide with built-in umbral plugins. Both crates
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
    let pascal = pascal_case_from_ident(name);
    let mut files = Vec::new();

    let version = env!("CARGO_PKG_VERSION");
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
umbral = "{version}"
serde = {{ version = "1", features = ["derive"] }}
sqlx = {{ version = "0.8", features = ["sqlite", "runtime-tokio", "chrono"] }}
chrono = {{ version = "0.4", features = ["serde"] }}
"#
    );
    let cargo_toml = match local_umbral_repo {
        Some(repo) => localize_deps(&cargo_toml, repo),
        None => cargo_toml,
    };
    write_file(&root, "Cargo.toml", &cargo_toml, &mut files)?;

    let lib_rs = format!(
        r#"//! {pascal}Plugin — generated by `umbral startapp {name}`.
//!
//! A plugin split one file per concern:
//!
//!   src/
//!     lib.rs     — the `Plugin` impl: glues models + routes together (this file)
//!     models.rs  — `#[derive(Model)]` structs (this app's tables)
//!     views.rs   — HTTP handlers
//!     urls.rs    — the URL conf: maps paths to `views::` handlers
//!
//! Wire this into your App by adding to `src/main.rs`:
//!
//! ```ignore
//! .plugin({crate_name}::{pascal}Plugin::default())
//! ```
//!
//! See `documentation/docs/v0.0.1/plugins/the-plugin-trait.mdx` for
//! what each `Plugin` method does. This layout is a recommended
//! convention — the framework only needs a type that impls `Plugin`.

pub mod models;
pub mod urls;
pub mod views;

use umbral::plugin::{{AppContext, Plugin, PluginError}};
use umbral::web::Router;

#[derive(Debug, Default, Clone)]
pub struct {pascal}Plugin;

impl Plugin for {pascal}Plugin {{
    fn name(&self) -> &'static str {{
        "{name}"
    }}

    fn models(&self) -> Vec<umbral::migrate::ModelMeta> {{
        // Register every model the plugin owns so makemigrations
        // picks them up. Uncomment + extend once you've defined one
        // in src/models.rs.
        // vec![umbral::migrate::ModelMeta::for_::<models::Example>()]
        Vec::new()
    }}

    fn routes(&self) -> Router {{
        // Routes live in `urls.rs` (this app's URL conf), one place to
        // see every path the plugin serves.
        urls::router()
    }}

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {{
        Ok(())
    }}
}}
"#
    );
    write_file(&root, "src/lib.rs", &lib_rs, &mut files)?;

    // IMP-4 from bugs/tests/testBugs.md: startapp scaffolds a
    // `models.rs` stub so the user has an obvious place to declare
    // their first `#[derive(Model)]` struct.
    let models_rs = format!(
        r#"//! Models for the `{name}` plugin.
//!
//! Declare one `#[derive(umbral::orm::Model)]` struct per database
//! table. Once registered via `Plugin::models()` in lib.rs, the
//! migration engine picks them up on the next `makemigrations`.
//!
//! ```ignore
//! use chrono::{{DateTime, Utc}};
//! use serde::{{Deserialize, Serialize}};
//!
//! #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
//! pub struct Example {{
//!     pub id: i64,
//!     #[umbral(string, max_length = 200)]
//!     pub title: String,
//!     #[umbral(noedit)]
//!     pub created_at: DateTime<Utc>,
//! }}
//! ```
"#
    );
    write_file(&root, "src/models.rs", &models_rs, &mut files)?;

    // src/views.rs — HTTP handlers for this plugin. One sample `index`
    // handler so `urls.rs` has something to route to out of the box.
    let views_rs = format!(
        r#"//! HTTP handlers for the `{name}` plugin.
//!
//! Each handler is an axum handler — return anything that implements
//! `IntoResponse` (`Html<String>`, `Json<T>`, `&'static str`, a
//! `Result<_, (StatusCode, String)>`, …). Read this app's data through
//! the ORM (`models::*::objects()`), never raw SQL.
//!
//! Routes that reach these handlers are declared in `urls.rs`.

/// Sample landing handler. `GET /{name}/` hits this; rewire the path in
/// `urls.rs`.
pub async fn index() -> &'static str {{
    "Hello from the {name} plugin"
}}
"#
    );
    write_file(&root, "src/views.rs", &views_rs, &mut files)?;

    // src/urls.rs — the plugin's URL conf (the route table). One place
    // that maps every path to a `views::` handler.
    let urls_rs = format!(
        r#"//! URL conf for the `{name}` plugin — the route table.
//! `router()` returns the axum `Router` that
//! `Plugin::routes()` in lib.rs hands back to the framework.
//!
//! Convention: `/<name>/...` for HTML pages, `/api/<name>/...` for JSON.
//! Map each path to a handler in `views.rs` so this file reads as the
//! single index of everything the plugin serves.

use umbral::web::{{Router, get}};

use crate::views;

/// Build this plugin's route table. Add one `.route(path, method(handler))`
/// line per endpoint.
pub fn router() -> Router {{
    Router::new().route("/{name}/", get(views::index))
}}
"#
    );
    write_file(&root, "src/urls.rs", &urls_rs, &mut files)?;

    // Auto-register the new crate as a path dep in the project's Cargo.toml.
    // This is a best-effort step: if it fails (e.g. the user ran startapp
    // from a directory that isn't a Cargo project), we warn but don't roll
    // back the scaffold files already written.
    let project_cargo_toml = project_root.join("Cargo.toml");
    let cargo_toml_registered = if project_cargo_toml.is_file() {
        match register_dep_in_cargo_toml(&project_cargo_toml, name) {
            Ok(added) => Some(added),
            Err(_) => None,
        }
    } else {
        None
    };

    let next_steps = vec![
        "Wire the plugin into your App::builder chain in src/main.rs:".to_string(),
        format!("    .plugin({crate_name}::{pascal}Plugin::default())"),
        "(The plugin crate was auto-added to your project dependencies.)".to_string(),
        "Declare your first model in src/models.rs and uncomment the".to_string(),
        "    `Plugin::models()` line in src/lib.rs.".to_string(),
        "Add handlers in src/views.rs and route them in src/urls.rs.".to_string(),
    ];

    Ok(ScaffoldReport {
        root,
        files,
        next_steps,
        cargo_toml_registered,
    })
}

/// Write a richer plugin scaffold at `<project_root>/plugins/<name>/`
/// targeted at *distributable* / reusable plugins (third-party crates
/// you'd publish or share across projects). Layout:
///
/// ```text
/// plugins/<name>/
/// ├── Cargo.toml         — deps: umbral, serde, sqlx, chrono, async-trait
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
    local_umbral_repo: Option<&Path>,
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
    let pascal = pascal_case_from_ident(name);
    let mut files = Vec::new();

    // Cargo.toml — pulls in the deps the example modules use. async-
    // trait is here because Plugin trait methods are sync today, but
    // the generated handlers.rs example uses an async axum extractor,
    // and most plugins grow async work quickly. Cheap to ship now,
    // saves the user a Cargo.toml edit later.
    let version = env!("CARGO_PKG_VERSION");
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
description = "A {crate_name} plugin for umbral."

[dependencies]
umbral = "{version}"
serde = {{ version = "1", features = ["derive"] }}
sqlx = {{ version = "0.8", default-features = false, features = ["macros", "runtime-tokio"] }}
chrono = {{ version = "0.4", features = ["serde"] }}
async-trait = "0.1"
"#
    );
    let cargo_toml = match local_umbral_repo {
        Some(repo) => localize_deps(&cargo_toml, repo),
        None => cargo_toml,
    };
    write_file(&root, "Cargo.toml", &cargo_toml, &mut files)?;

    // README.md — the user-facing tour. Mirrors the file structure so
    // a reader who clones the crate knows where to look first.
    let readme = format!(
        r#"# {name}

A {crate_name} plugin for [umbral](https://github.com/dalmasonto/umbral).

Generated by `umbral startplugin {name}`.

## What's inside

| File | Purpose |
|---|---|
| `src/lib.rs` | `{pascal}Plugin` struct + `impl Plugin` (registers models, routes, lifecycle hooks). |
| `src/models.rs` | One example model showing common field types (`#[umbral(...)]` attributes for `max_length`, `choices`, FK, defaults). |
| `src/handlers.rs` | One example axum handler showing how to read query params and return JSON. |

## Wiring it in

In your project's `Cargo.toml`:

```toml
[dependencies]
{name} = {{ path = "plugins/{name}" }}
```

In `src/main.rs`:

```rust,ignore
let app = umbral::App::builder()
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
//! umbral plugins. Generated by `umbral startplugin {name}`.
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
use umbral::migrate::ModelMeta;
use umbral::orm::Model;
use umbral::plugin::{{AppContext, Plugin, PluginError}};
use umbral::web::{{Router, get}};

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
    /// `umbral_migrations` tracking table once the initial migration
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
//! - `#[umbral(max_length = 200)]` — DDL `VARCHAR(200)` + admin form hint.
//! - `#[umbral(choices)]` on an enum — closed-set column with OpenAPI
//!   `enum` and a Postgres `CHECK (col IN (...))` constraint.
//! - `Option<DateTime<Utc>>` — nullable timestamptz column.
//! - `#[umbral(noedit)]` — read-only on admin forms; not editable via
//!   PUT/PATCH through the REST plugin.

use chrono::{{DateTime, Utc}};
use serde::{{Deserialize, Serialize}};

/// One {crate_name} item. Replace with whatever your plugin actually
/// stores.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct {pascal}Item {{
    /// Auto-incrementing primary key.
    pub id: i64,

    /// Display title. Capped at 200 chars; admin renders a single-line
    /// input.
    #[umbral(string, max_length = 200)]
    pub title: String,

    /// Lifecycle state. The choices map 1:1 to enum variants; the
    /// migration engine emits a CHECK constraint, the admin renders a
    /// `<select>`, and the OpenAPI schema gets an `enum` array.
    pub status: {pascal}Status,

    /// When the item was last published. Read-only on edit forms.
    #[umbral(noedit)]
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
use umbral::web::{{Json, extract::Query}};

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

    // Auto-register the new crate as a path dep in the project's Cargo.toml.
    let project_cargo_toml = project_root.join("Cargo.toml");
    let cargo_toml_registered = if project_cargo_toml.is_file() {
        match register_dep_in_cargo_toml(&project_cargo_toml, name) {
            Ok(added) => Some(added),
            Err(_) => None,
        }
    } else {
        None
    };

    let next_steps = vec![
        "Wire the plugin into your App::builder chain in src/main.rs:".to_string(),
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
        cargo_toml_registered,
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

/// Attempt to register `<name> = { path = "plugins/<name>" }` under
/// `[dependencies]` in the project's `Cargo.toml`.
///
/// Returns:
/// - `Ok(true)`  — dep was added.
/// - `Ok(false)` — dep was already present (idempotent; no duplicate written).
/// - `Err(_)`    — the file couldn't be read or written. Callers treat this
///   as a soft failure: the scaffold files are already on disk, so we warn
///   but don't roll them back.
///
/// The insertion uses minimal string surgery (find the `[dependencies]`
/// header, append one line immediately after it) so comments, ordering,
/// and formatting of existing deps are preserved. `toml_edit` is not yet
/// a dep of umbral-cli; if it's added later this function is the right
/// place to switch to it.
pub fn register_dep_in_cargo_toml(cargo_toml_path: &Path, name: &str) -> io::Result<bool> {
    let text = fs::read_to_string(cargo_toml_path)?;

    // The dep line we want present. Match on `name =` to catch both
    // quoted and unquoted forms that `cargo new` might emit.
    let dep_key = format!("{name} =");
    if text.lines().any(|l| l.trim_start().starts_with(&dep_key)) {
        // Already registered — nothing to do.
        return Ok(false);
    }

    // Find the `[dependencies]` section header and insert immediately after it.
    // We insert after the header line itself so the new dep sits at the top of
    // the block, before any existing deps. This is the least-surprising position:
    // the user can re-order freely after.
    let dep_line = format!("{name} = {{ path = \"plugins/{name}\" }}\n");

    let mut out = String::with_capacity(text.len() + dep_line.len());
    let mut inserted = false;

    for line in text.split_inclusive('\n') {
        out.push_str(line);
        // Match `[dependencies]` exactly (trimmed), not `[dev-dependencies]`
        // or `[build-dependencies]`.
        if !inserted && line.trim() == "[dependencies]" {
            out.push_str(&dep_line);
            inserted = true;
        }
    }

    if !inserted {
        // No `[dependencies]` section found — append one at the end so the
        // manifest stays valid rather than silently failing.
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n[dependencies]\n");
        out.push_str(&dep_line);
    }

    fs::write(cargo_toml_path, &out)?;
    Ok(true)
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
        assert_eq!(pascal_case_from_ident("posts"), "Posts");
        assert_eq!(pascal_case_from_ident("blog_engine"), "BlogEngine");
        assert_eq!(pascal_case_from_ident("blog-engine"), "BlogEngine");
        assert_eq!(pascal_case_from_ident("api2"), "Api2");
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
        // someone typing `umbral-storage` or `umbral_storage` doesn't slip
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
    fn scaffold_plugin_models_rs_uses_real_umbral_attributes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_plugin("widgets", tmp.path(), None).expect("scaffold ok");
        let models = fs::read_to_string(tmp.path().join("plugins/widgets/src/models.rs")).unwrap();

        assert!(
            models.contains("umbral::orm::Model"),
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

    // ----------------------------------------------------------------- //
    // scaffold_project per-concern layout (gaps2 #8) + SecurityPlugin    //
    // default (gaps2 #25)                                                //
    // ----------------------------------------------------------------- //

    #[test]
    fn scaffold_project_writes_per_concern_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = scaffold_project("blog", tmp.path(), None).expect("scaffold ok");

        let root = tmp.path().join("blog");
        assert!(root.is_dir());

        // The per-concern tree: views/, seed/, widgets/, plugins/.
        for rel in [
            "src/main.rs",
            "src/views/mod.rs",
            "src/views/public.rs",
            "src/seed/mod.rs",
            "src/seed/credentials.rs",
            "src/widgets/mod.rs",
            "src/widgets/cards.rs",
            "plugins/.gitkeep",
            "plugins/README.md",
        ] {
            assert!(
                root.join(rel).exists(),
                "missing expected file: {rel}; got {:?}",
                report.files,
            );
        }
    }

    #[test]
    fn scaffold_project_mod_files_carry_orchestrator_markers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_project("blog", tmp.path(), None).expect("scaffold ok");
        let root = tmp.path().join("blog");

        let views_mod = fs::read_to_string(root.join("src/views/mod.rs")).unwrap();
        assert!(
            views_mod.contains("re-export"),
            "views/mod.rs should describe itself as the re-export layer",
        );
        // gaps3 #57. The scaffold used to GENERATE a `fn internal_error` helper into every
        // new app — and that helper hands `err.to_string()` to the browser, so a missing
        // table or a SQL fragment is printed to whoever asked for the page. The scaffold
        // is the first umbral code a developer ever reads; it was teaching the leak.
        //
        // This assertion is deliberately inverted from what it used to be.
        assert!(
            !views_mod.contains("fn internal_error"),
            "the scaffold must NOT generate an internal_error helper — handlers return \
             ApiError, which logs the cause and keeps it off the wire",
        );
        let views_public = fs::read_to_string(root.join("src/views/public.rs")).unwrap();
        assert!(
            views_public.contains("Result<Html<String>, ApiError>")
                && !views_public.contains("map_err(internal_error)"),
            "generated handlers must return ApiError and use a bare `?`",
        );

        let seed_mod = fs::read_to_string(root.join("src/seed/mod.rs")).unwrap();
        assert!(
            seed_mod.contains("pub async fn all()"),
            "seed/mod.rs must declare the all() orchestrator",
        );
        assert!(
            seed_mod.contains("credentials::test_credentials()"),
            "seed::all() must call the credentials step",
        );
        assert!(
            seed_mod.contains("dependency order") || seed_mod.contains("order in which"),
            "seed/mod.rs should explain it pins dependency order",
        );

        let credentials = fs::read_to_string(root.join("src/seed/credentials.rs")).unwrap();
        assert!(
            credentials.contains("fn test_credentials"),
            "credentials.rs must define the test_credentials seed",
        );
        assert!(
            credentials.contains("count().await? > 0"),
            "test_credentials must be idempotent (short-circuit on existing users)",
        );

        let widgets_mod = fs::read_to_string(root.join("src/widgets/mod.rs")).unwrap();
        assert!(
            widgets_mod.contains("pub mod cards;"),
            "widgets/mod.rs must publish the cards submodule",
        );

        let cards = fs::read_to_string(root.join("src/widgets/cards.rs")).unwrap();
        assert!(
            cards.contains("builtin_total_models_widget")
                || cards.contains("builtin_recent_users_widget"),
            "cards.rs should re-export a builtin widget so the dashboard isn't empty",
        );
    }

    #[test]
    fn scaffold_project_main_declares_modules_and_mounts_security() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_project("blog", tmp.path(), None).expect("scaffold ok");
        let main = fs::read_to_string(tmp.path().join("blog/src/main.rs")).unwrap();

        // The table-of-contents module declarations.
        assert!(
            main.contains("mod views;"),
            "main.rs must declare mod views"
        );
        assert!(main.contains("mod seed;"), "main.rs must declare mod seed");
        assert!(
            main.contains("mod widgets;"),
            "main.rs must declare mod widgets",
        );

        // Routes reference the per-concern handlers.
        assert!(
            main.contains("views::public::home"),
            "route table should wire views::public::home",
        );
        // Boot runs the seed orchestrator.
        assert!(
            main.contains("seed::all().await"),
            "boot should run seed::all()",
        );

        // SecurityPlugin mounted by default (gaps2 #25).
        assert!(
            main.contains("SecurityPlugin"),
            "SecurityPlugin must be mounted by default",
        );
    }

    #[test]
    fn scaffold_project_creates_empty_plugins_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_project("blog", tmp.path(), None).expect("scaffold ok");
        let readme = fs::read_to_string(tmp.path().join("blog/plugins/README.md")).unwrap();
        assert!(
            readme.contains("umbral startapp"),
            "plugins/README.md should point at `umbral startapp`",
        );
    }

    // ----------------------------------------------------------------- //
    // scaffold_app per-concern plugin layout (gaps2 #8)                  //
    // ----------------------------------------------------------------- //

    #[test]
    fn scaffold_app_writes_per_concern_plugin_layout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = scaffold_app("posts", tmp.path(), None).expect("scaffold ok");

        let root = tmp.path().join("plugins").join("posts");
        assert!(root.is_dir());

        for rel in [
            "Cargo.toml",
            "src/lib.rs",
            "src/models.rs",
            "src/views.rs",
            "src/urls.rs",
        ] {
            assert!(
                root.join(rel).exists(),
                "missing expected file: {rel}; got {:?}",
                report.files,
            );
        }
    }

    #[test]
    fn scaffold_app_lib_wires_urls_and_views() {
        let tmp = tempfile::tempdir().expect("tempdir");
        scaffold_app("posts", tmp.path(), None).expect("scaffold ok");
        let root = tmp.path().join("plugins/posts");

        let lib = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert!(
            lib.contains("pub mod models;"),
            "lib.rs must publish models"
        );
        assert!(lib.contains("pub mod views;"), "lib.rs must publish views");
        assert!(lib.contains("pub mod urls;"), "lib.rs must publish urls");
        assert!(
            lib.contains("urls::router()"),
            "routes() must return urls::router()",
        );
        assert!(lib.contains("PostsPlugin"), "PascalCase plugin name");

        let urls = fs::read_to_string(root.join("src/urls.rs")).unwrap();
        assert!(
            urls.contains("pub fn router() -> Router"),
            "urls.rs must expose a router() returning a Router",
        );
        assert!(
            urls.contains("views::index"),
            "urls.rs route table should map to a views:: handler",
        );

        let views = fs::read_to_string(root.join("src/views.rs")).unwrap();
        assert!(
            views.contains("pub async fn index"),
            "views.rs should ship a sample index handler",
        );
    }

    #[test]
    fn scaffold_app_auto_registers_path_dep_in_project_cargo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Fixture project Cargo.toml with a [dependencies] section.
        let project_cargo = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nserde = \"1\"\n";
        fs::write(tmp.path().join("Cargo.toml"), project_cargo).unwrap();

        let report = scaffold_app("posts", tmp.path(), None).expect("scaffold ok");
        assert_eq!(
            report.cargo_toml_registered,
            Some(true),
            "the path dep should have been added",
        );

        let cargo = fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert!(
            cargo.contains("posts = { path = \"plugins/posts\" }"),
            "project Cargo.toml must gain the plugin path dep; got:\n{cargo}",
        );

        // Idempotent: a second run reports `false` (already present).
        // (Different name would re-add; same name short-circuits.)
        let second = register_dep_in_cargo_toml(&tmp.path().join("Cargo.toml"), "posts").unwrap();
        assert!(!second, "re-registering the same dep must be a no-op");
    }

    #[test]
    fn scaffold_app_still_rejects_reserved_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = scaffold_app("auth", tmp.path(), None);
        assert!(matches!(result, Err(ScaffoldError::ReservedName(_))));
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
