//! Smoke test for `umbral_cli::dispatch` — proves that argv routes
//! through plugin-contributed `PluginCommand` impls before the
//! built-in `serve` / `migrate` / etc. clap parser ever sees argv.
//!
//! This was a real shipped bug: gap 3's `umbral_cli::dispatch(app)`
//! had a hardcoded match against a fixed `Command` enum and never
//! consulted `Plugin::commands()`. `cargo run -- createsuperuser`
//! either errored with "no such subcommand" or — when callers
//! bypassed `dispatch` entirely with `app.serve(...)` — silently
//! started the server. Both paths were caught by the same shipping
//! review; this test pins the fix so future refactors of `dispatch`
//! that drop the plugin-routing step fail loudly.
//!
//! The strategy: build a tiny `Plugin` whose `commands()` returns one
//! command that flips an atomic flag in its `run`. Build a real
//! `App` (using an in-memory SQLite pool and figment defaults — the
//! same shape `tests/plugin_contract.rs` uses), then call
//! `dispatch_with_argv(app, [bin, smoke-cmd])` and assert the flag
//! flipped.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use clap::{ArgMatches, Command as ClapCommand};
use umbral::{App, Settings};
use umbral_core::cli::{CliError, PluginCommand};
use umbral_core::plugin::Plugin;

/// Fake `PluginCommand` that flips `fired` to true the first time its
/// `run` method is called. The atomic lets the test assert the
/// command really executed (not just matched).
struct SmokeCommand {
    fired: Arc<AtomicBool>,
}

#[async_trait]
impl PluginCommand for SmokeCommand {
    fn command(&self) -> ClapCommand {
        ClapCommand::new("smoke-cmd")
            .about("dispatch_smoke test fixture - flips an atomic when invoked")
    }

    async fn run(&self, _matches: &ArgMatches) -> Result<(), CliError> {
        self.fired.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Fake plugin that contributes one `SmokeCommand`. The `Arc<AtomicBool>`
/// is shared with the test body so the test can read whether `run` fired.
struct SmokePlugin {
    fired: Arc<AtomicBool>,
}

impl Plugin for SmokePlugin {
    fn name(&self) -> &'static str {
        "smoke"
    }

    fn commands(&self) -> Vec<Box<dyn PluginCommand>> {
        vec![Box::new(SmokeCommand {
            fired: self.fired.clone(),
        })]
    }
}

#[tokio::test]
async fn dispatch_routes_argv_to_plugin_contributed_commands() {
    // 1. Build an App with one plugin that contributes "smoke-cmd".
    let settings = Settings::from_env().expect("figment defaults always load");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let fired = Arc::new(AtomicBool::new(false));
    let plugin = SmokePlugin {
        fired: fired.clone(),
    };

    // `build_deferred` wires without firing `on_ready`; `dispatch_with_argv`
    // fires it iff argv warrants it (gaps3 #41). `smoke-cmd` runs against a live
    // app, so the hooks DO fire here.
    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(plugin)
        .build_deferred()
        .expect("App::build_deferred should succeed with figment defaults");

    // 2. Synthetic argv as if the user ran `bin smoke-cmd`.
    let argv: Vec<std::ffi::OsString> = vec!["test-binary".into(), "smoke-cmd".into()];

    // 3. Dispatch. If `umbral_cli::dispatch_with_argv` routes through
    //    `umbral_core::cli::dispatch` (the fix), the plugin's command
    //    matches and `run` fires. If it doesn't (the original bug),
    //    argv falls through to clap parsing where "smoke-cmd" is not
    //    a known built-in and the call errors.
    let result = umbral_cli::dispatch_with_argv(app, argv).await;

    // 4. Assert: no error AND the command actually ran.
    assert!(
        result.is_ok(),
        "dispatch returned an error - plugin command was probably not routed: {:?}",
        result.err()
    );
    assert!(
        fired.load(Ordering::SeqCst),
        "SmokeCommand::run did not fire - dispatch reached the built-in subcommand parser without consulting plugins. The wire between umbral-cli and umbral-core's cli::dispatch is broken.",
    );
}
