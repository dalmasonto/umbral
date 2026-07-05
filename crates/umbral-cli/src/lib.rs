//! Library surface for user binaries to host umbral's management
//! subcommands.
//!
//! umbral-cli ships as two artefacts. The library (this crate) exposes
//! [`dispatch`] — the entry point user binaries call to gain the
//! `serve` / `migrate` / `makemigrations` / `inspectdb` /
//! `dumpdata` / `loaddata` subcommands. The binary (`umbral`) ships as
//! the global scaffolding tool installed via `cargo install
//! umbral-cli`, and handles `startproject` / `startapp` from outside
//! any project.
//!
//! ## Quickstart
//!
//! In your project's `src/main.rs`:
//!
//! ```ignore
//! use umbral::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//!     tracing_subscriber::fmt::init();
//!
//!     let settings = Settings::from_env()?;
//!     let pool = umbral::db::connect(&settings.database_url).await?;
//!
//!     let app = App::builder()
//!         .settings(settings)
//!         .database("default", pool)
//!         .model::<Article>()
//!         .build()?;
//!
//!     umbral_cli::dispatch(app).await
//! }
//! ```
//!
//! Then:
//!
//! ```bash
//! cargo run -- migrate
//! cargo run -- serve
//! cargo run -- makemigrations
//! ```
//!
//! The subcommands run against the published ambient state (pool,
//! model registry) that `App::build` set up, so they see every model
//! and plugin the user wired into the builder.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};
use umbral::App;
use umbral::inspect::{InspectError, InspectOptions};
use umbral::migrate::MigrateError;

pub mod scaffold;

/// Build the `cargo` argv for forwarding a `umbral <cmd> [args...]`
/// invocation to the current project's binary (`cargo run -- <cmd> [args...]`).
///
/// The global `umbral` scaffolding binary forwards every non-scaffolding
/// subcommand here so `umbral dev` behaves as `cargo run -- dev`. The
/// caller runs `cargo` with these args.
pub fn cargo_run_forward_args(forwarded: &[String]) -> Vec<String> {
    let mut argv = vec!["run".to_string(), "--".to_string()];
    argv.extend(forwarded.iter().cloned());
    argv
}

/// Whether `start` (or any ancestor) contains a `Cargo.toml` — i.e. we're
/// inside a Cargo project `cargo run` could build. Mirrors how `cargo`
/// itself finds the manifest by walking up from the working directory, so
/// `umbral <cmd>` works from a subdirectory just like `cargo run` does.
pub fn in_cargo_project(start: &std::path::Path) -> bool {
    start
        .ancestors()
        .any(|dir| dir.join("Cargo.toml").is_file())
}

