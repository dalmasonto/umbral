//! `StaticPlugin` provides the `collectstatic` CLI command.
//!
//! `collectstatic` is NOT a built-in `umbra-cli` subcommand — it's
//! contributed by `StaticPlugin` via `Plugin::commands()`, so it only
//! exists when a project registers the plugin. These tests assert the
//! command name and flag parsing. The copy behaviour itself is covered
//! by `collect_into` tests in umbra-core; the published round-trip is
//! covered by umbra-core's `static_publish` test. Together those three
//! cover the end-to-end `cargo run -- collectstatic` path that's hard to
//! unit-test directly.

use umbra::plugin::Plugin;
use umbra_static::StaticPlugin;

#[test]
fn static_plugin_provides_collectstatic_command() {
    let plugin = StaticPlugin::new("/static", "./static");
    let commands = plugin.commands();
    assert_eq!(commands.len(), 1, "StaticPlugin contributes one command");

    let cmd = commands[0].command();
    assert_eq!(
        cmd.get_name(),
        "collectstatic",
        "the command the user types is `collectstatic`"
    );
}

#[test]
fn collectstatic_parses_clear_flag() {
    let plugin = StaticPlugin::new("/static", "./static");
    let cmd = plugin.commands()[0].command();

    // --clear parses to true.
    let matches = cmd
        .clone()
        .try_get_matches_from(["collectstatic", "--clear"])
        .expect("--clear is a valid flag");
    assert!(matches.get_flag("clear"));

    // Absent --clear defaults to false.
    let matches = cmd
        .try_get_matches_from(["collectstatic"])
        .expect("collectstatic parses with no args");
    assert!(!matches.get_flag("clear"));
}
