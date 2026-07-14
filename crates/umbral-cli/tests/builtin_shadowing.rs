//! A registered command can NEVER shadow a framework built-in.
//!
//! Dispatch tries app + plugin commands *before* the built-in clap parser. So a
//! command named `migrate` did not collide loudly — it quietly took over.
//! `cargo run -- migrate` ran the user's command, the real migrate never
//! executed, and the deploy shipped an un-migrated schema with exit code 0.
//! Nobody finds that until production.
//!
//! `umbral startcommand` refuses the name, but that check only guards the
//! scaffolder: a hand-written `.command(...)` or any third-party plugin walked
//! straight past it. The enforcement therefore lives in `collect_commands`,
//! where every path to argv has to go through it.
//!
//! Found by the pre-0.0.10 review sweep.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use umbral::cli::{CliError, PluginCommand, clap};
use umbral::{App, Settings};

/// A command that impersonates the built-in `migrate`.
struct ImpostorMigrate {
    fired: Arc<AtomicBool>,
}

#[umbral::async_trait]
impl PluginCommand for ImpostorMigrate {
    fn command(&self) -> clap::Command {
        clap::Command::new("migrate").about("definitely not the real migrate")
    }
    async fn run(&self, _m: &clap::ArgMatches) -> Result<(), CliError> {
        self.fired.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn an_app_command_named_migrate_never_runs() {
    let settings = Settings::from_env().expect("figment defaults always load");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let fired = Arc::new(AtomicBool::new(false));

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .command(ImpostorMigrate {
            fired: fired.clone(),
        })
        .build_deferred()
        .expect("build_deferred with figment defaults");

    // The impostor is not in the catalog dispatch would route through...
    let builtins = umbral_cli::builtin_command_names();
    let reserved: Vec<&str> = builtins.iter().map(String::as_str).collect();
    let catalog =
        umbral::cli::command_catalog_with_app_commands(app.commands(), app.plugins(), &reserved);
    assert!(
        !catalog.iter().any(|(name, _)| name == "migrate"),
        "the impostor is still listed as a runnable command: {catalog:?}"
    );

    // ...and dispatching `migrate` does not run it. It falls through to the
    // built-in, which here fails on the in-memory-database guard — an ERROR is
    // the correct outcome and proves the real migrate got argv. The bug would
    // have been a clean `Ok(())` with `fired == true`.
    let argv: Vec<std::ffi::OsString> = vec!["test-binary".into(), "migrate".into()];
    let result = umbral_cli::dispatch_with_argv(app, argv).await;

    assert!(
        !fired.load(Ordering::SeqCst),
        "a command named `migrate` hijacked the built-in — a deploy would apply \
         zero migrations and report success"
    );
    assert!(
        result.is_err(),
        "expected the REAL migrate to run and refuse the in-memory database; \
         got Ok, which means nothing migrated and nothing complained"
    );
}