#[derive(Debug, Parser)]
#[command(
    name = "umbral",
    about = "umbral management commands. Run from your project's binary.",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Boot the HTTP server on `settings.bind_addr`. Default
    /// subcommand when none is given. Override the bind address with
    /// `--addr` or `UMBRAL_BIND_ADDR`.
    Serve {
        /// Override `settings.bind_addr`. Format: `host:port`
        /// (e.g. `127.0.0.1:3000`).
        #[arg(long)]
        addr: Option<String>,
    },
    /// Diff registered models against the latest snapshot and write a
    /// new migration file per plugin with changes.
    Makemigrations {
        /// Write an EMPTY migration for `<plugin>` (current snapshot, no
        /// operations) instead of auto-detecting a schema diff. The stub
        /// for a hand-authored data migration: open the file and add a
        /// `RunSql { sql, reverse_sql }` op. Because it carries no schema
        /// change, it never disturbs the model-snapshot chain.
        #[arg(long, value_name = "PLUGIN")]
        empty: Option<String>,
    },
    /// Apply every pending migration against the ambient pool.
    Migrate {
        /// Mark a specific migration as applied in the tracking table
        /// WITHOUT running its SQL. Recovery path when the schema
        /// already exists (e.g. migrated outside umbral). Format:
        /// `<plugin>/<migration_name>` (e.g. `app/0001_create_post`).
        #[arg(long, value_name = "PLUGIN/NAME")]
        fake: Option<String>,
        /// For each plugin, if the first migration's tables already
        /// exist in the database, mark it applied without running SQL.
        /// Use when adopting a database bootstrapped outside umbral.
        #[arg(long, default_value_t = false)]
        fake_initial: bool,
        /// Proceed even if some applied migrations are missing from
        /// disk. Logs a warning for each missing file and applies the
        /// genuinely-pending ones. Without this flag, `migrate` errors
        /// on drift.
        #[arg(long, default_value_t = false)]
        allow_drift: bool,
    },
    /// List applied vs pending migrations per plugin.
    ///
    /// Markers: [X] applied, [ ] pending, [!] applied-but-missing-on-disk,
    /// [?] on-disk-but-out-of-order.
    Showmigrations,
    /// Classify pending migrations for zero-downtime (blue-green) safety.
    ///
    /// Walks every operation in every pending migration and tags it
    /// SAFE / WARNING / UNSAFE, with an expand-contract note on each
    /// non-safe op. Exits non-zero when any UNSAFE op is found (or any
    /// WARNING under `--strict`), so it drops into a CI gate before deploy.
    /// Read-only — applies nothing.
    Checkmigrations {
        /// Also exit non-zero when a WARNING-tier op is present, not just
        /// UNSAFE. Use in CI when even a column rename must be reviewed.
        #[arg(long, default_value_t = false)]
        strict: bool,
    },
    /// Introspect the ambient database into a `models.rs` plus an
    /// initial migration. Used to onboard an existing schema.
    Inspectdb {
        /// Directory the generated files are written under.
        #[arg(long)]
        output: PathBuf,
        /// Record `0001_initial` in `umbral_migrations` after writing
        /// it, so the next `migrate` is a no-op against the
        /// already-populated database.
        #[arg(long, default_value_t = false)]
        mark_applied: bool,
    },
    /// Dump every registered model's rows to JSON. The upgrade-safety
    /// snapshot.
    Dumpdata {
        /// Where the JSON envelope is written.
        #[arg(long)]
        output: PathBuf,
    },
    /// Load a `dumpdata` JSON envelope into the schema. `migrate`
    /// first so the schema exists.
    Loaddata {
        /// Path to the JSON envelope.
        input: PathBuf,
    },
    /// Import a CSV file into one table's rows. The header row names the
    /// columns; each cell is coerced to its column type and inserted
    /// through the same validated write path as a REST POST (validators,
    /// `auto_now`, `slug_from`, FK-existence all apply). Best-effort: a
    /// bad row is reported by line number and skipped, not fatal. The
    /// inverse of the REST list endpoint's `?format=csv` export.
    Importcsv {
        /// Target table name (e.g. `blog_post`).
        table: String,
        /// Path to the CSV file. Must have a header row.
        input: PathBuf,
    },
    /// Dev-loop runner: watches `src/` and re-runs `cargo run` on
    /// change. Wraps `cargo-watch`; if not installed, prints the
    /// install hint and exits. Templates hot-reload in-process when
    /// `settings.environment == Dev`, so editing an `.html` file
    /// doesn't need a restart at all.
    Dev {
        /// Watch additional paths beyond the default (`src/`,
        /// `Cargo.toml`). Repeatable.
        #[arg(long, short = 'w')]
        watch: Vec<String>,
        /// Pass-through args to `cargo run`. After `--`, e.g.
        /// `umbral dev -- migrate` re-runs `cargo run -- migrate`
        /// on every change.
        #[arg(last = true)]
        run_args: Vec<String>,
    },
    /// Generate a fresh X25519 keypair for `Masked<T>` field encryption
    /// and print the two env-var lines (`UMBRAL_MASK_PUBLIC_KEY` /
    /// `UMBRAL_MASK_PRIVATE_KEY`) needed to configure it.
    Maskkeygen,
}

/// Parse argv and run the requested management subcommand against the
/// passed-in App. The user binary's `main.rs` calls this after
/// building its App — see the module-level docs for the pattern.
///
/// The App must already be built (`App::builder()...build()?`) — the
/// builder phases publish the ambient pool and model registry, which
/// every management command reads. Passing a built `App` instead of
/// an `AppBuilder` keeps the boot order in the user's hands and lets
/// them register plugins / models / databases freely before
/// dispatching.
pub async fn dispatch(app: App) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    dispatch_with_argv(app, argv).await
}

