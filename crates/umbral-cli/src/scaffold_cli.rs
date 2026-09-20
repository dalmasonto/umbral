//! The scaffolding subcommands (`startproject` / `startapp` / `startplugin` /
//! `startcommand`), defined **once** and shared by both CLI entry points.
//!
//! Two binaries reach these commands:
//!
//! - The global `umbral` binary (`src/main.rs`) — `umbral startplugin foo`.
//! - The app-embedded dispatcher ([`crate::dispatch`]) run as
//!   `cargo run -- startplugin foo` from inside a project.
//!
//! Before this module existed, only `main.rs` knew how to dispatch them, so the
//! app-embedded surface *listed* the scaffolders in its unified help (gap 66)
//! but answered `error: unknown command \`startapp\`` when you actually ran one
//! — the help promised a command the dispatch couldn't honour. Both entry
//! points now call [`try_run_scaffold`], so `cargo run -- startapp --help`
//! renders the command's usage and `cargo run -- startapp foo` scaffolds, in
//! full parity with the global binary.
//!
//! Scaffolding needs **no built `App`** — `startproject` runs where no project
//! exists yet, and the others only write files — so both entry points
//! intercept these commands *before* building or readying the app. That is also
//! what keeps a scaffold invocation from firing plugin `on_ready` hooks (which
//! would seed rows into tables `migrate` has not created).

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

use crate::scaffold::{ScaffoldReport, scaffold_command, scaffold_plugin, scaffold_project};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Standalone clap parser for the scaffolding commands, so `--help` renders each
/// command's real usage (args + flags) at either entry point. `name = "umbral"`
/// so the rendered usage line reads `umbral startplugin …` regardless of which
/// binary parsed it.
#[derive(Debug, Parser)]
#[command(name = "umbral", disable_help_subcommand = true)]
pub struct ScaffoldCli {
    #[command(subcommand)]
    pub command: ScaffoldCommand,
}

#[derive(Debug, Subcommand)]
pub enum ScaffoldCommand {
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
    /// Deprecated alias of `startplugin`. Generates the same plugin crate.
    ///
    /// There is no separate "app" contract — everything under `plugins/`
    /// is a plugin — so `startapp` folds into `startplugin`. Prefer
    /// `startplugin`; this alias prints a deprecation note and forwards.
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
    /// Create a plugin crate in `<project>/plugins/<name>/`.
    ///
    /// Writes a complete starter: an example `Model` showing common field
    /// attributes (`max_length`, `choices`, nullable timestamp, `noedit`),
    /// an example axum handler that reads query params and returns JSON,
    /// and a README walking through the layout. This is the one plugin
    /// scaffolder; `startapp` is a deprecated alias.
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
}

/// The scaffolding command names, read off the clap parser so a new scaffolder
/// reserves its own name with nothing to keep in sync by hand.
pub fn scaffold_command_names() -> Vec<String> {
    ScaffoldCli::command()
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect()
}

/// Whether `name` is one of the scaffolding subcommands.
pub fn is_scaffold_command(name: &str) -> bool {
    ScaffoldCli::command()
        .get_subcommands()
        .any(|s| s.get_name() == name)
}

/// The `(name, about)` rows for the unified help catalog, read off the parser's
/// subcommands (`about` = the first line of each variant's doc comment, exactly
/// what clap renders). This is the one source of truth for the scaffold command
/// listing — `crate::scaffold_command_catalog` delegates here.
pub fn command_catalog() -> Vec<(String, Option<String>)> {
    ScaffoldCli::command()
        .get_subcommands()
        .map(|s| {
            (
                s.get_name().to_string(),
                s.get_about().map(|a| a.to_string()),
            )
        })
        .collect()
}

/// If the first non-flag token in `argv` names a scaffolding command, parse and
/// run it, returning `Some(result)`. Return `None` otherwise so the caller
/// falls through to its own dispatch.
///
/// `argv` is a full argv (element 0 is the program name), matching
/// `std::env::args_os()` and [`crate::dispatch_with_argv`]'s `argv`.
///
/// A `--help` (or a usage error such as a missing name) prints clap's rendered
/// help/usage and **exits the process** — the same convention the built-in
/// subcommand parser uses, so command-specific help exits cleanly at both entry
/// points rather than falling through to the top-level catalog.
pub fn try_run_scaffold(argv: &[OsString]) -> Option<Result<(), BoxError>> {
    let sub = argv
        .iter()
        .skip(1)
        .find(|a| !a.to_string_lossy().starts_with('-'))
        .map(|a| a.to_string_lossy().into_owned())?;
    if !is_scaffold_command(&sub) {
        return None;
    }
    match ScaffoldCli::try_parse_from(argv) {
        Ok(cli) => Some(run_scaffold(cli.command)),
        Err(e) => {
            // `--help`, `--version`, or a usage error (missing NAME, bad flag).
            // Let clap render it and exit with its own convention: 0 for the
            // help/version display, 2 for a genuine usage error.
            let _ = e.print();
            std::process::exit(if e.use_stderr() { 2 } else { 0 });
        }
    }
}

