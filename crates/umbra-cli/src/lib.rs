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
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    Migrate,
    /// List applied vs pending migrations per plugin.
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
pub async fn dispatch(app: App) -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve { addr: None }) {
        Command::Serve { addr } => serve(app, addr).await,
        Command::Makemigrations => makemigrations().await,
        Command::Migrate => migrate().await,
        Command::Showmigrations => showmigrations().await,
        Command::Inspectdb {
            output,
            mark_applied,
        } => inspectdb(output, mark_applied).await,
        Command::Dumpdata { output } => dumpdata(output).await,
        Command::Loaddata { input } => loaddata(input).await,
    }
}

async fn serve(app: App, addr_override: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
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

async fn makemigrations() -> Result<(), Box<dyn std::error::Error>> {
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

async fn migrate() -> Result<(), Box<dyn std::error::Error>> {
    let n = umbra::migrate::run().await?;
    if n == 0 {
        println!("No pending migrations");
    } else {
        println!("Applied {n} migration(s)");
    }
    Ok(())
}

async fn showmigrations() -> Result<(), Box<dyn std::error::Error>> {
    umbra::migrate::show().await?;
    Ok(())
}

async fn inspectdb(output: PathBuf, mark_applied: bool) -> Result<(), Box<dyn std::error::Error>> {
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

async fn dumpdata(output: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    umbra::backup::dump_to_path(&output).await?;
    println!("Wrote {}", output.display());
    Ok(())
}

async fn loaddata(input: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
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