/// Same as [`dispatch`] but argv is passed explicitly instead of read
/// from the process. Lets tests exercise the routing without spawning
/// a subprocess. User code should call [`dispatch`] (which reads
/// `std::env::args_os()` and delegates here).
///
/// The dispatch order is the same as [`dispatch`]: plugin-contributed
/// commands first via [`umbral_core::cli::dispatch`], then the built-in
/// subcommand set (`serve` / `migrate` / etc.).
pub async fn dispatch_with_argv(
    app: App,
    argv: Vec<std::ffi::OsString>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Step 0: intercept the unified-help requests before any per-command
    // clap parser sees argv. `umbral help`, `umbral --help`, and `umbral -h`
    // all print the merged catalog of built-in + plugin commands and exit
    // clean. This is gaps2 #54: the user gets one list of everything they
    // can run, not a per-layer clap help that omits the other layer's
    // commands. (A bare `umbral` keeps its documented serve default.)
    if wants_top_level_help(&argv) {
        print!("{}", render_full_help(&app));
        return Ok(());
    }

    // Step 1: try plugin-contributed subcommands first. Each registered
    // plugin's `commands()` is queried; if argv matches one of them
    // (e.g. `createsuperuser` from `umbral-auth`, `worker` from
    // `umbral-tasks`), that command's `run` fires and we return. If no
    // plugin command matches argv, fall through to the built-in
    // subcommand set below.
    if !app.plugins().is_empty() {
        match umbral_core::cli::dispatch(app.plugins(), argv.clone()).await {
            Ok(umbral_core::cli::DispatchOutcome::Matched(_)) => return Ok(()),
            Ok(umbral_core::cli::DispatchOutcome::Help(msg)) => {
                // A plugin command's --help was requested (e.g.
                // `umbral createsuperuser --help`). That's command-specific
                // help, not the top-level catalog, so print clap's
                // rendered body verbatim and exit clean.
                print!("{msg}");
                return Ok(());
            }
            Ok(umbral_core::cli::DispatchOutcome::Unmatched) => {
                // Fall through to the built-in subcommands.
            }
            Err(e) => return Err(e),
        }
    }

    // Step 2: built-in subcommands. clap parses argv against the fixed
    // `Command` enum. If argv has a token that's neither a built-in
    // subcommand nor a plugin command, clap surfaces a usage error here.
    let cli = match Cli::try_parse_from(&argv) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            match e.kind() {
                // Unknown subcommand / stray arg. The token is neither a
                // plugin command (Step 1 ruled that out) nor a built-in.
                // Print our unified `error: unknown command` + the full
                // catalog so the user sees what IS available, then exit
                // non-zero. Routing through `render_full_help` instead of
                // clap's default keeps plugin commands in the listing.
                ErrorKind::InvalidSubcommand
                | ErrorKind::UnknownArgument
                | ErrorKind::InvalidValue => {
                    let bad = unknown_token(&argv);
                    eprint!("{}", render_unknown(&app, bad.as_deref()));
                    std::process::exit(2);
                }
                _ => {
                    // Genuine clap output (a subcommand's own --help, a
                    // missing-required-arg usage error, --version, …).
                    // Let clap render it as before.
                    e.print()?;
                    std::process::exit(if e.use_stderr() { 2 } else { 0 });
                }
            }
        }
    };
    match cli.command.unwrap_or(Command::Serve { addr: None }) {
        Command::Serve { addr } => serve(app, addr).await,
        Command::Makemigrations { empty } => makemigrations(empty).await,
        Command::Migrate {
            fake,
            fake_initial,
            allow_drift,
        } => migrate(fake, fake_initial, allow_drift).await,
        Command::Showmigrations => showmigrations().await,
        Command::Checkmigrations { strict } => checkmigrations(strict).await,
        Command::Inspectdb {
            output,
            mark_applied,
        } => inspectdb(output, mark_applied).await,
        Command::Dumpdata { output } => dumpdata(output).await,
        Command::Loaddata { input } => loaddata(input).await,
        Command::Importcsv { table, input } => importcsv(table, input).await,
        Command::Dev { watch, run_args } => dev(watch, run_args).await,
        Command::Maskkeygen => maskkeygen(),
    }
}

