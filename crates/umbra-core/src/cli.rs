//! Plugin-contributed CLI subcommands — the M7 `Plugin::commands()`
//! deferral landing.
//!
//! Plugins implement [`PluginCommand`] to expose a `clap` subcommand
//! and an async handler. `App::build()` retains every registered
//! plugin in topological order; [`dispatch`] walks that list,
//! collects each plugin's commands, builds a single top-level clap
//! parser, and routes the user's args to the right handler.
//!
//! ## Why a trait, not a function pointer
//!
//! Plugin commands are async, and the implementation often needs to
//! capture instance state from the plugin (a configured prefix, a
//! handler registry, etc.). `Box<dyn PluginCommand>` lets a plugin
//! pass values through; a `fn` pointer can't carry closure state.
//!
//! ## Why clap
//!
//! `clap` is the de-facto Rust CLI library and is already in use by
//! `umbra-cli`. Plugins return a `clap::Command`, which carries help
//! text, arg validation, and subcommand groupings for free. The
//! dispatcher composes the per-plugin `Command` values under a
//! single parent so `umbra-cli <plugin-cmd>` works as one tree.
//!
//! ## Example
//!
//! ```ignore
//! use umbra::cli::{dispatch, CliError, PluginCommand};
//!
//! struct WorkerCmd;
//!
//! #[async_trait::async_trait]
//! impl PluginCommand for WorkerCmd {
//!     fn command(&self) -> clap::Command {
//!         clap::Command::new("tasks-worker")
//!             .about("Run the background task worker")
//!             .arg(clap::Arg::new("once")
//!                 .long("once")
//!                 .action(clap::ArgAction::SetTrue))
//!     }
//!
//!     async fn run(&self, m: &clap::ArgMatches) -> Result<(), CliError> {
//!         if m.get_flag("once") {
//!             umbra_tasks::run_worker_once().await?;
//!         } else {
//!             // run_worker loops forever
//!             umbra_tasks::run_worker(Default::default()).await
//!         }
//!         Ok(())
//!     }
//! }
//! ```

use std::ffi::OsString;

use async_trait::async_trait;
use clap::ArgMatches;

use crate::plugin::Plugin;

/// Error returned by a plugin command. Boxed so plugins can return
/// any concrete error type without forcing the trait into a
/// generic-over-E shape.
pub type CliError = Box<dyn std::error::Error + Send + Sync>;

/// One CLI subcommand contributed by a plugin.
#[async_trait]
pub trait PluginCommand: Send + Sync + 'static {
    /// The clap subcommand. `Command::get_name()` is the literal the
    /// user types after the program name (`umbra-cli <name>`).
    /// Long-form help, arg parsing, and subcommand grouping are all
    /// the plugin's to configure on the returned value.
    fn command(&self) -> clap::Command;

    /// Run the command. Called after clap has parsed args matching
    /// `self.command()`; `matches` is the per-subcommand
    /// `ArgMatches` (not the top-level one).
    async fn run(&self, matches: &ArgMatches) -> Result<(), CliError>;
}

/// Outcome of a dispatch call. Lets the caller decide what to do when
/// no plugin command matched — typically the framework binary then
/// falls through to its hardcoded subcommands (`serve`, `migrate`,
/// `makemigrations`, …).
#[derive(Debug)]
pub enum DispatchOutcome {
    /// A plugin command matched and its `run` completed. The bool is
    /// the matched subcommand name so the binary can log / report.
    Matched(String),
    /// No plugin command matched the parsed args. The framework
    /// binary should handle the request itself or surface a "no such
    /// command" error.
    Unmatched,
    /// User asked for help (`--help` on the top level). The
    /// formatted message is captured here so the binary can print
    /// it (or merge with its own help).
    Help(String),
}

