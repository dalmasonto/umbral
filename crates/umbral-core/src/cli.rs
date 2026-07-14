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
//! `umbral-cli`. Plugins return a `clap::Command`, which carries help
//! text, arg validation, and subcommand groupings for free. The
//! dispatcher composes the per-plugin `Command` values under a
//! single parent so `umbral-cli <plugin-cmd>` works as one tree.
//!
//! ## Example
//!
//! ```ignore
//! use umbral::cli::{dispatch, CliError, PluginCommand};
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
//!             umbral_tasks::run_worker_once().await?;
//!         } else {
//!             // run_worker loops forever
//!             umbral_tasks::run_worker(Default::default()).await
//!         }
//!         Ok(())
//!     }
//! }
//! ```

use std::ffi::OsString;

use async_trait::async_trait;
use clap::ArgMatches;

use crate::plugin::Plugin;

/// Re-export of `clap`, the crate [`PluginCommand`] names in its own
/// public signature (`fn command(&self) -> clap::Command`).
///
/// A trait whose surface names a foreign type has to hand that type
/// out, or every implementor adds its own `clap = "4"` dependency and
/// gets to discover — at link time, via a type mismatch a page long —
/// that it resolved a different major version than the framework. Write
/// `use umbral::cli::clap;` and you are provably on the same clap the
/// dispatcher parses with.
pub use clap;

/// Error returned by a plugin command. Boxed so plugins can return
/// any concrete error type without forcing the trait into a
/// generic-over-E shape.
pub type CliError = Box<dyn std::error::Error + Send + Sync>;

/// One CLI subcommand contributed by a plugin.
#[async_trait]
pub trait PluginCommand: Send + Sync + 'static {
    /// The clap subcommand. `Command::get_name()` is the literal the
    /// user types after the program name (`umbral-cli <name>`).
    /// Long-form help, arg parsing, and subcommand grouping are all
    /// the plugin's to configure on the returned value.
    fn command(&self) -> clap::Command;

    /// Run the command. Called after clap has parsed args matching
    /// `self.command()`; `matches` is the per-subcommand
    /// `ArgMatches` (not the top-level one).
    async fn run(&self, matches: &ArgMatches) -> Result<(), CliError>;

    /// Whether this command needs a *live* application — pools open, schema
    /// migrated, every plugin's `on_ready` fired.
    ///
    /// Default `true`, which is right for almost everything: a command that
    /// touches data wants the app up.
    ///
    /// Return `false` for a command that only touches the filesystem — a code
    /// generator, a linter, a config dump. `on_ready` hooks seed content and
    /// backfill rows, so firing them for `startpermission` means a pure
    /// codegen command writes to the database, and on a fresh checkout it
    /// fails against tables `migrate` has not created yet — before writing the
    /// file it exists to write.
    ///
    /// The framework's own schema commands (`migrate`, `makemigrations`, …)
    /// are excluded by name in `umbral-cli`; that list structurally cannot
    /// know about a plugin's offline commands, which is why the plugin gets to
    /// declare it here.
    fn needs_ready(&self) -> bool {
        true
    }
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
    dispatch_with_app_commands(&[], plugins, &[], args).await
}

/// [`dispatch`], plus the commands the *project* registered directly on
/// its `App` via [`crate::app::AppBuilder::command`].
///
/// A project's own management command (`backfill_slugs`, `import_prices`)
/// belongs to no plugin — it belongs to the binary. Without this, the only
/// way to add one is to wrap it in a dummy plugin, which is a contract
/// smell: the plugin trait exists to package a *reusable* unit, not to be
/// the sole doorway to argv.
///
/// App commands are collected first, so on a name clash the project's own
/// command wins over a plugin's (most-specific layer wins) and a warning
/// names the plugin that lost.
pub async fn dispatch_with_app_commands<I, T>(
    app_commands: &[Box<dyn PluginCommand>],
    plugins: &[Box<dyn Plugin>],
    reserved: &[&str],
    args: I,
) -> Result<DispatchOutcome, CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    // A one-shot convenience. A caller that ALSO needs the catalog or a
    // readiness answer should build a `CommandSet` once and ask it all three
    // questions: collecting is what runs every plugin's command constructors,
    // builds their clap parsers, and prints the built-in-shadow warning — so
    // collecting per-question printed that warning per-question too.
    CommandSet::collect(app_commands, plugins, reserved)
        .dispatch(args)
        .await
}

/// One collected command, however it got here.
///
/// The app registers its commands once on the builder and the `App` owns
/// them for its lifetime, so those arrive as borrows. A plugin *builds* a
/// fresh `Box<dyn PluginCommand>` every time `Plugin::commands()` is
/// called, so those arrive owned. `run` takes `&self`, so neither side
/// needs to be cloned — this enum is just the seam that lets one list
/// hold both.
enum CommandHandle<'a> {
    Borrowed(&'a dyn PluginCommand),
    Owned(Box<dyn PluginCommand>),
}

