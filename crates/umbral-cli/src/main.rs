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
//! - `umbral startcommand [name] [--in root|<plugin>]` — create a
//!   management command (`cargo run -- <name>`), interactively asking
//!   where it should live, and register it there.
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
    about = "umbral CLI. Scaffolds projects, plugins and commands \
             (startproject/startapp/startplugin/startcommand) and runs project-free \
             utilities (maskkeygen) directly; every other command (serve, migrate, \
             makemigrations, worker, seed_data, …) is forwarded to \
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

    /// Create a management command (`cargo run -- <name>`).
    ///
    /// Interactive by default: asks for the command's name, then where it
    /// lives — the project root, or one of the plugins under `plugins/`
    /// (they're listed; you pick). It writes `commands/<name>.rs`, keeps a
    /// `commands/mod.rs` registry, and wires that registry into `main.rs`
    /// (root) or the plugin's `Plugin::commands()` — so the command is
    /// runnable the moment it's written, with nothing to register by hand.
    ///
    /// Pass `<NAME>` and `--in` to skip the prompts (CI, scripts).
    Startcommand {
        /// Command name — what you'll type after `cargo run --`. Prompted
        /// for if omitted.
        name: Option<String>,
        /// Where the command lives: `root` for the project's own binary, or
        /// a plugin name from `plugins/`. Prompted for if omitted.
        #[arg(long = "in", value_name = "root|PLUGIN")]
        target: Option<String>,
        /// Project root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        path: PathBuf,
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

/// `umbral startcommand [NAME] [--in root|<plugin>]`.
///
/// Anything not passed on the command line is asked for, which is the whole
/// point of the command: you shouldn't have to know that a plugin's commands
/// live behind `Plugin::commands()` and a project's behind
/// `AppBuilder::commands()` in order to write your first one.
///
/// Non-interactive when stdin isn't a terminal (a script, CI, a pipe): the
/// flags become required, and a missing one is an error rather than a
/// prompt that would block forever on a closed stdin.
fn run_startcommand(
    name: Option<String>,
    target: Option<String>,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // `CommandTarget` IS `codegen::Target` (a re-export), so there is nothing to
    // convert between them.
    use umbral::codegen::{Target as CommandTarget, prompt};
    use umbral_cli::scaffold::scaffold_command;

    // The prompts come from `umbral::codegen::prompt`, the same ones a plugin's
    // generator uses (`umbral-rest`'s startpermission / startauthentication /
    // …). One implementation means one behaviour: the menu reads the same, and
    // the non-TTY rule — never prompt a pipe — holds everywhere rather than in
    // whichever generator remembered it.
    let interactive = prompt::is_interactive();

    let name = match name {
        Some(n) => n,
        None if interactive => prompt::ask_required("Command name (e.g. backfill_slugs): ")?,
        None => {
            return Err("a command name is required when stdin isn't a terminal: \
                        `umbral startcommand <NAME> --in root`"
                .into());
        }
    };
    let name = name.trim().to_string();

    let target = match target {
        Some(t) => CommandTarget::parse(&t),
        None if interactive => prompt::ask_target(path)?,
        None => {
            return Err("`--in <root|PLUGIN>` is required when stdin isn't a terminal".into());
        }
    };

    let report = scaffold_command(&name, &target, path)?;

    println!("Created in `{}`:", report.root.display());
    for f in &report.files {
        println!("  {}", f.display());
    }
    println!();
    // Report what was ACTUALLY wired, not what was asked for. Announcing
    // "Registered" for a command the tool failed to register is how a user ends
    // up running `cargo run -- <name>`, getting `unknown command`, and trusting
    // the tool less than they trust the error.
    match (&target, report.registered) {
        (_, Some(false)) => {
            println!("NOT registered yet — the steps below are the ones I could not do for you.")
        }
        (CommandTarget::Root, _) => println!(
            "Registered `{name}` on the App builder (src/main.rs: `.commands(commands::all())`)."
        ),
        (CommandTarget::Plugin(p), _) => println!(
            "Registered `{name}` with the `{p}` plugin (src/lib.rs: `Plugin::commands()`)."
        ),
    }
    println!();
    println!("Next steps:");
    for step in &report.next_steps {
        println!("  {step}");
    }
    Ok(())
}

/// Print what a scaffolder wrote: the files, what got registered, and what the
/// user still has to do.
///
/// One printer, because there was one report. The three scaffolding arms were
/// three copies of this block (two of them byte-identical apart from
/// "Next step:" vs "Next steps:"), so any change to the output was a
/// four-place edit.
fn print_report(r: &umbral_cli::scaffold::ScaffoldReport, name: &str, wants_dep: bool) {
    println!("Created `{}`:", r.root.display());
    for f in &r.files {
        println!("  {}", f.display());
    }
    println!();
    // `wants_dep` distinguishes "this scaffolder does not register a dependency
    // at all" (startproject) from "it tried and found no Cargo.toml" — both of
    // which `cargo_toml_registered` spells `None`. That overloading is worth
    // collapsing into one enum; noted for the next pass rather than churned
    // through the tests tonight.
    if wants_dep {
        match r.cargo_toml_registered {
            Some(true) => {
                println!("Registered `{name} = {{ path = \"plugins/{name}\" }}` in Cargo.toml.")
            }
            Some(false) => println!("Cargo.toml already lists `{name}` — no duplicate added."),
            None => println!(
                "Note: could not find a Cargo.toml to update. \
                 Add `{name} = {{ path = \"plugins/{name}\" }}` manually."
            ),
        }
        println!();
    }
    println!("Next steps:");
    for step in &r.next_steps {
        println!("  {step}");
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
    // Every arm reports through the same `error: {err}` line below, so they
    // agree on a boxed error rather than one arm's concrete type.
    let result: Result<(), Box<dyn std::error::Error>> = match cli.command {
        Command::Startproject { name, path, local } => {
            umbral_cli::scaffold::scaffold_project(&name, &path, local.as_deref())
                .map(|r| print_report(&r, &name, false))
                .map_err(Into::into)
        }
        Command::Startapp { name, path, local } => {
            umbral_cli::scaffold::scaffold_app(&name, &path, local.as_deref())
                .map(|r| print_report(&r, &name, true))
                .map_err(Into::into)
        }
        Command::Startplugin { name, path, local } => {
            umbral_cli::scaffold::scaffold_plugin(&name, &path, local.as_deref())
                .map(|r| print_report(&r, &name, true))
                .map_err(Into::into)
        }
        Command::Startcommand { name, target, path } => run_startcommand(name, target, &path),
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