/// Generate a fresh `Masked<T>` field-encryption keypair and print the
/// two env-var lines. The public key encrypts (every tier that writes
/// masked data needs it); the private key decrypts (`reveal()`) and
/// crypto-shreds on deletion.
fn maskkeygen() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (public, secret) = umbral_core::orm::MaskKeyring::generate();
    println!("# Masked<T> field-encryption keypair — add to your environment / .env:");
    println!("#   UMBRAL_MASK_PUBLIC_KEY encrypts; UMBRAL_MASK_PRIVATE_KEY decrypts (reveal()).");
    println!(
        "#   Keep the PRIVATE key secret. Destroying it crypto-shreds every masked column\n\
         #   (a fast bulk \"right to be forgotten\")."
    );
    println!(
        "#   WARNING: the private key is printed below to STDOUT. Capture it straight into a\n\
         #   secret store (Vault, cloud secret manager, a sealed CI variable) and keep it out\n\
         #   of shell history, terminal scrollback, CI job logs, and any committed .env."
    );
    println!("UMBRAL_MASK_PUBLIC_KEY={public}");
    println!("UMBRAL_MASK_PRIVATE_KEY={secret}");
    Ok(())
}

/// True when argv is asking for the top-level command catalog: the
/// `help` pseudo-subcommand, or a top-level `--help` / `-h`. A `--help`
/// that follows a subcommand (e.g. `migrate --help`) is NOT top-level —
/// that's command-specific help and is left to clap, so we only treat
/// the FIRST post-argv0 token.
///
/// A bare `umbral` (no subcommand) is deliberately NOT intercepted: it
/// keeps its documented default of booting the server (`Serve`), which
/// the example apps rely on via a plain `cargo run`.
fn wants_top_level_help(argv: &[std::ffi::OsString]) -> bool {
    match argv.get(1) {
        None => false,
        Some(first) => first == "help" || first == "--help" || first == "-h",
    }
}

/// The first non-flag token after argv0 — the subcommand the user
/// tried to run. Used to name the offending command in the
/// `error: unknown command \`<x>\`` line.
fn unknown_token(argv: &[std::ffi::OsString]) -> Option<String> {
    argv.iter()
        .skip(1)
        .find(|a| !a.to_string_lossy().starts_with('-'))
        .map(|a| a.to_string_lossy().into_owned())
}

/// Build the merged `(name, about)` catalog: every built-in subcommand
/// (read off the derived clap `Command` via `CommandFactory`) followed
/// by every plugin-contributed command. Built-ins are placed first so
/// they win a name clash in [`umbral_core::cli::render_help`]'s dedup.
fn full_catalog(app: &App) -> Vec<(String, Option<String>)> {
    let mut catalog: Vec<(String, Option<String>)> = Vec::new();
    let root = <Cli as CommandFactory>::command();
    for sub in root.get_subcommands() {
        catalog.push((
            sub.get_name().to_string(),
            sub.get_about().map(|s| s.to_string()),
        ));
    }
    catalog.extend(umbral_core::cli::command_catalog(app.plugins()));
    catalog
}

/// Render the full help screen (built-ins + plugin commands), for
/// `umbral help` / `umbral --help` / bare `umbral`. Prints to stdout.
fn render_full_help(app: &App) -> String {
    umbral_core::cli::render_help(&full_catalog(app))
}

/// Render the unknown-command screen: an `error: unknown command` line
/// (naming the bad token if known) followed by the full catalog so the
/// user sees what they CAN run. Printed to stderr; the caller exits
/// non-zero.
fn render_unknown(app: &App, bad: Option<&str>) -> String {
    let mut s = String::new();
    match bad {
        Some(b) => s.push_str(&format!("error: unknown command `{b}`\n\n")),
        None => s.push_str("error: unknown command\n\n"),
    }
    s.push_str(&render_full_help(app));
    s
}