impl CommandHandle<'_> {
    fn get(&self) -> &dyn PluginCommand {
        match self {
            Self::Borrowed(c) => *c,
            Self::Owned(c) => c.as_ref(),
        }
    }
}

/// The registered commands, collected once.
///
/// Collecting is not free and it is not idempotent-looking: it runs every
/// plugin's `commands()` constructor, builds a `clap::Command` per command
/// (help prose and all), and PRINTS the built-in-shadow warning. Doing that
/// two or three times per invocation — once for the readiness check, once to
/// dispatch, once more for the help catalog — meant a user with a shadowing
/// command saw the same scary warning twice, which reads like two problems.
///
/// So: collect once, then ask it questions.
pub struct CommandSet<'a> {
    entries: Vec<Entry<'a>>,
}

struct Entry<'a> {
    name: String,
    /// Built once, here. Every consumer reads it rather than rebuilding it.
    clap: clap::Command,
    handle: CommandHandle<'a>,
}

impl<'a> CommandSet<'a> {
    /// Collect the app's own commands followed by every plugin's, dropping any
    /// that shadow a framework built-in named in `reserved`.
    pub fn collect(
        app_commands: &'a [Box<dyn PluginCommand>],
        plugins: &'a [Box<dyn Plugin>],
        reserved: &[&str],
    ) -> Self {
        Self {
            entries: collect_commands(app_commands, plugins, reserved),
        }
    }

    /// Nothing registered — the caller handles everything itself.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Does the command named `name` need a live app? `None` if nothing
    /// registered that name (the caller's own built-in rules then decide).
    pub fn needs_ready(&self, name: &str) -> Option<bool> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.handle.get().needs_ready())
    }

    /// `(name, about)` for every registered command — the listing half of
    /// `umbral help`. Shares this collection with dispatch, so the help can
    /// never advertise a command that would not actually run.
    pub fn catalog(&self) -> Vec<(String, Option<String>)> {
        self.entries
            .iter()
            .map(|e| {
                let about = e.clap.get_about().map(|s| s.to_string());
                if about.is_none() {
                    tracing::debug!(
                        target: "umbral::cli",
                        "command `{}` has no `about`; it lists with a blank description. \
                         Add `.about(...)` so users can discover what it does.",
                        e.name,
                    );
                }
                (e.name.clone(), about)
            })
            .collect()
    }

    /// Route `args` to the matching command.
    pub async fn dispatch<I, T>(&self, args: I) -> Result<DispatchOutcome, CliError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        if self.entries.is_empty() {
            return Ok(DispatchOutcome::Unmatched);
        }

        let mut root = clap::Command::new("umbral")
            .about("umbral plugin subcommands")
            .disable_help_subcommand(true)
            .subcommand_required(false)
            .arg_required_else_help(false);
        for entry in &self.entries {
            root = root.subcommand(entry.clap.clone());
        }

        let owned: Vec<OsString> = args.into_iter().map(|t| t.into()).collect();
        let matches = match root.clone().try_get_matches_from(owned) {
            Ok(m) => m,
            Err(e) => {
                return match e.kind() {
                    clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayVersion => {
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

        for entry in &self.entries {
            if entry.name == name {
                entry.handle.get().run(&sub_matches).await?;
                return Ok(DispatchOutcome::Matched(name));
            }
        }
        Ok(DispatchOutcome::Unmatched)
    }
}

/// Collect the app's own commands followed by every plugin's, keyed by
/// the clap name and deduplicated (first-registered wins).
///
/// The single place the precedence rule lives: app commands are pushed
/// before plugin commands, so a project can deliberately shadow a
/// plugin's command with its own. Both [`dispatch_with_app_commands`]
/// and [`command_catalog_with_app_commands`] route through here, which
/// is what keeps the help listing honest about what would actually run.
fn collect_commands<'a>(
    app_commands: &'a [Box<dyn PluginCommand>],
    plugins: &'a [Box<dyn Plugin>],
    reserved: &[&str],
) -> Vec<Entry<'a>> {
    let mut commands: Vec<Entry<'a>> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // A command whose name is a framework built-in is DROPPED, not run.
    //
    // The dispatcher tries these commands before the built-in parser, so
    // without this a command named `migrate` doesn't collide loudly — it
    // quietly takes over, `cargo run -- migrate` runs the wrong thing, and the
    // deploy ships an un-migrated schema with a zero exit code. Nobody finds
    // that until production. The built-in wins; the shadow is refused out loud.
    let shadow = |name: &str, source: &str| {
        if !reserved.contains(&name) {
            return false;
        }
        // eprintln, not just tracing: this must be visible in a plain
        // `cargo run` with no subscriber configured, because the thing it is
        // warning about is that a framework command has stopped working.
        eprintln!(
            "warning: {source} registers a command named `{name}`, which is a framework \
             built-in. The built-in wins and the registered one is IGNORED — rename it. \
             (Without this, `{name}` would silently run your command instead of the \
             framework's.)"
        );
        tracing::warn!(
            target: "umbral::cli",
            "{source} command `{name}` shadows a framework built-in; ignoring it",
        );
        true
    };

    // Each `clap::Command` is built ONCE here and carried on the entry. It used
    // to be built to read `.get_name()` and then thrown away, and rebuilt by
    // every consumer — three times per invocation for prose that was discarded.
    for cmd in app_commands {
        let clap = cmd.command();
        let name = clap.get_name().to_string();
        if shadow(&name, "the app") {
            continue;
        }
        if !seen.insert(name.clone()) {
            tracing::warn!(
                target: "umbral::cli",
                "app command `{name}` is registered twice on the App builder; \
                 ignoring the second",
            );
            continue;
        }
        commands.push(Entry {
            name,
            clap,
            handle: CommandHandle::Borrowed(cmd.as_ref()),
        });
    }
    for plugin in plugins {
        for cmd in plugin.commands() {
            let clap = cmd.command();
            let name = clap.get_name().to_string();
            if shadow(&name, &format!("plugin `{}`", plugin.name())) {
                continue;
            }
            if !seen.insert(name.clone()) {
                tracing::warn!(
                    target: "umbral::cli",
                    "duplicate command `{name}` from plugin `{}`; ignoring (an \
                     earlier plugin — or the app itself — registered it first)",
                    plugin.name()
                );
                continue;
            }
            commands.push(Entry {
                name,
                clap,
                handle: CommandHandle::Owned(cmd),
            });
        }
    }
    commands
}