/// Dispatch CLI args across the registered plugins' commands.
///
/// `args` is the raw `std::env::args_os()` slice including argv[0].
/// The dispatcher builds a top-level `clap::Command` named after
/// argv[0], hangs every plugin's contributed subcommand off it, and
/// matches.
///
/// Duplicate command names across plugins are caught here (as a
/// build-time would be ideal, but the plugin set isn't known at
/// build time): the second plugin to register the same name loses,
/// and a warning is logged.
pub async fn dispatch<I, T>(
    plugins: &[Box<dyn Plugin>],
    args: I,
) -> Result<DispatchOutcome, CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    // Collect every plugin command, deduplicating by name. The
    // first-registered wins; subsequent collisions log and are
    // dropped.
    let mut commands: Vec<(String, Box<dyn PluginCommand>)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for plugin in plugins {
        for cmd in plugin.commands() {
            let name = cmd.command().get_name().to_string();
            if !seen.insert(name.clone()) {
                tracing::warn!(
                    target: "umbra::cli",
                    "duplicate plugin command `{name}` from `{}`; ignoring",
                    plugin.name()
                );
                continue;
            }
            commands.push((name, cmd));
        }
    }

    // Nothing contributed → caller handles everything.
    if commands.is_empty() {
        return Ok(DispatchOutcome::Unmatched);
    }

    let mut root = clap::Command::new("umbra")
        .about("umbra plugin subcommands")
        .disable_help_subcommand(true)
        .subcommand_required(false)
        .arg_required_else_help(false);
    for (_, cmd) in &commands {
        root = root.subcommand(cmd.command());
    }

    // Run match. `try_get_matches_from` swallows the std::process::exit
    // clap usually does on parse error / --help.
    let owned: Vec<OsString> = args.into_iter().map(|t| t.into()).collect();
    let matches = match root.clone().try_get_matches_from(owned) {
        Ok(m) => m,
        Err(e) => {
            // --help / --version / parse errors. For --help we surface
            // the rendered text so the caller can print it. For
            // "subcommand not one of mine" — InvalidSubcommand /
            // UnknownArgument — we return Unmatched so the caller's
            // own clap parser (umbra-cli's built-in subcommands like
            // serve / migrate / dev) gets a chance to match. Without
            // this, `cargo run -- dev` errors out at this layer
            // because plugin-dispatch claims authority over argv but
            // doesn't know what `dev` is, and `dev` is a built-in.
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                    Ok(DispatchOutcome::Help(e.render().to_string()))
                }
                clap::error::ErrorKind::InvalidSubcommand
                | clap::error::ErrorKind::UnknownArgument => Ok(DispatchOutcome::Unmatched),
                _ => Err(Box::new(e)),
            };
        }
    };

    let (name, sub_matches) = match matches.subcommand() {
        Some((n, m)) => (n.to_string(), m.clone()),
        None => return Ok(DispatchOutcome::Unmatched),
    };

    for (cmd_name, cmd) in &commands {
        if cmd_name == &name {
            cmd.run(&sub_matches).await?;
            return Ok(DispatchOutcome::Matched(name));
        }
    }
    Ok(DispatchOutcome::Unmatched)
}

/// Collect every plugin-contributed command as `(name, about)` pairs.
///
/// This is the plugin half of the unified help catalog the CLI prints
/// on `umbra help`, `umbra --help`, and `umbra <unknown>`. The other
/// half — the framework's built-in subcommands (`serve` / `migrate` /
/// …) — is collected in `umbra-cli` from the derived clap `Command`,
/// then merged with this list by [`render_help`].
///
/// Duplicate names across plugins are dropped (first-registered wins),
/// mirroring [`dispatch`]'s own dedup so the listing matches what would
/// actually run. A command whose `clap::Command` carries no `about`
/// still appears (with `None` description); the CLI renders a dash for
/// it and emits a `debug!` nudging the plugin author to add help text.
pub fn command_catalog(plugins: &[Box<dyn Plugin>]) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for plugin in plugins {
        for cmd in plugin.commands() {
            let clap_cmd = cmd.command();
            let name = clap_cmd.get_name().to_string();
            if !seen.insert(name.clone()) {
                continue;
            }
            let about = clap_cmd.get_about().map(|s| s.to_string());
            if about.is_none() {
                tracing::debug!(
                    target: "umbra::cli",
                    "plugin command `{name}` (from `{}`) has no `about`; \
                     it lists with a blank description. Add `.about(...)` so \
                     users can discover what it does.",
                    plugin.name()
                );
            }
            out.push((name, about));
        }
    }
    out
}