/// `umbral dev` — wraps `cargo-watch` to re-run `cargo run` on source
/// changes. If `cargo-watch` isn't installed, prints the install hint
/// and exits non-zero so the user notices.
///
/// Template edits don't need this command — they hot-reload in-process
/// when `settings.environment == Dev` (see `umbral-core/src/templates.rs`).
/// `dev` exists for the Rust-source case where the binary needs a
/// rebuild + restart.
async fn dev(
    extra_watches: Vec<String>,
    run_args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Probe for cargo-watch up front so the failure message is clear.
    let probe = std::process::Command::new("cargo")
        .args(["watch", "--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if probe.is_err() || probe.as_ref().map(|s| !s.success()).unwrap_or(true) {
        eprintln!(
            "umbral dev: `cargo-watch` is not installed.\n\n\
             Install with:\n\n\
             \x20\x20\x20\x20cargo install cargo-watch\n\n\
             Then re-run `cargo run -- dev`.\n\n\
             Workaround without cargo-watch: leave one terminal running\n\
             `cargo run` and Ctrl-C + re-run after each edit. Templates\n\
             still hot-reload in dev mode without any restart.",
        );
        std::process::exit(1);
    }

    // Build the cargo-watch invocation. -x runs the given cargo command;
    // -w adds extra watch paths. Default watches are cargo-watch's own
    // (Cargo.toml + src/) so we don't pile -w on every invocation.
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("watch");
    for path in &extra_watches {
        cmd.arg("-w").arg(path);
    }
    let cargo_cmd = if run_args.is_empty() {
        "run".to_string()
    } else {
        format!("run -- {}", run_args.join(" "))
    };
    cmd.arg("-x").arg(&cargo_cmd);

    eprintln!("umbral dev: watching for changes, running `cargo {cargo_cmd}` on each save");
    eprintln!(
        "umbral dev: templates also hot-reload in-process; no restart needed for .html edits"
    );
    eprintln!("umbral dev: Ctrl-C to stop");
    eprintln!();

    let status = cmd.status()?;
    if !status.success() {
        return Err(format!(
            "cargo-watch exited with status {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".to_string())
        )
        .into());
    }
    Ok(())
}

async fn serve(
    app: App,
    addr_override: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr_str = match addr_override {
        Some(s) => s,
        None => umbral_core::settings::get().bind_addr.clone(),
    };
    let addr: SocketAddr = addr_str
        .parse()
        .map_err(|e| format!("umbral: invalid bind_addr `{addr_str}`: {e}"))?;
    app.serve(addr).await?;
    Ok(())
}

async fn makemigrations(
    empty: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // --empty <plugin>: write a no-op migration (current snapshot, empty
    // ops) the developer edits to add a `RunSql` data migration.
    if let Some(plugin) = empty {
        let path = umbral::migrate::make_empty(&plugin).await?;
        println!("Wrote {} (empty)", path.display());
        println!(
            "  Edit it to add a data migration, e.g.:\n  \
             {{ \"kind\": \"RunSql\", \"sql\": \"UPDATE ... SET ...\", \
             \"reverse_sql\": null }}"
        );
        return Ok(());
    }

    match umbral::migrate::make().await {
        Ok(paths) => {
            for path in paths {
                println!("Wrote {}", path.display());
            }
            Ok(())
        }
        Err(MigrateError::NoChanges) => {
            println!("no changes detected");
            Ok(())
        }
        Err(err) => Err(Box::new(err)),
    }
}

async fn migrate(
    fake: Option<String>,
    fake_initial: bool,
    allow_drift: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // --fake <plugin/name>: mark one migration applied without running SQL.
    if let Some(ref spec) = fake {
        let (plugin, name) = parse_migration_spec(spec)?;
        umbral::migrate::fake_apply(plugin, name).await?;
        println!("Marked {spec} as applied (no SQL executed)");
        return Ok(());
    }

    // --fake-initial: for every plugin, if the 0001 tables exist, fake-apply.
    if fake_initial {
        let n = umbral::migrate::fake_initial().await?;
        if n == 0 {
            println!("No plugins needed fake-initial (either already applied or tables absent)");
        } else {
            println!("Fake-applied initial migration for {n} plugin(s)");
        }
        return Ok(());
    }

    // Normal migrate with optional --allow-drift.
    match umbral::migrate::run_checked(allow_drift).await {
        Ok(n) => {
            if n == 0 {
                println!("No pending migrations");
            } else {
                println!("Applied {n} migration(s)");
            }
            Ok(())
        }
        Err(MigrateError::DriftDetected { ref missing }) => {
            let names: Vec<String> = missing.iter().map(|(p, n)| format!("{p}/{n}")).collect();
            eprintln!("error: umbral migrate: drift detected");
            eprintln!("  The following migrations are in the tracking table but missing on disk:");
            for name in &names {
                eprintln!("    [!] {name}");
            }
            eprintln!();
            eprintln!(
                "  Options:\n  \
                 1. Restore the file(s) from VCS.\n  \
                 2. Run `umbral migrate --allow-drift` to proceed and apply pending migrations.\n  \
                 3. Run `umbral migrate --fake <plugin/name>` to mark an individual migration \
                 as applied without running SQL."
            );
            Err(Box::new(MigrateError::DriftDetected {
                missing: missing.clone(),
            }))
        }
        Err(err) => Err(Box::new(err)),
    }
}