/// Does the command named `name` need a live app (pools, migrated schema,
/// `on_ready` fired)?
///
/// `None` means no app or plugin registered that name — the framework binary's
/// own built-in list decides. `Some(false)` is a command that declared itself
/// offline via [`PluginCommand::needs_ready`], e.g. a code generator.
pub fn command_needs_ready(
    app_commands: &[Box<dyn PluginCommand>],
    plugins: &[Box<dyn Plugin>],
    name: &str,
    reserved: &[&str],
) -> Option<bool> {
    CommandSet::collect(app_commands, plugins, reserved).needs_ready(name)
}

/// Collect every plugin-contributed command as `(name, about)` pairs.
///
/// This is the plugin half of the unified help catalog the CLI prints
/// on `umbral help`, `umbral --help`, and `umbral <unknown>`. The other
/// half — the framework's built-in subcommands (`serve` / `migrate` /
/// …) — is collected in `umbral-cli` from the derived clap `Command`,
/// then merged with this list by [`render_help`].
///
/// Duplicate names across plugins are dropped (first-registered wins),
/// mirroring [`dispatch`]'s own dedup so the listing matches what would
/// actually run. A command whose `clap::Command` carries no `about`
/// still appears (with `None` description); the CLI renders a dash for
/// it and emits a `debug!` nudging the plugin author to add help text.
pub fn command_catalog(plugins: &[Box<dyn Plugin>]) -> Vec<(String, Option<String>)> {
    command_catalog_with_app_commands(&[], plugins, &[])
}

/// [`command_catalog`], plus the project's own `AppBuilder::command`
/// registrations — the listing half of [`dispatch_with_app_commands`].
///
/// Shares [`collect_commands`] with the dispatcher, so a command that
/// lost a name clash is absent from the help for the same reason it
/// would never have run: it is not in the collected set.
pub fn command_catalog_with_app_commands(
    app_commands: &[Box<dyn PluginCommand>],
    plugins: &[Box<dyn Plugin>],
    reserved: &[&str],
) -> Vec<(String, Option<String>)> {
    CommandSet::collect(app_commands, plugins, reserved).catalog()
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
    s.push_str("umbral — manage your umbral app\n\n");
    s.push_str("Usage: umbral <command> [options]\n\n");
    s.push_str("Commands:\n");
    for (name, desc) in &rows {
        let desc = if desc.is_empty() { "-" } else { desc };
        // First line of a multi-line `about` is the summary.
        let summary = desc.lines().next().unwrap_or("-");
        s.push_str(&format!("  {name:<width$}  {summary}\n"));
    }
    s.push('\n');
    s.push_str("Run `umbral <command> --help` for command-specific help.\n");
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
