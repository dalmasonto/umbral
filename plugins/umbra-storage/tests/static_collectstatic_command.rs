//! `StoragePlugin` (static side) provides the `collectstatic` CLI command.
//!
//! Moved from umbra-static. `collectstatic` is contributed by the static
//! side via `Plugin::commands()`, so it only exists when a project
//! configures a static side.

use umbra::plugin::Plugin;
use umbra_storage::StoragePlugin;

#[test]
fn static_plugin_provides_collectstatic_command() {
    let plugin = StoragePlugin::new().static_files("/static", "./static");
    let commands = plugin.commands();
    assert_eq!(commands.len(), 1, "static side contributes one command");

    let cmd = commands[0].command();
    assert_eq!(
        cmd.get_name(),
        "collectstatic",
        "the command the user types is `collectstatic`"
    );
}

#[test]
fn collectstatic_parses_clear_flag() {
    let plugin = StoragePlugin::new().static_files("/static", "./static");
    let cmd = plugin.commands()[0].command();

    let matches = cmd
        .clone()
        .try_get_matches_from(["collectstatic", "--clear"])
        .expect("--clear is a valid flag");
    assert!(matches.get_flag("clear"));

    let matches = cmd
        .try_get_matches_from(["collectstatic"])
        .expect("collectstatic parses with no args");
    assert!(!matches.get_flag("clear"));
}