/// Run a parsed scaffolding command: write the files and print the report. This
/// is the single implementation both the global `umbral` binary and the
/// app-embedded dispatcher call.
pub fn run_scaffold(cmd: ScaffoldCommand) -> Result<(), BoxError> {
    match cmd {
        ScaffoldCommand::Startproject { name, path, local } => {
            let r = scaffold_project(&name, &path, local.as_deref())?;
            print_report(&r, &name, false);
            Ok(())
        }
        ScaffoldCommand::Startapp { name, path, local } => {
            // `startapp` is a deprecated alias of `startplugin` — everything
            // generated under plugins/ is a plugin; there is no separate
            // "app" contract. Same output either way.
            eprintln!(
                "note: `startapp` is deprecated — use `startplugin` (there's no separate \
                 \"app\" contract; everything under plugins/ is a plugin)."
            );
            let r = scaffold_plugin(&name, &path, local.as_deref())?;
            print_report(&r, &name, true);
            Ok(())
        }
        ScaffoldCommand::Startplugin { name, path, local } => {
            let r = scaffold_plugin(&name, &path, local.as_deref())?;
            print_report(&r, &name, true);
            Ok(())
        }
        ScaffoldCommand::Startcommand { name, target, path } => {
            run_startcommand(name, target, &path)
        }
    }
}

fn run_startcommand(
    name: Option<String>,
    target: Option<String>,
    path: &std::path::Path,
) -> Result<(), BoxError> {
    // `CommandTarget` IS `codegen::Target` (a re-export), so there is nothing to
    // convert between them.
    use umbral::codegen::{Target as CommandTarget, prompt};

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
/// One printer, because there was one report. The scaffolding arms were copies
/// of this block (two of them byte-identical apart from "Next step:" vs
/// "Next steps:"), so any change to the output was a multi-place edit.
fn print_report(r: &ScaffoldReport, name: &str, wants_dep: bool) {
    println!("Created `{}`:", r.root.display());
    for f in &r.files {
        println!("  {}", f.display());
    }
    println!();
    // `wants_dep` distinguishes "this scaffolder does not register a dependency
    // at all" (startproject) from "it tried and found no Cargo.toml" — both of
    // which `cargo_toml_registered` spells `None`. That overloading is worth
    // collapsing into one enum; noted for the next pass.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn os(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(OsString::from).collect()
    }

    #[test]
    fn recognizes_the_four_scaffolders_and_nothing_else() {
        for name in ["startproject", "startapp", "startplugin", "startcommand"] {
            assert!(is_scaffold_command(name), "`{name}` should be a scaffolder");
        }
        for name in ["migrate", "serve", "plugin", "frobnicate", ""] {
            assert!(
                !is_scaffold_command(name),
                "`{name}` must NOT be a scaffolder"
            );
        }
    }

    #[test]
    fn scaffold_help_renders_the_command_usage_not_unknown_command() {
        // The bug: `startapp --help` used to fall through to `error: unknown
        // command`. clap must instead surface DisplayHelp with the command's
        // args (NAME, --path, --local) so the user sees the real usage.
        let err = ScaffoldCli::try_parse_from(os(&["umbral", "startapp", "--help"]))
            .expect_err("--help returns a clap Err carrying the help text");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = err.to_string();
        assert!(help.contains("--path"), "usage missing --path:\n{help}");
        assert!(help.contains("--local"), "usage missing --local:\n{help}");
        // Deprecation is documented right in the about line clap renders.
        assert!(
            help.contains("Deprecated alias") || help.contains("startplugin"),
            "startapp help should point at startplugin:\n{help}"
        );
    }

    #[test]
    fn non_scaffold_argv_falls_through() {
        // A management command must return None so the caller's own dispatch
        // (built-ins / forwarding) handles it.
        assert!(try_run_scaffold(&os(&["umbral", "migrate"])).is_none());
        assert!(try_run_scaffold(&os(&["umbral", "--verbose", "serve"])).is_none());
        // No subcommand at all: nothing to scaffold.
        assert!(try_run_scaffold(&os(&["umbral"])).is_none());
    }

    #[test]
    fn try_run_scaffold_actually_writes_a_plugin() {
        // Behavioral: drive the real public path (argv → try_run_scaffold →
        // files on disk), not just the parser. This is the parity the fix
        // promises: `cargo run -- startplugin foo` scaffolds for real.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().to_string_lossy().into_owned();
        let result = try_run_scaffold(&os(&["umbral", "startplugin", "widgets", "--path", &path]))
            .expect("startplugin is a scaffold command, so this is Some");
        result.expect("scaffolding a fresh plugin succeeds");

        let plugin_dir = tmp.path().join("plugins").join("widgets");
        assert!(
            plugin_dir.join("Cargo.toml").is_file(),
            "startplugin must write plugins/widgets/Cargo.toml"
        );
        assert!(
            plugin_dir.join("src").join("lib.rs").is_file(),
            "startplugin must write plugins/widgets/src/lib.rs"
        );
    }
}
