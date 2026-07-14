//! `AppBuilder::command` — the project's own management commands (gaps3 #81).
//!
//! Before this, `Plugin::commands()` was the only doorway to argv, so a
//! command that belonged to the *binary* (a one-off backfill, an import)
//! could only be added by inventing a plugin to carry it. `umbral
//! startcommand --in root` writes one of these, so what's asserted here is
//! the contract the scaffold depends on.
//!
//! The command below is deliberately written in the exact shape
//! `scaffold::render_command_file` emits — `use umbral::cli::{clap, ...}`,
//! `#[umbral::async_trait]`, the same four arg kinds. If the generated code
//! ever stops compiling, this test stops compiling with it, which is the
//! only way a string template gets a typechecker.
//!
//! One `App` per test *binary*: `App::build` publishes the settings into a
//! process-wide `OnceLock`, so a second build in the same process panics.
//! The zero-plugins case therefore lives in its own file
//! (`app_command_no_plugins.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use umbral::cli::{CliError, PluginCommand, clap};
use umbral::plugin::Plugin;
use umbral::{App, Settings};

/// What the command parsed out of argv, so the test can assert on it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Parsed {
    slug: String,
    limit: u64,
    tags: Vec<String>,
    dry_run: bool,
}

struct BackfillSlugsCommand {
    seen: Arc<std::sync::Mutex<Option<Parsed>>>,
}

#[umbral::async_trait]
impl PluginCommand for BackfillSlugsCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("backfill_slugs")
            .about("Fill in empty post slugs")
            .arg(clap::Arg::new("slug").required(true))
            .arg(
                clap::Arg::new("limit")
                    .long("limit")
                    .short('l')
                    .value_parser(clap::value_parser!(u64))
                    .default_value("25"),
            )
            .arg(
                clap::Arg::new("tag")
                    .long("tag")
                    .action(clap::ArgAction::Append),
            )
            .arg(
                clap::Arg::new("dry-run")
                    .long("dry-run")
                    .action(clap::ArgAction::SetTrue),
            )
    }

    async fn run(&self, matches: &clap::ArgMatches) -> Result<(), CliError> {
        *self.seen.lock().unwrap() = Some(Parsed {
            slug: matches.get_one::<String>("slug").unwrap().clone(),
            limit: *matches.get_one::<u64>("limit").unwrap(),
            tags: matches
                .get_many::<String>("tag")
                .map(|v| v.cloned().collect())
                .unwrap_or_default(),
            dry_run: matches.get_flag("dry-run"),
        });
        Ok(())
    }
}

struct PluginSideCommand {
    fired: Arc<AtomicBool>,
}

#[umbral::async_trait]
impl PluginCommand for PluginSideCommand {
    fn command(&self) -> clap::Command {
        // Same NAME as the app's command — the clash under test.
        clap::Command::new("backfill_slugs").about("the plugin's version")
    }
    async fn run(&self, _m: &clap::ArgMatches) -> Result<(), CliError> {
        self.fired.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct ClashingPlugin {
    fired: Arc<AtomicBool>,
}

impl Plugin for ClashingPlugin {
    fn name(&self) -> &'static str {
        "clashing"
    }
    fn commands(&self) -> Vec<Box<dyn PluginCommand>> {
        vec![Box::new(PluginSideCommand {
            fired: self.fired.clone(),
        })]
    }
}

/// Three claims, one App (see the module note on why one):
///
/// 1. A command registered on the builder — belonging to no plugin — runs,
///    and its positional / named / repeated / flag args arrive parsed.
/// 2. On a name clash the project's own command wins. It's the most specific
///    layer, and it's the code the user is looking at.
/// 3. It appears in the catalog `umbral help` renders. A command that can run
///    but doesn't list is a command nobody finds.
#[tokio::test]
async fn app_command_runs_wins_its_clash_and_lists() {
    let settings = Settings::from_env().expect("figment defaults always load");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let seen = Arc::new(std::sync::Mutex::new(None));
    let plugin_fired = Arc::new(AtomicBool::new(false));

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .command(BackfillSlugsCommand { seen: seen.clone() })
        .plugin(ClashingPlugin {
            fired: plugin_fired.clone(),
        })
        .build_deferred()
        .expect("build_deferred with figment defaults");

    // (3) Listed — checked before dispatch consumes the App.
    let catalog =
        umbral::cli::command_catalog_with_app_commands(app.commands(), app.plugins(), &[]);
    assert!(
        catalog.iter().any(|(name, about)| name == "backfill_slugs"
            && about.as_deref() == Some("Fill in empty post slugs")),
        "app command missing from the catalog `umbral help` renders: {catalog:?}"
    );

    // (1) Runs, with every arg kind parsed.
    let argv: Vec<std::ffi::OsString> = vec![
        "test-binary".into(),
        "backfill_slugs".into(),
        "hello-world".into(),
        "--limit".into(),
        "5".into(),
        "--tag".into(),
        "a".into(),
        "--tag".into(),
        "b".into(),
        "--dry-run".into(),
    ];
    umbral_cli::dispatch_with_argv(app, argv)
        .await
        .expect("dispatch should route to the app command");

    let parsed = seen.lock().unwrap().clone().expect("run() never fired");
    assert_eq!(
        parsed,
        Parsed {
            slug: "hello-world".to_string(),
            limit: 5,
            tags: vec!["a".to_string(), "b".to_string()],
            dry_run: true,
        }
    );

    // (2) The plugin's same-named command did NOT run.
    assert!(
        !plugin_fired.load(Ordering::SeqCst),
        "the plugin's command ran even though the app registered the same name"
    );
}
