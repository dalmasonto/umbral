//! The global `umbral` binary runs project-independent built-ins (e.g.
//! `maskkeygen`) directly, and forwards everything else — built-in project
//! commands AND custom dev commands like `seed_data` — to `cargo run -- <cmd>`.
//! `try_run_standalone` is the discriminator: `Some` = run here, `None` =
//! forward.

use umbral_cli::{STANDALONE_COMMANDS, try_run_standalone};

#[test]
fn maskkeygen_runs_standalone_without_a_project() {
    assert!(
        STANDALONE_COMMANDS.contains(&"maskkeygen"),
        "maskkeygen must be registered as a standalone command"
    );
    // `Some(..)` means the global binary handled it directly (no project /
    // `cargo run` needed). It prints a keypair to stdout as a side effect.
    let handled = try_run_standalone(&["maskkeygen".to_string()]);
    assert!(
        handled.is_some(),
        "maskkeygen must run standalone, not be forwarded to a project"
    );
    assert!(handled.unwrap().is_ok(), "maskkeygen succeeds");
}

#[test]
fn project_and_custom_commands_are_forwarded_not_run_standalone() {
    // Built-in commands that need the compiled App forward (None).
    for cmd in [
        "migrate",
        "serve",
        "makemigrations",
        "worker",
        "squashmigrations",
    ] {
        assert!(
            try_run_standalone(&[cmd.to_string()]).is_none(),
            "`{cmd}` needs the project and must forward, not run standalone"
        );
    }
    // A custom dev command (defined in the user's project) forwards too.
    assert!(
        try_run_standalone(&[
            "seed_data".to_string(),
            "--count".to_string(),
            "10".to_string()
        ])
        .is_none(),
        "a custom command like seed_data forwards to cargo run"
    );
    // No command at all → nothing to run standalone.
    assert!(try_run_standalone(&[]).is_none());
}