/// Parse `"plugin/name"` into `(&str, &str)`. Returns an error if the
/// format is wrong.
fn parse_migration_spec(
    spec: &str,
) -> Result<(&str, &str), Box<dyn std::error::Error + Send + Sync>> {
    let mut parts = spec.splitn(2, '/');
    let plugin = parts.next().ok_or("migration spec must be `plugin/name`")?;
    let name = parts
        .next()
        .ok_or("migration spec must be `plugin/name`; missing name after `/`")?;
    Ok((plugin, name))
}

async fn showmigrations() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pending = umbral::migrate::show().await?;
    if pending > 0 {
        println!("\n{pending} migration(s) not yet applied.");
    }
    Ok(())
}

/// `umbral checkmigrations` — classify every pending operation for
/// zero-downtime safety (feature #65). Prints the UNSAFE ops first, then
/// WARNING, then a SAFE count, and exits non-zero when any UNSAFE op is
/// present (or any WARNING under `--strict`). Applies nothing.
async fn checkmigrations(strict: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ops = umbral::migrate::check_pending_safety().await?;
    if ops.is_empty() {
        println!("No pending migrations — nothing to check.");
        return Ok(());
    }

    let unsafe_ops: Vec<_> = ops.iter().filter(|c| c.safety.is_unsafe()).collect();
    let warn_ops: Vec<_> = ops.iter().filter(|c| c.safety.is_warning()).collect();
    let safe_count = ops.len() - unsafe_ops.len() - warn_ops.len();

    let migrations: std::collections::BTreeSet<_> =
        ops.iter().map(|c| (&c.plugin, &c.migration)).collect();
    println!(
        "Checking {} operation(s) across {} pending migration(s)...\n",
        ops.len(),
        migrations.len()
    );

    if !unsafe_ops.is_empty() {
        println!("UNSAFE ({}):", unsafe_ops.len());
        for c in &unsafe_ops {
            println!(
                "  [{}] {}/{} — {}",
                op_kind(&c.op),
                c.plugin,
                c.migration,
                c.safety.reason()
            );
        }
        println!();
    }

    if !warn_ops.is_empty() {
        println!("WARNING ({}):", warn_ops.len());
        for c in &warn_ops {
            println!(
                "  [{}] {}/{} — {}",
                op_kind(&c.op),
                c.plugin,
                c.migration,
                c.safety.reason()
            );
        }
        println!();
    }

    println!(
        "Summary: {} safe, {} warning, {} unsafe.",
        safe_count,
        warn_ops.len(),
        unsafe_ops.len()
    );

    // Gate: UNSAFE always fails; WARNING fails only under --strict.
    let blocked = !unsafe_ops.is_empty() || (strict && !warn_ops.is_empty());
    if blocked {
        let why = if !unsafe_ops.is_empty() {
            format!("{} unsafe operation(s) found", unsafe_ops.len())
        } else {
            format!("{} warning(s) found (--strict)", warn_ops.len())
        };
        return Err(format!(
            "checkmigrations: {why}. Review the expand-contract notes above before deploying."
        )
        .into());
    }

    println!("\nAll pending operations are safe for a rolling deploy.");
    Ok(())
}

