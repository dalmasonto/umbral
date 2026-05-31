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
            // --help / --version / parse error all become an error
            // type. For --help we surface the rendered text so the
            // caller can print it; everything else propagates.
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                    Ok(DispatchOutcome::Help(e.render().to_string()))
                }
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
