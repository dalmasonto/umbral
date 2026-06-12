//! Library surface for user binaries to host umbra's management
//! subcommands.
//!
//! umbra-cli ships as two artefacts. The library (this crate) exposes
//! [`dispatch`] — the entry point user binaries call to gain the
//! `serve` / `migrate` / `makemigrations` / `inspectdb` /
//! `dumpdata` / `loaddata` subcommands. The binary (`umbra`) ships as
//! the global scaffolding tool installed via `cargo install
//! umbra-cli`, and handles `startproject` / `startapp` from outside
//! any project.
//!
//! ## Quickstart
//!
//! In your project's `src/main.rs`:
//!
//! ```ignore
//! use umbra::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//!     tracing_subscriber::fmt::init();
//!
//!     let settings = Settings::from_env()?;
//!     let pool = umbra::db::connect(&settings.database_url).await?;
//!
//!     let app = App::builder()
//!         .settings(settings)
//!         .database("default", pool)
//!         .model::<Article>()
//!         .build()?;
//!
//!     umbra_cli::dispatch(app).await
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

use clap::{Parser, Subcommand};
use umbra::App;
use umbra::inspect::{InspectError, InspectOptions};
use umbra::migrate::MigrateError;

pub mod scaffold;

#[derive(Debug, Parser)]
#[command(
    name = "umbra",
    about = "umbra management commands. Run from your project's binary.",
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
    /// `--addr` or `UMBRA_BIND_ADDR`.
    Serve {
        /// Override `settings.bind_addr`. Format: `host:port`
        /// (e.g. `127.0.0.1:3000`).
        #[arg(long)]
        addr: Option<String>,
    },
    /// Diff registered models against the latest snapshot and write a
    /// new migration file per plugin with changes.
    Makemigrations,
    /// Apply every pending migration against the ambient pool.
    Migrate {
        /// Mark a specific migration as applied in the tracking table
        /// WITHOUT running its SQL. Recovery path when the schema
        /// already exists (e.g. migrated outside umbra). Format:
        /// `<plugin>/<migration_name>` (e.g. `app/0001_create_post`).
        #[arg(long, value_name = "PLUGIN/NAME")]
        fake: Option<String>,
        /// For each plugin, if the first migration's tables already
        /// exist in the database, mark it applied without running SQL.
        /// Use when adopting a database bootstrapped outside umbra.
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
    /// Introspect the ambient database into a `models.rs` plus an
    /// initial migration. Used to onboard an existing schema.
    Inspectdb {
        /// Directory the generated files are written under.
        #[arg(long)]
        output: PathBuf,
        /// Record `0001_initial` in `umbra_migrations` after writing
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
        /// `umbra dev -- migrate` re-runs `cargo run -- migrate`
        /// on every change.
        #[arg(last = true)]
        run_args: Vec<String>,
    },
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
/// commands first via [`umbra_core::cli::dispatch`], then the built-in
/// subcommand set (`serve` / `migrate` / etc.).
pub async fn dispatch_with_argv(
    app: App,
    argv: Vec<std::ffi::OsString>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Step 1: try plugin-contributed subcommands first. Each registered
    // plugin's `commands()` is queried; if argv matches one of them
    // (e.g. `createsuperuser` from `umbra-auth`, `worker` from
    // `umbra-tasks`), that command's `run` fires and we return. If no
    // plugin command matches argv, fall through to the built-in
    // subcommand set below.
    if !app.plugins().is_empty() {
        match umbra_core::cli::dispatch(app.plugins(), argv.clone()).await {
            Ok(umbra_core::cli::DispatchOutcome::Matched(_)) => return Ok(()),
            Ok(umbra_core::cli::DispatchOutcome::Help(msg)) => {
                // A plugin command's --help was requested. Print and exit clean.
                print!("{msg}");
                return Ok(());
            }
            Ok(umbra_core::cli::DispatchOutcome::Unmatched) => {
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
            // clap formats the error (usage / suggestion / --help body)
            // and returns it as the error message.
            e.print()?;
            // Exit 0 for --help/--version; otherwise exit 2 for usage.
            std::process::exit(if e.use_stderr() { 2 } else { 0 });
        }
    };
    match cli.command.unwrap_or(Command::Serve { addr: None }) {
        Command::Serve { addr } => serve(app, addr).await,
        Command::Makemigrations => makemigrations().await,
        Command::Migrate {
            fake,
            fake_initial,
            allow_drift,
        } => migrate(fake, fake_initial, allow_drift).await,
        Command::Showmigrations => showmigrations().await,
        Command::Inspectdb {
            output,
            mark_applied,
        } => inspectdb(output, mark_applied).await,
        Command::Dumpdata { output } => dumpdata(output).await,
        Command::Loaddata { input } => loaddata(input).await,
        Command::Dev { watch, run_args } => dev(watch, run_args).await,
    }
}

/// `umbra dev` — wraps `cargo-watch` to re-run `cargo run` on source
/// changes. If `cargo-watch` isn't installed, prints the install hint
/// and exits non-zero so the user notices.
///
/// Template edits don't need this command — they hot-reload in-process
/// when `settings.environment == Dev` (see `umbra-core/src/templates.rs`).
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
            "umbra dev: `cargo-watch` is not installed.\n\n\
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

    eprintln!("umbra dev: watching for changes, running `cargo {cargo_cmd}` on each save");
    eprintln!("umbra dev: templates also hot-reload in-process; no restart needed for .html edits");
    eprintln!("umbra dev: Ctrl-C to stop");
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
        None => umbra_core::settings::get().bind_addr.clone(),
    };
    let addr: SocketAddr = addr_str
        .parse()
        .map_err(|e| format!("umbra: invalid bind_addr `{addr_str}`: {e}"))?;
    app.serve(addr).await?;
    Ok(())
}

async fn makemigrations() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match umbra::migrate::make().await {
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
        umbra::migrate::fake_apply(plugin, name).await?;
        println!("Marked {spec} as applied (no SQL executed)");
        return Ok(());
    }

    // --fake-initial: for every plugin, if the 0001 tables exist, fake-apply.
    if fake_initial {
        let n = umbra::migrate::fake_initial().await?;
        if n == 0 {
            println!("No plugins needed fake-initial (either already applied or tables absent)");
        } else {
            println!("Fake-applied initial migration for {n} plugin(s)");
        }
        return Ok(());
    }

    // Normal migrate with optional --allow-drift.
    match umbra::migrate::run_checked(allow_drift).await {
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
            eprintln!("error: umbra migrate: drift detected");
            eprintln!("  The following migrations are in the tracking table but missing on disk:");
            for name in &names {
                eprintln!("    [!] {name}");
            }
            eprintln!();
            eprintln!(
                "  Options:\n  \
                 1. Restore the file(s) from VCS.\n  \
                 2. Run `umbra migrate --allow-drift` to proceed and apply pending migrations.\n  \
                 3. Run `umbra migrate --fake <plugin/name>` to mark an individual migration \
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
    let pending = umbra::migrate::show().await?;
    if pending > 0 {
        println!("\n{pending} migration(s) not yet applied.");
    }
    Ok(())
}

async fn inspectdb(
    output: PathBuf,
    mark_applied: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let opts = InspectOptions {
        output,
        mark_applied,
    };
    match umbra::inspect::inspectdb(opts).await {
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
    umbra::backup::dump_to_path(&output).await?;
    println!("Wrote {}", output.display());
    Ok(())
}

async fn loaddata(input: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let report = umbra::backup::load_from_path(&input).await?;
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
