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
    /// The chosen command name is already a framework built-in (`migrate`,
    /// `serve`, …) or a built-in plugin's command (`createsuperuser`, …).
    /// Registering it would shadow the real one at dispatch — the plugin/app
    /// layer is tried before the built-in clap parser — so `migrate` would
    /// stop migrating. Rejected at scaffold time, where the fix is free.
    ReservedCommandName(String),
    /// `startcommand --in <plugin>` named a plugin that isn't under
    /// `plugins/`. Carries the names that ARE there, so the message can
    /// list the real choices instead of just saying no.
    NoSuchPlugin {
        asked: String,
        available: Vec<String>,
    },
    /// `startcommand` needs a project to put the command in, and this
    /// directory has no `src/main.rs` (root) / `src/lib.rs` (plugin).
    NotAProject(PathBuf),
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

/// Commands shipped by a **built-in plugin**. Unlike the framework's own
/// subcommands, these can't be read off a clap parser — they only exist
/// once the plugin is registered on an App, and `startcommand` runs
/// outside any App. So they're listed.
///
/// Adding a command to a built-in plugin? Add its name here, or a user's
/// `startcommand createsuperuser` will scaffold a command that silently
/// shadows the real one.
pub const RESERVED_PLUGIN_COMMAND_NAMES: &[&str] = &[
    "clearsessions",
    "collectstatic",
    "createsuperuser",
    "gen-client",
    "migrate_schemas",
    "tasks-beat",
    "tasks-worker",
];