/// Short uppercase tag for an operation, used in the `checkmigrations`
/// report (e.g. `DROP TABLE`, `RENAME COL`, `ADD COL`).
fn op_kind(op: &umbral::migrate::Operation) -> &'static str {
    use umbral::migrate::Operation;
    match op {
        Operation::CreateTable { .. } => "CREATE TABLE",
        Operation::DropTable { .. } => "DROP TABLE",
        Operation::AddColumn { .. } => "ADD COL",
        Operation::DropColumn { .. } => "DROP COL",
        Operation::AlterColumn { .. } => "ALTER COL",
        Operation::RenameTable { .. } => "RENAME TABLE",
        Operation::RenameColumn { .. } => "RENAME COL",
        Operation::CreateM2MTable { .. } => "CREATE M2M",
        Operation::DropM2MTable { .. } => "DROP M2M",
        Operation::RunSql { .. } => "RUN SQL",
        Operation::AddIndex { unique: true, .. } => "ADD UNIQUE",
        Operation::AddIndex { unique: false, .. } => "ADD INDEX",
        Operation::DropIndex { .. } => "DROP INDEX",
    }
}

async fn inspectdb(
    output: PathBuf,
    mark_applied: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let opts = InspectOptions {
        output,
        mark_applied,
    };
    match umbral::inspect::inspectdb(opts).await {
        Ok(report) => {
            println!(
                "Inspected {} table(s), {} column(s)",
                report.tables, report.columns,
            );
            println!("Wrote {}", report.models_path.display());
            println!("Wrote {}", report.migration_path.display());
            Ok(())
        }
        Err(InspectError::NoTables) => {
            println!("no tables found in the database");
            Ok(())
        }
        Err(err) => Err(Box::new(err)),
    }
}

async fn dumpdata(output: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    umbral::backup::dump_to_path(&output).await?;
    println!("Wrote {}", output.display());
    Ok(())
}

async fn loaddata(input: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let report = umbral::backup::load_from_path(&input).await?;
    println!(
        "Loaded {} row(s) into {} table(s)",
        report.rows_loaded,
        report.tables_loaded.len()
    );
    for skipped in &report.skipped_tables {
        eprintln!("warning: skipped table `{skipped}` (not in current schema)");
    }
    Ok(())
}

