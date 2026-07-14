//! An app with ZERO plugins still reaches its own commands (gaps3 #81).
//!
//! `dispatch_with_argv` short-circuits the whole command-dispatch step when
//! there's nothing to dispatch to. That guard used to read
//! `if !app.plugins().is_empty()`, which is precisely wrong for the case
//! `startcommand --in root` creates: a plugin-free project whose only
//! command is its own. It would have been unreachable, and the failure mode
//! is the worst kind — `cargo run -- my_command` reporting "unknown
//! command" for a command that is right there in main.rs.
//!
//! Lives in its own file because `App::build` publishes settings into a
//! process-wide `OnceLock`; a second build in the same test binary panics.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use umbral::cli::{CliError, PluginCommand, clap};
use umbral::{App, Settings};

struct ImportPricesCommand {
    fired: Arc<AtomicBool>,
}

#[umbral::async_trait]
impl PluginCommand for ImportPricesCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("import_prices").about("Load the price sheet")
    }
    async fn run(&self, _m: &clap::ArgMatches) -> Result<(), CliError> {
        self.fired.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn plugin_free_app_still_dispatches_its_own_command() {
    let settings = Settings::from_env().expect("figment defaults always load");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let fired = Arc::new(AtomicBool::new(false));

    // `.commands(vec![...])` — the plural form, which is what the generated
    // `main.rs` calls with `commands::all()`.
    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .commands(vec![Box::new(ImportPricesCommand {
            fired: fired.clone(),
        })])
        .build_deferred()
        .expect("build_deferred with figment defaults");

    assert!(
        app.plugins().is_empty(),
        "fixture must have no plugins for this to test what it claims"
    );

    let argv: Vec<std::ffi::OsString> = vec!["test-binary".into(), "import_prices".into()];
    umbral_cli::dispatch_with_argv(app, argv)
        .await
        .expect("dispatch should route to the app command");

    assert!(
        fired.load(Ordering::SeqCst),
        "a plugin-free app never reached its own command"
    );
}