/// Render the unified command listing shown on help / unknown-command.
///
/// `catalog` is the merged `(name, about)` set — built-in subcommands
/// plus every plugin-contributed command. Entries are sorted by name
/// and deduplicated (first occurrence wins, so callers should place the
/// built-ins first to let them win a name clash). Descriptions are
/// padded into an aligned column; a command with no `about` shows a
/// dash.
///
/// The output is a complete help screen (header, usage, command table,
/// footer hint) ready to print to stdout (for `help`/`--help`) or
/// stderr (after an `error: unknown command` line).
pub fn render_help(catalog: &[(String, Option<String>)]) -> String {
    // Dedup by name, preserving order so built-ins (passed first) win.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut rows: Vec<(&str, &str)> = Vec::new();
    for (name, about) in catalog {
        if !seen.insert(name.as_str()) {
            continue;
        }
        let desc = about.as_deref().map(str::trim).unwrap_or("");
        rows.push((name.as_str(), desc));
    }
    rows.sort_by(|a, b| a.0.cmp(b.0));

    let width = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);

    let mut s = String::new();
    s.push_str("umbra — manage your umbra app\n\n");
    s.push_str("Usage: umbra <command> [options]\n\n");
    s.push_str("Commands:\n");
    for (name, desc) in &rows {
        let desc = if desc.is_empty() { "-" } else { desc };
        // First line of a multi-line `about` is the summary.
        let summary = desc.lines().next().unwrap_or("-");
        s.push_str(&format!("  {name:<width$}  {summary}\n"));
    }
    s.push('\n');
    s.push_str("Run `umbra <command> --help` for command-specific help.\n");
    s
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::plugin::Plugin;

    struct Counter(Arc<AtomicUsize>);

    #[async_trait]
    impl PluginCommand for Counter {
        fn command(&self) -> clap::Command {
            clap::Command::new("count").about("Increment a counter")
        }
        async fn run(&self, _matches: &ArgMatches) -> Result<(), CliError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct OnePlugin {
        name: &'static str,
        cmd: Box<dyn Fn() -> Box<dyn PluginCommand> + Send + Sync>,
    }

    impl Plugin for OnePlugin {
        fn name(&self) -> &'static str {
            self.name
        }
        fn commands(&self) -> Vec<Box<dyn PluginCommand>> {
            vec![(self.cmd)()]
        }
    }

    #[tokio::test]
    async fn empty_plugin_list_is_unmatched() {
        let plugins: Vec<Box<dyn Plugin>> = Vec::new();
        let out = dispatch(&plugins, ["argv0"]).await.unwrap();
        assert!(matches!(out, DispatchOutcome::Unmatched));
    }

    #[tokio::test]
    async fn matched_command_runs_its_handler() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(OnePlugin {
            name: "one",
            cmd: Box::new(move || Box::new(Counter(c.clone()))),
        })];
        let out = dispatch(&plugins, ["argv0", "count"]).await.unwrap();
        assert!(matches!(out, DispatchOutcome::Matched(name) if name == "count"));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn duplicate_command_name_across_plugins_is_dropped() {
        let counter_a = Arc::new(AtomicUsize::new(0));
        let counter_b = Arc::new(AtomicUsize::new(0));
        let ca = counter_a.clone();
        let cb = counter_b.clone();
        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(OnePlugin {
                name: "first",
                cmd: Box::new(move || Box::new(Counter(ca.clone()))),
            }),
            Box::new(OnePlugin {
                name: "second",
                cmd: Box::new(move || Box::new(Counter(cb.clone()))),
            }),
        ];
        let out = dispatch(&plugins, ["argv0", "count"]).await.unwrap();
        assert!(matches!(out, DispatchOutcome::Matched(_)));
        // The FIRST-registered plugin's command wins.
        assert_eq!(counter_a.load(Ordering::SeqCst), 1);
        assert_eq!(counter_b.load(Ordering::SeqCst), 0);
    }

    struct NoAboutCmd;

    #[async_trait]
    impl PluginCommand for NoAboutCmd {
        fn command(&self) -> clap::Command {
            // Deliberately no `.about(...)` — exercises the blank-desc path.
            clap::Command::new("tasks-worker")
        }
        async fn run(&self, _matches: &ArgMatches) -> Result<(), CliError> {
            Ok(())
        }
    }

    struct AboutCmd;

    #[async_trait]
    impl PluginCommand for AboutCmd {
        fn command(&self) -> clap::Command {
            clap::Command::new("tasks-worker").about("Run the task worker")
        }
        async fn run(&self, _matches: &ArgMatches) -> Result<(), CliError> {
            Ok(())
        }
    }

    fn plugin_with(cmd: fn() -> Box<dyn PluginCommand>) -> Box<dyn Plugin> {
        Box::new(OnePlugin {
            name: "tasks",
            cmd: Box::new(cmd),
        })
    }

    #[test]
    fn command_catalog_collects_name_and_about() {
        let plugins: Vec<Box<dyn Plugin>> = vec![plugin_with(|| Box::new(AboutCmd))];
        let cat = command_catalog(&plugins);
        assert_eq!(cat.len(), 1);
        assert_eq!(cat[0].0, "tasks-worker");
        assert_eq!(cat[0].1.as_deref(), Some("Run the task worker"));
    }

    #[test]
    fn command_catalog_lists_command_without_about_as_none() {
        let plugins: Vec<Box<dyn Plugin>> = vec![plugin_with(|| Box::new(NoAboutCmd))];
        let cat = command_catalog(&plugins);
        assert_eq!(cat.len(), 1);
        assert_eq!(cat[0].0, "tasks-worker");
        assert_eq!(cat[0].1, None);
    }

    #[test]
    fn render_help_aligns_and_shows_dash_for_blank() {
        // A built-in-style entry, a plugin entry with about, one without.
        let catalog = vec![
            (
                "migrate".to_string(),
                Some("Apply pending migrations".to_string()),
            ),
            (
                "tasks-worker".to_string(),
                Some("Run the task worker".to_string()),
            ),
            ("blank".to_string(), None),
        ];
        let out = render_help(&catalog);

        // Both descriptions present.
        assert!(
            out.contains("Apply pending migrations"),
            "missing built-in desc:\n{out}"
        );
        assert!(
            out.contains("Run the task worker"),
            "missing plugin desc:\n{out}"
        );
        // Blank-about command shows a dash.
        assert!(
            out.contains("blank") && out.contains(" -\n"),
            "missing dash for blank:\n{out}"
        );
        // Column alignment: the longest name is `tasks-worker` (12). The
        // shorter `migrate` row pads its name out to the same column, so
        // its description starts at the same offset.
        let worker_line = out.lines().find(|l| l.contains("tasks-worker")).unwrap();
        let migrate_line = out.lines().find(|l| l.contains("migrate")).unwrap();
        let worker_desc_col = worker_line.find("Run the task worker").unwrap();
        let migrate_desc_col = migrate_line.find("Apply pending migrations").unwrap();
        assert_eq!(
            worker_desc_col, migrate_desc_col,
            "descriptions not column-aligned:\n{out}"
        );
        // Sorted by name: blank < migrate < tasks-worker.
        let bi = out.find("\n  blank").unwrap();
        let mi = out.find("\n  migrate").unwrap();
        let ti = out.find("\n  tasks-worker").unwrap();
        assert!(bi < mi && mi < ti, "commands not sorted by name:\n{out}");
    }

    #[test]
    fn render_help_dedups_first_wins() {
        // Built-in `migrate` placed first should win over a plugin that
        // also registers `migrate` with a different description.
        let catalog = vec![
            (
                "migrate".to_string(),
                Some("Apply pending migrations".to_string()),
            ),
            ("migrate".to_string(), Some("a plugin override".to_string())),
        ];
        let out = render_help(&catalog);
        assert!(out.contains("Apply pending migrations"), "{out}");
        assert!(!out.contains("a plugin override"), "{out}");
    }

    #[tokio::test]
    async fn help_request_returns_help_outcome() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(OnePlugin {
            name: "one",
            cmd: Box::new(move || Box::new(Counter(c.clone()))),
        })];
        let out = dispatch(&plugins, ["argv0", "--help"]).await.unwrap();
        assert!(
            matches!(out, DispatchOutcome::Help(text) if text.contains("count")),
            "expected Help with subcommand listed"
        );
        // Handler did NOT run on --help.
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
}
