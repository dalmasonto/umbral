//! The `umbral` global scaffolding binary.
//!
//! `cargo install umbral-cli` installs this as `umbral` on the user's
//! PATH. It handles **scaffolding** commands that don't need an App:
//!
//! - `umbral startproject <name>` — create a new umbral project
//!   directory with `Cargo.toml`, `src/main.rs`, `umbral.toml`,
//!   templates, and a default `404` / `500` page.
//! - `umbral startapp <name>` — create a new plugin crate at
//!   `plugins/<name>/` with a minimal `{Name}Plugin` skeleton.
//! - `umbral startplugin <name>` — like `startapp` but writes a
//!   richer template (example Model with field-type attributes,
//!   example handler, README) aimed at distributable plugins.
//!
//! Every other (**management**) command — `serve`, `dev`, `migrate`,
//! `makemigrations`, `inspectdb`, `worker`, … — is **forwarded** to the
//! current project's binary as `cargo run -- <command>` (those commands
//! are hosted by [`umbral_cli::dispatch`] inside the project, where the
//! model registry lives). So `umbral dev` is shorthand for
//! `cargo run -- dev`. Run these from inside a project directory; the
//! equivalent `cargo run -- <command>` form always works too.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "umbral",
    about = "umbral CLI. Scaffolds projects (startproject/startapp/startplugin) and \
             runs project-free utilities (maskkeygen) directly; every other command \
             (serve, migrate, makemigrations, worker, seed_data, …) is forwarded to \
             `cargo run -- <command>` in the current project.",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new umbral project in `./<name>/`.
    ///
    /// Scaffolds Cargo.toml, src/main.rs (with `umbral_cli::dispatch`
    /// wired), umbral.toml, a templates/ dir with base / 404 / 500
    /// pages, and a .gitignore.
    Startproject {
        /// Project name. Used as both the Cargo package name and the
        /// directory name. ASCII alphanumeric, underscore, hyphen.
        name: String,
        /// Parent directory. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Path to a local umbral repo checkout. When set, scaffold
        /// path-deps every umbral crate against the checkout instead
        /// of the public `git = "..."` URL. Closes BUG-17 from
        /// `bugs/tests/testBugs.md` — lets contributors / framework
        /// dev iterate without pushing to a remote.
        #[arg(long, value_name = "PATH")]
        local: Option<PathBuf>,
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
        /// Path to a local umbral repo checkout. See `startproject --local`.
        #[arg(long, value_name = "PATH")]
        local: Option<PathBuf>,
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
        /// Path to a local umbral repo checkout. See `startproject --local`.
        #[arg(long, value_name = "PATH")]
        local: Option<PathBuf>,
    },

    /// Any non-scaffolding command (`dev`, `migrate`, `makemigrations`,
    /// `serve`, `worker`, …) is captured here and forwarded to the current
    /// project's binary via `cargo run -- <args>`. So `umbral dev` runs
    /// `cargo run -- dev`.
    #[command(external_subcommand)]
    Forward(Vec<String>),
}

/// Forward `umbral <cmd> [args...]` to the current project via
/// `cargo run -- <cmd> [args...]`, inheriting stdio and propagating the
/// child's exit code. Requires a Cargo project in (or above) the working
/// directory; otherwise prints a clear error rather than cargo's.
fn forward_to_project(args: &[String]) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("error: cannot read the current directory: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !umbral_cli::in_cargo_project(&cwd) {
        let cmd = args.first().map(String::as_str).unwrap_or("<command>");
        eprintln!(
            "error: `umbral {cmd}` must run inside an umbral project — no Cargo.toml in {} \
             (or any parent).\n  cd into your project directory, or create one with \
             `umbral startproject <name>`.",
            cwd.display()
        );
        return ExitCode::FAILURE;
    }
    let cargo_args = umbral_cli::cargo_run_forward_args(args);
    match std::process::Command::new("cargo")
        .args(&cargo_args)
        .status()
    {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("error: failed to run `cargo {}`: {e}", cargo_args.join(" "));
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Non-scaffolding commands are handled in one of two ways:
    //   1. Project-INDEPENDENT built-ins (e.g. `maskkeygen`) run right here —
    //      no project, no `cargo run` build. See `STANDALONE_COMMANDS`.
    //   2. Everything else (`serve`, `migrate`, `seed_data`, custom plugin
    //      commands, …) needs the project's compiled `App`, so it forwards to
    //      `cargo run -- <cmd>` in the current project.
    if let Command::Forward(args) = &cli.command {
        if let Some(result) = umbral_cli::try_run_standalone(args) {
            return match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        return forward_to_project(args);
    }
    let result = match cli.command {
        Command::Startproject { name, path, local } => {
            umbral_cli::scaffold::scaffold_project(&name, &path, local.as_deref()).map(|r| {
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
        Command::Startapp { name, path, local } => {
            umbral_cli::scaffold::scaffold_app(&name, &path, local.as_deref()).map(|r| {
                println!("Created `{}`:", r.root.display());
                for f in &r.files {
                    println!("  {}", f.display());
                }
                println!();
                match r.cargo_toml_registered {
                    Some(true) => println!(
                        "Registered `{name} = {{ path = \"plugins/{name}\" }}` in Cargo.toml."
                    ),
                    Some(false) => {
                        println!("Cargo.toml already lists `{name}` — no duplicate added.")
                    }
                    None => println!(
                        "Note: could not find a Cargo.toml to update. \
                         Add `{name} = {{ path = \"plugins/{name}\" }}` manually."
                    ),
                }
                println!();
                println!("Next step:");
                for step in &r.next_steps {
                    println!("  {step}");
                }
            })
        }
        Command::Startplugin { name, path, local } => {
            umbral_cli::scaffold::scaffold_plugin(&name, &path, local.as_deref()).map(|r| {
                println!("Created `{}`:", r.root.display());
                for f in &r.files {
                    println!("  {}", f.display());
                }
                println!();
                match r.cargo_toml_registered {
                    Some(true) => println!(
                        "Registered `{name} = {{ path = \"plugins/{name}\" }}` in Cargo.toml."
                    ),
                    Some(false) => {
                        println!("Cargo.toml already lists `{name}` — no duplicate added.")
                    }
                    None => println!(
                        "Note: could not find a Cargo.toml to update. \
                         Add `{name} = {{ path = \"plugins/{name}\" }}` manually."
                    ),
                }
                println!();
                println!("Next steps:");
                for step in &r.next_steps {
                    println!("  {step}");
                }
            })
        }
        // Handled by the early return above; kept for match exhaustiveness.
        Command::Forward(_) => unreachable!("Forward is dispatched before this match"),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
