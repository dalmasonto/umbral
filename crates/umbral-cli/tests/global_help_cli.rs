//! End-to-end tests for the global `umbral` binary's top-level routing:
//! `--version`, unified `--help` / `help` / bare-invocation, and the
//! `plugin add` project guard.
//!
//! These spawn the real built binary (`CARGO_BIN_EXE_umbral`) so they cover
//! `main.rs`'s pre-clap interception and clap's version routing — the wiring
//! the pure-function tests in `help_and_plugin_add.rs` can't reach. Every
//! case runs in a fresh temp dir so no ancestor `Cargo.toml` is visible,
//! exercising the OUTSIDE-a-project paths deterministically (no cargo build,
//! no network).

use std::process::Command;

use tempfile::TempDir;

fn umbral() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_umbral"));
    // Run outside any project: a bare temp dir has no Cargo.toml ancestor.
    let tmp = TempDir::new().unwrap();
    c.current_dir(tmp.path());
    // Keep the TempDir alive for the child's lifetime by leaking it — the OS
    // cleans /tmp; the test process is short-lived. (Dropping it here would
    // delete the dir before the child runs.)
    std::mem::forget(tmp);
    c
}

#[test]
fn version_flag_prints_version_and_exits_zero() {
    let out = umbral().arg("--version").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "version missing from `--version` output: {stdout}"
    );
}

#[test]
fn help_flag_lists_full_builtin_catalog_outside_a_project() {
    let out = umbral().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The #395 fix: NOT just the four scaffold commands — the whole built-in
    // surface, grouped.
    for expected in [
        "startproject",
        "serve",
        "migrate",
        "makemigrations",
        "inspectdb",
    ] {
        assert!(
            stdout.contains(expected),
            "`{expected}` missing from top-level --help:\n{stdout}"
        );
    }
    // Version in the header.
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "{stdout}");
}

#[test]
fn help_subcommand_matches_help_flag_outside_a_project() {
    // `umbral help` and `umbral --help` must produce the same catalog.
    let via_sub = umbral().arg("help").output().unwrap();
    let via_flag = umbral().arg("--help").output().unwrap();
    assert!(via_sub.status.success() && via_flag.status.success());
    assert_eq!(
        String::from_utf8_lossy(&via_sub.stdout),
        String::from_utf8_lossy(&via_flag.stdout),
        "`umbral help` and `umbral --help` diverged"
    );
}

#[test]
fn bare_invocation_shows_help_outside_a_project() {
    let out = umbral().output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Usage:"),
        "bare `umbral` should show help:\n{stdout}"
    );
    assert!(stdout.contains("migrate"), "{stdout}");
}

#[test]
fn plugin_add_requires_a_project() {
    let out = umbral().args(["plugin", "add", "auth"]).output().unwrap();
    assert!(!out.status.success(), "should fail outside a Cargo project");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Cargo project"),
        "expected a project-required error, got:\n{stderr}"
    );
    // It must NOT have shelled out to `cargo add` before the guard.
    assert!(
        !stderr.contains("Adding")
            && !String::from_utf8_lossy(&out.stdout).contains("Running: cargo"),
        "guard should fire before running cargo add"
    );
}
