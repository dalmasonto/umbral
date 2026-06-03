//! The `umbra` global scaffolding binary.
//!
//! `cargo install umbra-cli` installs this as `umbra` on the user's
//! PATH. It handles **scaffolding** commands that don't need an App:
//!
//! - `umbra startproject <name>` — create a new umbra project
//!   directory with `Cargo.toml`, `src/main.rs`, `umbra.toml`,
//!   templates, and a default `404` / `500` page.
//! - `umbra startapp <name>` — create a new plugin crate at
//!   `plugins/<name>/` with a minimal `{Name}Plugin` skeleton.
//! - `umbra startplugin <name>` — like `startapp` but writes a
//!   richer template (example Model with field-type attributes,
//!   example handler, README) aimed at distributable plugins.
//!
//! For every **management** command (`serve`, `migrate`,
//! `makemigrations`, `inspectdb`, etc.), users run them inside their
//! project via `cargo run -- <command>` — the project's own binary
//! hosts those via [`umbra_cli::dispatch`]. This binary points users
//! at that pattern instead of trying to manage their database
//! without their model registry.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "umbra",
    about = "umbra project scaffolding. `cargo install umbra-cli` puts this on your PATH.",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new umbra project in `./<name>/`.
    ///
    /// Scaffolds Cargo.toml, src/main.rs (with `umbra_cli::dispatch`
    /// wired), umbra.toml, a templates/ dir with base / 404 / 500
    /// pages, and a .gitignore.
    Startproject {
        /// Project name. Used as both the Cargo package name and the
        /// directory name. ASCII alphanumeric, underscore, hyphen.
        name: String,
        /// Parent directory. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Create a new plugin (app) crate in `<project>/plugins/<name>/`.
    ///
    /// Run this from inside a project. The generated plugin lives at
    /// `plugins/<name>/` and exports a `{Name}Plugin` struct. Wire
    /// it into your App by editing `src/main.rs` per the printed
    /// instructions. Minimal: one lib.rs with a stub Plugin impl.
    Startapp {
        /// Plugin name. ASCII alphanumeric, underscore, hyphen.
        name: String,
        /// Project root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Create a richer plugin scaffold in `<project>/plugins/<name>/`.
    ///
    /// Like `startapp` but writes a more complete starter: an example
    /// `Model` showing common field attributes (`max_length`,
    /// `choices`, nullable timestamp, `noedit`), an example axum
    /// handler that reads query params and returns JSON, and a
    /// README walking through the layout. Use this when you're
    /// building a plugin you intend to distribute.
    Startplugin {
        /// Plugin name. ASCII alphanumeric, underscore, hyphen.
        name: String,
        /// Project root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Startproject { name, path } => umbra_cli::scaffold::scaffold_project(&name, &path)
            .map(|r| {
                println!("Created `{}`:", r.root.display());
                for f in &r.files {
                    println!("  {}", f.display());
                }
                println!();
                println!("Next steps:");
                for step in &r.next_steps {
                    println!("  {step}");
                }
            }),
        Command::Startapp { name, path } => {
            umbra_cli::scaffold::scaffold_app(&name, &path).map(|r| {
                println!("Created `{}`:", r.root.display());
                for f in &r.files {
                    println!("  {}", f.display());
                }
                println!();
                println!("Next steps:");
                for step in &r.next_steps {
                    println!("  {step}");
                }
            })
        }
        Command::Startplugin { name, path } => umbra_cli::scaffold::scaffold_plugin(&name, &path)
            .map(|r| {
                println!("Created `{}`:", r.root.display());
                for f in &r.files {
                    println!("  {}", f.display());
                }
                println!();
                println!("Next steps:");
                for step in &r.next_steps {
                    println!("  {step}");
                }
            }),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