/// Every command name a new command may not take: the framework's own
/// subcommands plus [`RESERVED_PLUGIN_COMMAND_NAMES`].
///
/// The framework half is read off the derived clap parser rather than
/// hand-listed, so adding a subcommand to `Command` in `lib.rs`
/// automatically reserves its name here. A hand-maintained copy would
/// have drifted the first time someone added one.
///
/// This matters because dispatch tries app/plugin commands *before* the
/// built-in parser (`lib.rs`, step 1 vs step 2). A user command named
/// `migrate` wouldn't collide loudly — it would just quietly take over,
/// and their migrations would stop applying.
pub fn reserved_command_names() -> Vec<String> {
    use clap::CommandFactory;
    let mut names: Vec<String> = <crate::Cli as CommandFactory>::command()
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    names.extend(RESERVED_PLUGIN_COMMAND_NAMES.iter().map(|s| s.to_string()));
    names.push("help".to_string());
    names.sort();
    names.dedup();
    names
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
                "an app already exists at `{}`; move it aside or pick a different name",
                p.display()
            ),
            Self::ReservedName(s) => write!(
                f,
                "`{s}` is the name of a built-in umbral plugin; pick a different name to avoid conflicts at registration time. Reserved names: {}.",
                RESERVED_PLUGIN_NAMES.join(", ")
            ),
            Self::ReservedCommandName(s) => write!(
                f,
                "`{s}` is already an umbral command; pick another name. A command you register \
                 is dispatched BEFORE the built-in of the same name, so this one would shadow \
                 it. Taken names: {}.",
                reserved_command_names().join(", ")
            ),
            Self::NoSuchPlugin { asked, available } => {
                if available.is_empty() {
                    write!(
                        f,
                        "no plugin named `{asked}` — this project has no `plugins/` directory yet. \
                         Create one with `umbral startapp <name>`, or place the command at the \
                         project root with `--in root`."
                    )
                } else {
                    write!(
                        f,
                        "no plugin named `{asked}`. Available: root, {}.",
                        available.join(", ")
                    )
                }
            }
            Self::NotAProject(p) => write!(
                f,
                "`{}` doesn't look like an umbral project — no `src/main.rs`. cd into your \
                 project directory, or pass `--path <dir>`.",
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

/// The generator primitives in `umbral::codegen` fail with their own error;
/// `startcommand` reports through `ScaffoldError` like the rest of this
/// module. The variants line up one-for-one — they were the same errors, which
/// is why `codegen` exists.
impl From<umbral::codegen::CodegenError> for ScaffoldError {
    fn from(e: umbral::codegen::CodegenError) -> Self {
        use umbral::codegen::CodegenError as C;
        match e {
            C::InvalidName(s) => Self::InvalidName(s),
            C::AlreadyExists(p) => Self::AlreadyExists(p),
            C::NoSuchPlugin { asked, available } => Self::NoSuchPlugin { asked, available },
            C::NotAProject(p) => Self::NotAProject(p),
            C::Io(e) => Self::Io(e),
        }
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
/// Where the generated templates point their "Docs" links.
const DOCS_URL: &str = "https://dalmasonto.github.io/umbral/docs/v0.0.1";

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
/// Walk up from `start` looking for an umbral source checkout.
///
/// Identified by `crates/umbral-core/Cargo.toml`, which no consumer project has.
fn find_umbral_checkout(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join("crates/umbral-core/Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

/// Warn when `startproject` is run from inside the umbral repo WITHOUT `--local`.
///
/// The generated `Cargo.toml` pins `env!("CARGO_PKG_VERSION")` — the CLI's own version, which
/// during development is the LAST PUBLISHED release. So a `cargo run -p umbral-cli --
/// startproject foo` from a HEAD checkout writes `umbral = "<last release>"` and then
/// generates code against **main's** API. Any surface added since that release makes the new
/// project fail to compile, and the failure looks like a bug in the framework rather than a
/// version skew.
///
/// It heals itself at release (the scaffold and the libs ship together), so end users of a
/// published CLI never see it. The only person who hits it is a contributor testing their own
/// change — which is exactly the person who most needs `--local`, and exactly the person the
/// silence misleads. gaps3 #65.
fn warn_if_run_from_a_source_checkout(name: &str, parent_dir: &Path) {
    let from_cwd = std::env::current_dir()
        .ok()
        .and_then(|d| find_umbral_checkout(&d));
    let Some(repo) = from_cwd.or_else(|| find_umbral_checkout(parent_dir)) else {
        return;
    };
    let version = env!("CARGO_PKG_VERSION");
    let repo = repo.display();
    eprintln!(
        "warning: running `startproject` from an umbral source checkout ({repo}) without `--local`."
    );
    eprintln!();
    eprintln!(
        "  The new project will depend on the PUBLISHED umbral {version}, while your checkout is on"
    );
    eprintln!(
        "  whatever you have got. Any framework surface you have added since {version} was released"
    );
    eprintln!(
        "  will be missing, and the generated project will fail to compile against it — looking for"
    );
    eprintln!("  all the world like a framework bug rather than a version skew.");
    eprintln!();
    eprintln!("  To build against this checkout instead:");
    eprintln!();
    eprintln!("      umbral startproject {name} --local {repo}");
    eprintln!();
}

pub fn scaffold_project(
    name: &str,
    parent_dir: &Path,
    local_umbral_repo: Option<&Path>,
) -> Result<ScaffoldReport, ScaffoldError> {
    validate_name(name)?;

    if local_umbral_repo.is_none() {
        warn_if_run_from_a_source_checkout(name, parent_dir);
    }

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
# Serves ./static at /static — including the compiled Tailwind bundle this
# project ships. Not optional: the SecurityPlugin's CSP blocks third-party
# script/style CDNs, so an app must serve its own assets.
umbral-storage  = "{version}"

# ----- Available built-ins (uncomment + register in main.rs to enable) -----
# umbral-playground   = "{version}"  # Interactive API playground UI (think mini-Postman) at /playground/.
# umbral-tasks        = "{version}"  # DB-backed background task queue with a worker process.
# umbral-permissions  = "{version}"  # ContentType + Group + Permission model.
# umbral-rls          = "{version}"  # Postgres row-level security policy registration.
# umbral-cache        = "{version}"  # Per-request caching helper.
# umbral-email        = "{version}"  # SMTP + MIME email composer + sender.
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
        r#"//! {name} — application entrypoint.
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
use umbral_storage::StoragePlugin;

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
        // Static files: serves ./static at /static, which is where the compiled
        // Tailwind bundle lives. Use `{{ static('css/app.css') }}` in templates
        // rather than a hardcoded path — in production it resolves through the
        // hashed-asset manifest so you get cache-busting for free.
        //
        // The same plugin also gives you uploaded-file storage (local FS or S3)
        // when you add a FileField / ImageField: `.media("/media", "./media")`.
        .plugin(StoragePlugin::new().static_files("/static", "./static"))
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

Your umbral app.

It starts with one model (`Post`), an admin, a JSON API and an OpenAPI browser, so there
is something running from the first `cargo run`. All of it is ordinary code in this
repository — rename it, gut it, replace it.

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

# Inspect the schema:
cargo run -- showmigrations
cargo run -- makemigrations
```

## Styling

The pages use Tailwind, compiled to `static/css/app.css` and served by the
StoragePlugin at `/static`. That bundle ships **prebuilt**, so this project renders
correctly with no `npm install`.

You only need Node once you edit a template and reach for a utility class that is not
already in the bundle:

```bash
cd styles
npm install
npm run build      # or: npm run watch
```

The palette lives in `styles/input.css` as CSS variables (`--accent` is the violet).
Change them there and every page follows. There is deliberately no `cdn.tailwindcss.com`
script: it is versionless, it pulls a third party into every page load, and it is the
first thing a `default-src 'self'` Content-Security-Policy blocks.

## Where to go next

- Add a plugin: `umbral startapp posts`
- Your first app: {docs}/getting-started/your-first-app
- Models & the ORM: {docs}/orm/models
- Migrations: {docs}/migrations/managed-migrations
- Admin: {docs}/plugins/admin
- REST: {docs}/rest/index
- Login & signup pages: {docs}/auth/login-and-signup-pages
- The Plugin trait: {docs}/plugins/the-plugin-trait
"#,
        docs = DOCS_URL,
    );
    write_file(&root, "README.md", &readme, &mut files)?;

    // ------------------------------------------------------------------ //
    // templates/ + styles/ + static/  — the design system                 //
    //                                                                      //
    // These live as real files under `crates/umbral-cli/assets/scaffold/`  //
    // rather than as string literals, so the templates can be edited (and  //
    // the Tailwind bundle actually COMPILED) like the HTML and CSS they    //
    // are. `__PROJECT__` / `__INITIAL__` / `__DOCS__` are substituted here.//
    // ------------------------------------------------------------------ //
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "U".to_string());
    let fill = |tpl: &str| -> String {
        tpl.replace("__PROJECT__", name)
            .replace("__INITIAL__", &initial)
            .replace("__DOCS__", DOCS_URL)
    };

    for (path, body) in [
        (
            "templates/base.html",
            include_str!("../assets/scaffold/templates/base.html"),
        ),
        (
            "templates/home.html",
            include_str!("../assets/scaffold/templates/home.html"),
        ),
        (
            "templates/dashboard.html",
            include_str!("../assets/scaffold/templates/dashboard.html"),
        ),
        (
            "templates/404.html",
            include_str!("../assets/scaffold/templates/404.html"),
        ),
        (
            "templates/500.html",
            include_str!("../assets/scaffold/templates/500.html"),
        ),
        (
            "styles/input.css",
            include_str!("../assets/scaffold/styles/input.css"),
        ),
        (
            "styles/tailwind.config.js",
            include_str!("../assets/scaffold/styles/tailwind.config.js"),
        ),
        (
            "styles/package.json",
            include_str!("../assets/scaffold/styles/package.json"),
        ),
        // The COMPILED bundle, shipped prebuilt. A brand-new project renders correctly
        // with no npm install — `npm run build` in styles/ is only needed once you edit
        // the templates and use a utility class that isn't already in here.
        (
            "static/css/app.css",
            include_str!("../assets/scaffold/static/css/app.css"),
        ),
    ] {
        write_file(&root, path, &fill(body), &mut files)?;
    }

    let next_steps = vec![
        format!("cd {name}"),
        "cargo run -- migrate  # apply schema migrations".to_string(),
        "cargo run -- serve    # boot the HTTP server on http://127.0.0.1:8000".to_string(),
        "cargo run -- createsuperuser  # create an admin login".to_string(),
        "umbral startapp <name>          # add another app to this project".to_string(),
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
        register_dep_in_cargo_toml(&project_cargo_toml, name).ok()
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
        r#"//! {pascal}Plugin — a distributable umbral plugin.
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
        register_dep_in_cargo_toml(&project_cargo_toml, name).ok()
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

// ===================================================================== //
// startcommand (gaps3 #81)                                              //
// ===================================================================== //

/// Where a scaffolded management command lives: the project's own binary
/// (registered on the App builder via `.commands(commands::all())`) or a
/// plugin under `plugins/<name>/` (returned from its `Plugin::commands()`,
/// so it travels with the plugin).
///
/// This is `umbral::codegen::Target` — the same "root or which plugin?" every
/// generator asks, including the ones plugins ship (`umbral-rest`'s
/// `startpermission` and friends). Two enums saying the same thing is one
/// enum too many.
pub use umbral::codegen::Target as CommandTarget;

/// The marker line the scaffolder inserts new module declarations above.
const MODS_MARKER: &str =
    "// umbral:startcommand — `umbral startcommand` declares new modules above this line.";
/// The marker line the scaffolder inserts new registry entries above.
const REGISTRY_MARKER: &str =
    "// umbral:startcommand — `umbral startcommand` registers new commands above this line.";

/// List the plugins available in this project: every `plugins/<name>/`
/// directory that holds a `Cargo.toml`.
///
/// Reads the disk rather than `main.rs`, so a plugin you scaffolded but
/// haven't registered yet is still offered as a home for a command. Shared
/// with every other generator via `umbral::codegen`.
pub use umbral::codegen::discover_plugins;

/// Write a management command and register it.
///
/// Two targets, one shape. Either way the command lands in a
/// `commands/<name>.rs` next to a `commands/mod.rs` whose `all()` function
/// is the registry, and the registry is wired into the thing that owns it:
///
/// ```text
/// --in root                        --in <plugin>
/// src/                             plugins/<plugin>/src/
///   main.rs   .commands(all())       lib.rs   fn commands() -> all()
///   commands/                        commands/
///     mod.rs  pub fn all()             mod.rs  pub fn all()
///     <name>.rs                        <name>.rs
/// ```
///
/// ## Why a hand-maintained `all()` and not real auto-detection
///
/// Rust has no runtime module reflection: nothing can walk `commands/` at
/// startup and find the structs in it. The choices are a build script that
/// generates the registry, an inventory-style linker-section crate, or a
/// registry function the tool maintains. The registry function wins because
/// it stays *readable and editable by hand* — you can see every command the
/// app has in one place, reorder them, comment one out — and the scaffolder
/// keeps it up to date so the common path costs you nothing. The marker
/// comments are how it finds its insertion points; delete them and the tool
/// falls back to telling you the two lines to add.
///
/// Calling this a second time with a different name appends to the existing
/// `mod.rs` and touches neither `main.rs` nor the plugin's `lib.rs` again.
pub fn scaffold_command(
    name: &str,
    target: &CommandTarget,
    project_root: &Path,
) -> Result<ScaffoldReport, ScaffoldError> {
    validate_name(name)?;

    if reserved_command_names().iter().any(|r| r == name) {
        return Err(ScaffoldError::ReservedCommandName(name.to_string()));
    }

    let module = rust_ident(name);
    let pascal = pascal_case_from_ident(name);
    let struct_name = format!("{pascal}Command");

    // Resolve the crate the command lands in, and the file that owns its
    // registry (main.rs registers via the builder; a plugin via its
    // `Plugin::commands()` impl). `resolve_target` is shared with every other
    // generator, including the ones plugins ship.
    let resolved = umbral::codegen::resolve_target(project_root, target)?;
    let crate_root = resolved.crate_root.clone();
    let owner_file = resolved.owner_file.clone();

    let mut files = Vec::new();

    // ---------------------------------------------------------------- //
    // src/commands/<name>.rs — the command itself. `write_new_file`     //
    // refuses to overwrite, so a re-run can't eat an existing command.  //
    // ---------------------------------------------------------------- //
    umbral::codegen::write_new_file(
        &crate_root,
        &format!("src/commands/{module}.rs"),
        &render_command_file(name, &struct_name, target),
        &mut files,
    )?;

    // ---------------------------------------------------------------- //
    // src/commands/mod.rs — the registry. Created on the first command, //
    // appended to on every one after.                                   //
    // ---------------------------------------------------------------- //
    let mod_rs = crate_root.join("src/commands/mod.rs");
    let mut next_steps: Vec<String> = Vec::new();
    if mod_rs.is_file() {
        let text = fs::read_to_string(&mod_rs)?;
        match append_to_registry(&text, &module, &struct_name) {
            Some(updated) => {
                fs::write(&mod_rs, updated)?;
                files.push(PathBuf::from("src/commands/mod.rs"));
            }
            None => {
                // The markers are gone — the user restructured the file. Say so
                // and hand back the exact two lines rather than guessing where
                // they go and corrupting a file we don't understand.
                next_steps.push(
                    "src/commands/mod.rs has no `umbral:startcommand` markers — add by hand:"
                        .to_string(),
                );
                next_steps.push(format!("    pub mod {module};"));
                next_steps.push(format!(
                    "    ...and inside `all()`:  Box::new({module}::{struct_name}),"
                ));
            }
        }
    } else {
        umbral::codegen::write_new_file(
            &crate_root,
            "src/commands/mod.rs",
            &render_registry_file(&module, &struct_name, target),
            &mut files,
        )?;
    }

    // ---------------------------------------------------------------- //
    // Register the registry with its owner (once — the second command    //
    // reuses the same `all()` call).                                     //
    // ---------------------------------------------------------------- //
    let owner_text = fs::read_to_string(&owner_file)?;
    let wiring = match target {
        CommandTarget::Root => wire_registry_into_main(&owner_text),
        CommandTarget::Plugin(_) => wire_registry_into_plugin(&owner_text),
    };
    match wiring {
        Wiring::Updated { text, steps } => {
            fs::write(&owner_file, text)?;
            next_steps.extend(steps);
        }
        Wiring::AlreadyWired => {}
        Wiring::Manual(steps) => next_steps.extend(steps),
    }

    next_steps.push(format!("Run it:  cargo run -- {name} --help"));

    Ok(ScaffoldReport {
        root: crate_root,
        files,
        next_steps,
        cargo_toml_registered: None,
    })
}

/// Outcome of registering the `commands::all()` registry with the file
/// that owns it (`main.rs` for root, the plugin's `lib.rs` otherwise).
///
/// `Updated` carries leftover manual steps because the two aren't
/// exclusive: we can add the `pub mod commands;` line and still be unable
/// to touch a hand-written `fn commands()` we don't own. Discarding the
/// half that worked to keep the enum tidy would help nobody.
enum Wiring {
    /// The file was edited. `text` is the new content; `steps` is anything
    /// the edit could NOT do and the user must.
    Updated { text: String, steps: Vec<String> },
    /// Already registered — a previous `startcommand` did it. Nothing to do,
    /// which is exactly what makes the second command free.
    AlreadyWired,
    /// The file doesn't match the shape we know how to edit. Rather than
    /// guess, hand the user the lines to paste.
    Manual(Vec<String>),
}

/// Wire `mod commands;` + `.commands(commands::all())` into a project's
/// `main.rs`.
///
/// The builder call is inserted immediately before `.build()` /
/// `.build_deferred()`, which is the one anchor every umbral `main.rs` has
/// — the chain ends there by definition.
fn wire_registry_into_main(text: &str) -> Wiring {
    let already_mod = text.lines().any(|l| l.trim() == "mod commands;");
    let already_registered = text.contains(".commands(commands::all())");
    if already_mod && already_registered {
        return Wiring::AlreadyWired;
    }

    let mut out = text.to_string();

    if !already_mod {
        // Slot it in with the other top-level module declarations so the
        // file's table of contents stays alphabetical-ish and intact.
        // Before the first `mod x;` line, so the table of contents at the top
        // of main.rs stays alphabetical (`commands` sorts before `seed`).
        match out
            .lines()
            .position(|l| l.starts_with("mod ") && l.ends_with(';'))
        {
            Some(idx) => out = insert_line_at_before(&out, idx, "mod commands;"),
            None => {
                return Wiring::Manual(vec![
                    "Add to src/main.rs:".to_string(),
                    "    mod commands;".to_string(),
                    "    ...and in the App::builder() chain:  .commands(commands::all())"
                        .to_string(),
                ]);
            }
        }
    }

    if !already_registered {
        let Some(idx) = out.lines().position(|l| {
            l.trim_start().starts_with(".build_deferred()")
                || l.trim_start().starts_with(".build()")
        }) else {
            return Wiring::Manual(vec![
                "Add to the App::builder() chain in src/main.rs:".to_string(),
                "    .commands(commands::all())".to_string(),
            ]);
        };
        let indent: String = out
            .lines()
            .nth(idx)
            .map(|l| l.chars().take_while(|c| c.is_whitespace()).collect())
            .unwrap_or_default();
        let call = format!(
            "{indent}// Project-owned management commands (`umbral startcommand`).\n\
             {indent}.commands(commands::all())"
        );
        out = insert_line_at_before(&out, idx, &call);
    }

    Wiring::Updated {
        text: out,
        steps: Vec::new(),
    }
}

/// Wire `pub mod commands;` + a `Plugin::commands()` impl into a plugin's
/// `lib.rs`.
///
/// The impl method is inserted at the top of the `impl Plugin for ...`
/// block. If the plugin already has a `fn commands`, we don't touch it —
/// a hand-written one may return more than the registry, and silently
/// rewriting someone's trait impl is exactly the kind of "helpful" edit
/// that eats work.
fn wire_registry_into_plugin(text: &str) -> Wiring {
    let already_mod = text.lines().any(|l| l.trim() == "pub mod commands;");
    let has_commands_fn = text.contains("fn commands(");
    if already_mod && has_commands_fn {
        return Wiring::AlreadyWired;
    }

    let mut out = text.to_string();
    let mut steps: Vec<String> = Vec::new();

    if !already_mod {
        match out
            .lines()
            .position(|l| l.starts_with("pub mod ") && l.ends_with(';'))
        {
            Some(idx) => out = insert_line_at_before(&out, idx, "pub mod commands;"),
            None => steps.push("Add to src/lib.rs:  pub mod commands;".to_string()),
        }
    }

    if !has_commands_fn {
        match out.lines().position(|l| l.starts_with("impl Plugin for ")) {
            Some(idx) => {
                let method = "\n    fn commands(&self) -> Vec<Box<dyn umbral::cli::PluginCommand>> {\n        \
                     // Every command in `src/commands/` — `umbral startcommand`\n        \
                     // appends to the registry in `commands/mod.rs`, so this line\n        \
                     // never needs to change again.\n        \
                     commands::all()\n    }";
                out = insert_line_at(&out, idx, method);
            }
            None => steps.push(
                "Add to your `impl Plugin`:  fn commands(&self) -> Vec<Box<dyn umbral::cli::PluginCommand>> { commands::all() }"
                    .to_string(),
            ),
        }
    } else {
        steps.push(
            "Your plugin already has a `fn commands()` — make sure it returns \
             `commands::all()` (or extends it) so the new command is registered."
                .to_string(),
        );
    }

    if out == text {
        // Nothing we could edit. Everything is a manual step (or, if there are
        // none, it was already wired).
        if steps.is_empty() {
            Wiring::AlreadyWired
        } else {
            Wiring::Manual(steps)
        }
    } else {
        Wiring::Updated { text: out, steps }
    }
}

/// Insert `line` immediately after line index `idx` of `text`.
fn insert_line_at(text: &str, idx: usize, line: &str) -> String {
    let mut out = String::with_capacity(text.len() + line.len() + 1);
    for (i, l) in text.lines().enumerate() {
        out.push_str(l);
        out.push('\n');
        if i == idx {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Append a module declaration + a registry entry to an existing
/// `commands/mod.rs`, using the marker comments as insertion points.
///
/// Returns `None` when a marker is missing — the caller then reports the
/// lines to add by hand rather than guessing at a file it doesn't
/// recognise.
fn append_to_registry(text: &str, module: &str, struct_name: &str) -> Option<String> {
    if text
        .lines()
        .any(|l| l.trim() == format!("pub mod {module};"))
    {
        // Already declared (the command file was deleted but the registry
        // entry survived). Adding a second `pub mod` would not compile.
        return Some(text.to_string());
    }
    let mods_idx = text.lines().position(|l| l.trim() == MODS_MARKER)?;
    let with_mod = insert_line_at_before(text, mods_idx, &format!("pub mod {module};"));

    let reg_idx = with_mod.lines().position(|l| l.trim() == REGISTRY_MARKER)?;
    let entry = format!("        Box::new({module}::{struct_name}),");
    Some(insert_line_at_before(&with_mod, reg_idx, &entry))
}

/// Insert `line` immediately *before* line index `idx` of `text`.
fn insert_line_at_before(text: &str, idx: usize, line: &str) -> String {
    let mut out = String::with_capacity(text.len() + line.len() + 1);
    for (i, l) in text.lines().enumerate() {
        if i == idx {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// The generated `commands/mod.rs` — the registry.
fn render_registry_file(module: &str, struct_name: &str, target: &CommandTarget) -> String {
    let (owner, wiring) = match target {
        CommandTarget::Root => (
            "this project",
            "`main.rs` passes `all()` to `App::builder().commands(...)`.",
        ),
        CommandTarget::Plugin(_) => (
            "this plugin",
            "`lib.rs` returns `all()` from `Plugin::commands()`.",
        ),
    };
    format!(
        r#"//! Management commands owned by {owner} — one file per command,
//! and `all()` is the registry that hands them to the framework.
//!
//! {wiring}
//!
//! Rust can't discover a module by scanning this directory at runtime, so
//! `all()` IS the auto-detection: `umbral startcommand` appends to it for
//! you (that's what the marker comments below are for). You can also edit
//! it by hand — comment a command out and it stops existing, which is
//! harder to do with a magic registry you can't see.

use umbral::cli::PluginCommand;

pub mod {module};
{MODS_MARKER}

/// Every command {owner} registers.
pub fn all() -> Vec<Box<dyn PluginCommand>> {{
    vec![
        Box::new({module}::{struct_name}),
        {REGISTRY_MARKER}
    ]
}}
"#
    )
}

/// The generated `commands/<name>.rs` — one command, showing the three arg
/// shapes clap gives you (positional, named value, flag) and how each is
/// read back out of `ArgMatches`.
fn render_command_file(name: &str, struct_name: &str, target: &CommandTarget) -> String {
    // A plugin's command reaches its own models through `crate::models`;
    // a root command reaches the project's through `crate::`.
    let orm_note = match target {
        CommandTarget::Root => "//     use crate::{Post, post};",
        CommandTarget::Plugin(_) => "//     use crate::models::{Post, post};",
    };
    format!(
        r#"//! `{name}` — a management command.
//!
//! ```bash
//! cargo run -- {name} --help                       # what it takes
//! cargo run -- {name} hello --limit 5 --dry-run    # a real run
//! umbral {name} hello --tag a --tag b              # same thing, via the umbral CLI
//! ```
//!
//! Registered through `commands::all()` in `commands/mod.rs`. It runs against
//! a fully-built app: settings loaded, pool open, every model registered — so
//! the ORM works ambiently here, with no pool to thread through.

use umbral::cli::{{CliError, PluginCommand, clap}};

/// The `{name}` command.
///
/// A unit struct is enough when the command is stateless. It doesn't have to
/// be: the trait is object-safe over `&self`, so anything the command needs
/// configured (a prefix, a client, a channel) can live on the struct and be
/// passed in at registration — which is exactly why this is a trait and not a
/// bare `fn` pointer.
pub struct {struct_name};

#[umbral::async_trait]
impl PluginCommand for {struct_name} {{
    /// Declare the command: its name, its help, and its arguments.
    ///
    /// This is plain `clap`, so everything clap can do is available here —
    /// value parsing and validation, defaults, conflicts, subcommands of your
    /// own. Note the import: `umbral::cli::clap`, the framework's own clap.
    /// Add `clap` to your Cargo.toml separately and a major-version bump on
    /// either side turns into a type mismatch a page long.
    fn command(&self) -> clap::Command {{
        clap::Command::new("{name}")
            // Shown next to the command in `umbral help`. Write it — a command
            // with no `about` lists as a dash and nobody discovers it.
            .about("TODO: one line on what {name} does")
            .long_about(
                "TODO: the longer story, shown on `{name} --help`. What it \
                 changes, whether it's safe to re-run, what it needs first.",
            )
            // POSITIONAL argument — `{name} <slug>`. Required, so clap
            // rejects the call with a usage error if it's missing and `run`
            // never sees a half-formed invocation.
            .arg(
                clap::Arg::new("slug")
                    .required(true)
                    .help("The thing to operate on"),
            )
            // NAMED argument with a value and a default — `--limit 25` / `-l 25`.
            // `value_parser` is what makes it a `u64` on the other side rather
            // than a string you'd have to parse (and mis-parse) yourself.
            .arg(
                clap::Arg::new("limit")
                    .long("limit")
                    .short('l')
                    .value_name("N")
                    .value_parser(clap::value_parser!(u64))
                    .default_value("25")
                    .help("How many rows to touch at most"),
            )
            // REPEATABLE named argument — `--tag a --tag b` collects both.
            // `ArgAction::Append` is the difference between the second `--tag`
            // overwriting the first and the two accumulating.
            .arg(
                clap::Arg::new("tag")
                    .long("tag")
                    .value_name("TAG")
                    .action(clap::ArgAction::Append)
                    .help("Filter by tag. Repeat for more than one."),
            )
            // BOOLEAN flag — `--dry-run`, no value. `SetTrue` is what makes it
            // a flag rather than an option that demands a value.
            .arg(
                clap::Arg::new("dry-run")
                    .long("dry-run")
                    .action(clap::ArgAction::SetTrue)
                    .help("Report what would change without writing anything"),
            )
    }}

    /// Run the command. `matches` is this subcommand's own `ArgMatches` —
    /// clap has already validated it against `command()` above, so every
    /// `get_one` here is reading a value that exists and typechecked.
    async fn run(&self, matches: &clap::ArgMatches) -> Result<(), CliError> {{
        let slug = matches
            .get_one::<String>("slug")
            .expect("clap enforces `required(true)`");
        let limit = *matches
            .get_one::<u64>("limit")
            .expect("clap fills in `default_value`");
        let tags: Vec<&String> = matches
            .get_many::<String>("tag")
            .map(Iterator::collect)
            .unwrap_or_default();
        let dry_run = matches.get_flag("dry-run");

        println!("{name}: slug={{slug}} limit={{limit}} tags={{tags:?}} dry_run={{dry_run}}");

        // The app is already built by the time this runs, so the ORM is live:
        //
        {orm_note}
        //
        //     let posts = Post::objects()
        //         .filter(post::PUBLISHED.eq(true))
        //         .limit(limit as i64)
        //         .fetch()
        //         .await?;
        //
        //     if dry_run {{
        //         println!("would touch {{}} post(s)", posts.len());
        //         return Ok(());
        //     }}
        //
        // `?` just works: `CliError` is a boxed error, so every umbral error
        // converts into it. Return `Err(...)` and the process exits non-zero,
        // which is what a CI step or a cron job is watching for.

        Ok(())
    }}
}}
"#
    )
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

    // ----------------------------------------------------------------- //
    // startcommand (gaps3 #81)                                           //
    // ----------------------------------------------------------------- //

    /// A real scaffolded project to run `startcommand` against — the same
    /// `main.rs` a user gets from `umbral startproject`, so the wiring
    /// surgery is exercised against the file it actually has to edit, not a
    /// fixture written to make the test pass.
    fn project(tmp: &tempfile::TempDir) -> PathBuf {
        scaffold_project("demo", tmp.path(), None).expect("scaffold_project");
        tmp.path().join("demo")
    }

    fn read(root: &Path, rel: &str) -> String {
        fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }

    #[test]
    fn startcommand_root_writes_the_command_and_wires_main() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);

        let report =
            scaffold_command("backfill_slugs", &CommandTarget::Root, &root).expect("scaffold");
        assert!(
            report
                .files
                .contains(&PathBuf::from("src/commands/backfill_slugs.rs"))
        );
        assert!(report.files.contains(&PathBuf::from("src/commands/mod.rs")));

        // The command file: right struct, right trait, framework's clap.
        let cmd = read(&root, "src/commands/backfill_slugs.rs");
        assert!(cmd.contains("pub struct BackfillSlugsCommand;"), "{cmd}");
        assert!(
            cmd.contains("impl PluginCommand for BackfillSlugsCommand"),
            "{cmd}"
        );
        assert!(
            cmd.contains("use umbral::cli::{CliError, PluginCommand, clap};"),
            "the generated file must import the framework's clap, not its own: {cmd}"
        );
        assert!(
            cmd.contains(r#"clap::Command::new("backfill_slugs")"#),
            "{cmd}"
        );

        // The registry.
        let registry = read(&root, "src/commands/mod.rs");
        assert!(registry.contains("pub mod backfill_slugs;"), "{registry}");
        assert!(
            registry.contains("Box::new(backfill_slugs::BackfillSlugsCommand),"),
            "{registry}"
        );

        // The wiring: main.rs declares the module AND registers the registry.
        let main_rs = read(&root, "src/main.rs");
        assert!(
            main_rs.contains("mod commands;"),
            "main.rs never declared the module: {main_rs}"
        );
        assert!(
            main_rs.contains(".commands(commands::all())"),
            "main.rs never registered the command registry: {main_rs}"
        );
        // ...and it goes INSIDE the builder chain, before the terminal build.
        let reg = main_rs.find(".commands(commands::all())").unwrap();
        let build = main_rs.find(".build_deferred()").unwrap();
        assert!(
            reg < build,
            "`.commands(...)` landed after `.build_deferred()`, which doesn't compile"
        );
    }

    /// The whole reason `all()` exists: the SECOND command is free. It
    /// appends to the registry and touches `main.rs` exactly zero more
    /// times — no duplicate `mod commands;`, no second `.commands(...)`
    /// call (which wouldn't compile as a duplicate... it would silently
    /// register the same list twice).
    #[test]
    fn startcommand_second_command_appends_and_leaves_main_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);

        scaffold_command("backfill_slugs", &CommandTarget::Root, &root).expect("first");
        let main_after_first = read(&root, "src/main.rs");
        scaffold_command("import-prices", &CommandTarget::Root, &root).expect("second");
        let main_after_second = read(&root, "src/main.rs");

        assert_eq!(
            main_after_first, main_after_second,
            "the second startcommand edited main.rs again"
        );

        let registry = read(&root, "src/commands/mod.rs");
        assert!(registry.contains("pub mod backfill_slugs;"), "{registry}");
        // A hyphenated command name becomes a snake_case module and a
        // PascalCase struct, while the CLI name keeps its hyphen.
        assert!(registry.contains("pub mod import_prices;"), "{registry}");
        assert!(
            registry.contains("Box::new(import_prices::ImportPricesCommand),"),
            "{registry}"
        );
        let cmd = read(&root, "src/commands/import_prices.rs");
        assert!(
            cmd.contains(r#"clap::Command::new("import-prices")"#),
            "the clap name should be what the user typed, hyphens and all: {cmd}"
        );

        assert_eq!(
            main_after_second
                .matches(".commands(commands::all())")
                .count(),
            1,
            "main.rs registered the registry twice"
        );
        assert_eq!(
            main_after_second.matches("\nmod commands;").count(),
            1,
            "main.rs declared `mod commands;` twice"
        );
    }

    #[test]
    fn startcommand_plugin_writes_the_command_and_wires_the_plugin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        scaffold_app("blog", &root, None).expect("scaffold_app");

        scaffold_command("reindex", &CommandTarget::Plugin("blog".to_string()), &root)
            .expect("scaffold");

        let plugin_root = root.join("plugins/blog");
        let registry = read(&plugin_root, "src/commands/mod.rs");
        assert!(registry.contains("pub mod reindex;"), "{registry}");
        assert!(
            registry.contains("Box::new(reindex::ReindexCommand),"),
            "{registry}"
        );

        let lib_rs = read(&plugin_root, "src/lib.rs");
        assert!(lib_rs.contains("pub mod commands;"), "{lib_rs}");
        assert!(
            lib_rs.contains("fn commands(&self) -> Vec<Box<dyn umbral::cli::PluginCommand>>"),
            "the plugin never got a `Plugin::commands()` impl: {lib_rs}"
        );
        assert!(
            lib_rs.contains("commands::all()"),
            "the impl doesn't return the registry: {lib_rs}"
        );
        // The method has to land INSIDE the impl block, not after it.
        let impl_start = lib_rs.find("impl Plugin for BlogPlugin {").unwrap();
        let method = lib_rs.find("fn commands(&self)").unwrap();
        assert!(method > impl_start, "the method landed outside the impl");
    }

    /// A command name that's already a framework built-in would SHADOW it:
    /// dispatch tries app/plugin commands before the built-in clap parser.
    /// `migrate` would stop migrating, silently. Reject it where the fix is
    /// free.
    #[test]
    fn startcommand_rejects_a_builtin_command_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        for taken in ["migrate", "serve", "makemigrations", "dev"] {
            assert!(
                matches!(
                    scaffold_command(taken, &CommandTarget::Root, &root),
                    Err(ScaffoldError::ReservedCommandName(_))
                ),
                "`{taken}` is a built-in and must be rejected"
            );
        }
    }

    /// Same shadowing hazard, but for a command a built-in *plugin* ships.
    /// These can't be read off a clap parser (they only exist on a built
    /// App), so they're listed — and the list has to be honoured.
    #[test]
    fn startcommand_rejects_a_builtin_plugin_command_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        assert!(matches!(
            scaffold_command("createsuperuser", &CommandTarget::Root, &root),
            Err(ScaffoldError::ReservedCommandName(_))
        ));
        assert!(matches!(
            scaffold_command("tasks-worker", &CommandTarget::Root, &root),
            Err(ScaffoldError::ReservedCommandName(_))
        ));
    }

    /// The reserved set is derived from the clap parser, so a subcommand
    /// added to `Command` in lib.rs reserves its own name with no second
    /// list to remember to update.
    #[test]
    fn reserved_command_names_are_read_off_the_real_parser() {
        let names = reserved_command_names();
        for expected in ["migrate", "serve", "typegen", "squashmigrations", "help"] {
            assert!(
                names.iter().any(|n| n == expected),
                "`{expected}` missing from the reserved set: {names:?}"
            );
        }
    }

    #[test]
    fn startcommand_rejects_an_unknown_plugin_and_lists_the_real_ones() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        scaffold_app("blog", &root, None).expect("scaffold_app");

        let err = scaffold_command("reindex", &CommandTarget::Plugin("blgo".into()), &root)
            .expect_err("a typo'd plugin name must not scaffold anything");
        match err {
            ScaffoldError::NoSuchPlugin { asked, available } => {
                assert_eq!(asked, "blgo");
                assert_eq!(available, vec!["blog".to_string()]);
            }
            other => panic!("expected NoSuchPlugin, got {other:?}"),
        }
    }

    #[test]
    fn startcommand_refuses_to_overwrite_an_existing_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        scaffold_command("reindex", &CommandTarget::Root, &root).expect("first");
        assert!(matches!(
            scaffold_command("reindex", &CommandTarget::Root, &root),
            Err(ScaffoldError::AlreadyExists(_))
        ));
    }

    #[test]
    fn startcommand_outside_a_project_says_so() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            scaffold_command("reindex", &CommandTarget::Root, tmp.path()),
            Err(ScaffoldError::NotAProject(_))
        ));
    }

    #[test]
    fn discover_plugins_lists_plugin_crates_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        // A fresh project has an empty `plugins/` (a .gitkeep + README, no crates).
        assert!(discover_plugins(&root).is_empty());

        scaffold_app("blog", &root, None).expect("scaffold_app");
        scaffold_app("shop", &root, None).expect("scaffold_app");
        // A stray directory with no Cargo.toml isn't a plugin and must not be
        // offered as a home for a command.
        fs::create_dir_all(root.join("plugins/notacrate")).unwrap();

        assert_eq!(
            discover_plugins(&root),
            vec!["blog".to_string(), "shop".to_string()]
        );
    }

    /// When a user has restructured `commands/mod.rs` past recognition, the
    /// tool must not "helpfully" rewrite a file it doesn't understand. It
    /// writes the command and hands back the two lines to add.
    #[test]
    fn startcommand_reports_manual_steps_when_the_registry_markers_are_gone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = project(&tmp);
        scaffold_command("first", &CommandTarget::Root, &root).expect("first");

        let mod_rs = root.join("src/commands/mod.rs");
        let mangled = read(&root, "src/commands/mod.rs")
            .lines()
            .filter(|l| !l.trim().starts_with("// umbral:startcommand"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&mod_rs, &mangled).unwrap();

        let report = scaffold_command("second", &CommandTarget::Root, &root).expect("second");

        // The registry was NOT touched...
        assert_eq!(read(&root, "src/commands/mod.rs"), mangled);
        // ...and the user was told exactly what to add.
        let steps = report.next_steps.join("\n");
        assert!(steps.contains("pub mod second;"), "{steps}");
        assert!(steps.contains("Box::new(second::SecondCommand)"), "{steps}");
    }
}