/// `umbral importcsv <table> <file.csv>` — parse the CSV (the `csv` crate
/// handles quoting/escaping) and hand the header + string rows to
/// `import_table_rows`, which coerces each cell to its column type and
/// inserts through the validated dynamic write path.
async fn importcsv(
    table: String,
    input: PathBuf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Resolve the table against the registered models so a typo fails
    // loudly (with the list of valid tables) before we read the file.
    let models = umbral::migrate::registered_models();
    let Some(meta) = models.into_iter().find(|m| m.table == table) else {
        let mut known: Vec<String> = umbral::migrate::registered_models()
            .iter()
            .map(|m| m.table.clone())
            .collect();
        known.sort();
        return Err(format!(
            "importcsv: unknown table `{table}`. Registered tables: {}",
            known.join(", ")
        )
        .into());
    };

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_path(&input)?;
    let headers: Vec<String> = reader.headers()?.iter().map(|s| s.to_string()).collect();
    if headers.is_empty() {
        return Err("importcsv: the CSV has no header row".into());
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    for record in reader.records() {
        let record = record?;
        rows.push(record.iter().map(|s| s.to_string()).collect());
    }

    let report = umbral::orm::import_table_rows(&meta, &headers, &rows).await;
    println!(
        "Imported {} row(s) into `{}` ({} failed)",
        report.inserted,
        table,
        report.errors.len()
    );
    for (line, message) in &report.errors {
        eprintln!("  line {line}: {message}");
    }
    // Non-zero exit when any row failed, so a CI/script catches a partial
    // import without parsing stdout.
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(format!("importcsv: {} row(s) failed", report.errors.len()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use clap::ArgMatches;
    use umbral::Settings;
    use umbral_core::cli::{CliError, PluginCommand};
    use umbral_core::plugin::Plugin;

    #[test]
    fn forward_args_prefix_cargo_run_dashdash() {
        // `umbral dev` → `cargo run -- dev`
        assert_eq!(
            cargo_run_forward_args(&["dev".to_string()]),
            vec!["run", "--", "dev"]
        );
        // Flags and extra args ride along verbatim.
        assert_eq!(
            cargo_run_forward_args(&[
                "migrate".to_string(),
                "--fake".to_string(),
                "accounts/0001_auto".to_string(),
            ]),
            vec!["run", "--", "migrate", "--fake", "accounts/0001_auto"]
        );
    }

    #[test]
    fn in_cargo_project_detects_manifest_upward() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        // No Cargo.toml anywhere yet.
        assert!(!in_cargo_project(root));
        // A manifest at the root is found from a nested subdir (like cargo).
        std::fs::write(root.join("Cargo.toml"), b"[package]\nname='x'\n").unwrap();
        let nested = root.join("src").join("widgets");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(in_cargo_project(&nested), "walks up to find the manifest");
        assert!(in_cargo_project(root));
    }

    struct WorkerCmd;

    #[async_trait]
    impl PluginCommand for WorkerCmd {
        fn command(&self) -> clap::Command {
            clap::Command::new("tasks-worker").about("Run the task worker")
        }
        async fn run(&self, _m: &ArgMatches) -> Result<(), CliError> {
            Ok(())
        }
    }

    struct WorkerPlugin;

    impl Plugin for WorkerPlugin {
        fn name(&self) -> &'static str {
            "tasks"
        }
        fn commands(&self) -> Vec<Box<dyn PluginCommand>> {
            vec![Box::new(WorkerCmd)]
        }
    }

    async fn app_with_worker() -> App {
        let settings = Settings::from_env().expect("figment defaults load");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite connects");
        App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(WorkerPlugin)
            .build()
            .expect("App builds")
    }

    #[test]
    fn wants_top_level_help_recognizes_help_forms() {
        let os = |s: &str| std::ffi::OsString::from(s);
        assert!(wants_top_level_help(&[os("umbral"), os("help")]));
        assert!(wants_top_level_help(&[os("umbral"), os("--help")]));
        assert!(wants_top_level_help(&[os("umbral"), os("-h")]));
        // Bare invocation keeps the serve default — NOT intercepted.
        assert!(!wants_top_level_help(&[os("umbral")]));
        // `migrate --help` is command-specific, left to clap.
        assert!(!wants_top_level_help(&[
            os("umbral"),
            os("migrate"),
            os("--help")
        ]));
        // A real subcommand is not help.
        assert!(!wants_top_level_help(&[os("umbral"), os("migrate")]));
    }

    #[test]
    fn unknown_token_picks_first_non_flag() {
        let os = |s: &str| std::ffi::OsString::from(s);
        assert_eq!(
            unknown_token(&[os("umbral"), os("--verbose"), os("frobnicate")]).as_deref(),
            Some("frobnicate")
        );
        assert_eq!(unknown_token(&[os("umbral")]), None);
    }

    // NOTE: both the help and unknown-command paths are asserted in ONE
    // test because `App::build` calls the global `settings::init` (a
    // `OnceLock`) which panics if called twice in the same process.
    // Building one App and exercising both render paths against it sidesteps
    // that, and is also a faithful "one process, one App" shape.
    #[tokio::test]
    async fn help_and_unknown_list_builtins_and_plugin_commands() {
        let app = app_with_worker().await;

        // --- full help (umbral help / --help) ---
        let out = render_full_help(&app);
        // A built-in subcommand with its real `about`.
        assert!(
            out.contains("migrate"),
            "built-in `migrate` missing:\n{out}"
        );
        assert!(
            out.contains("Apply every pending migration"),
            "built-in `migrate` about missing:\n{out}"
        );
        // The plugin-contributed command with its about.
        assert!(
            out.contains("tasks-worker") && out.contains("Run the task worker"),
            "plugin command missing:\n{out}"
        );
        // Column alignment: built-in and plugin descriptions start at the
        // same offset on their respective lines.
        let mig_line = out
            .lines()
            .find(|l| l.trim_start().starts_with("migrate"))
            .unwrap();
        let worker_line = out.lines().find(|l| l.contains("tasks-worker")).unwrap();
        let mig_col = mig_line.find("Apply every pending migration").unwrap();
        let worker_col = worker_line.find("Run the task worker").unwrap();
        assert_eq!(mig_col, worker_col, "descriptions not aligned:\n{out}");

        // --- unknown command (umbral frobnicate) ---
        let out = render_unknown(&app, Some("frobnicate"));
        assert!(
            out.contains("unknown command") && out.contains("frobnicate"),
            "missing unknown-command error:\n{out}"
        );
        // Still shows what IS available — both a built-in and the plugin cmd.
        assert!(out.contains("migrate"), "listing missing built-in:\n{out}");
        assert!(
            out.contains("tasks-worker"),
            "listing missing plugin cmd:\n{out}"
        );
    }
}
