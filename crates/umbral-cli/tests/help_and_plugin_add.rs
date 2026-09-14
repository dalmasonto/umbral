//! Unit tests for the CLI's help routing and `plugin add` resolution.
//!
//! These exercise the *pure* seams — argv classification, the static help
//! catalog, and plugin-name resolution — so the cargo-spawning paths
//! (`plugin add` actually shelling out, `--help` forwarding into a project)
//! are tested at the function boundary without a live cargo or project.

use umbral_cli::{PluginAddPlan, plan_plugin_add, render_static_help, static_catalog, wants_help};

// ---- wants_help: top-level help classification (#395) ----

#[test]
fn wants_help_recognizes_top_level_help_forms() {
    assert!(wants_help(&["help".into()]));
    assert!(wants_help(&["--help".into()]));
    assert!(wants_help(&["-h".into()]));
    // `umbral -- help` — tolerate a leading `--` separator.
    assert!(wants_help(&["--".into(), "help".into()]));
}

#[test]
fn wants_help_ignores_subcommand_specific_help() {
    // `umbral migrate --help` is command-specific help, NOT the top-level
    // catalog — it must fall through to clap so the command's flags render.
    assert!(!wants_help(&["migrate".into(), "--help".into()]));
    // A bare subcommand is not a help request.
    assert!(!wants_help(&["startproject".into()]));
    // No args is not a help request here (the binary decides its own default).
    assert!(!wants_help(&[]));
    // Version is not help.
    assert!(!wants_help(&["--version".into()]));
}

// ---- static_catalog / render_static_help: help outside a project (#395) ----

#[test]
fn static_catalog_includes_builtins_and_scaffolders() {
    let names: Vec<String> = static_catalog().into_iter().map(|(n, _)| n).collect();
    for expected in [
        "serve",
        "migrate",
        "makemigrations",
        "inspectdb",
        "startproject",
        "startplugin",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "static catalog missing `{expected}`: {names:?}"
        );
    }
}

#[test]
fn render_static_help_lists_management_commands_and_notes_plugins() {
    let out = render_static_help();
    // The whole point of #395: the full built-in surface, not "a few commands".
    assert!(out.contains("migrate"), "missing migrate:\n{out}");
    assert!(out.contains("serve"), "missing serve:\n{out}");
    assert!(out.contains("startproject"), "missing startproject:\n{out}");
    // A note that plugin-contributed commands appear inside a project.
    assert!(
        out.to_lowercase().contains("plugin"),
        "missing plugin-commands note:\n{out}"
    );
    // Version in the header — the CLI crate version.
    assert!(
        out.contains(env!("CARGO_PKG_VERSION")),
        "missing version in header:\n{out}"
    );
}

// ---- plan_plugin_add: name resolution (plugin add) ----

#[test]
fn plugin_add_resolves_short_name_to_crate_and_struct() {
    match plan_plugin_add("auth") {
        PluginAddPlan::Known {
            krate, struct_name, ..
        } => {
            assert_eq!(krate, "umbral-auth");
            assert_eq!(struct_name, "AuthPlugin");
        }
        other => panic!("expected Known, got {other:?}"),
    }
}

#[test]
fn plugin_add_accepts_full_crate_name() {
    match plan_plugin_add("umbral-sessions") {
        PluginAddPlan::Known {
            krate, struct_name, ..
        } => {
            assert_eq!(krate, "umbral-sessions");
            assert_eq!(struct_name, "SessionsPlugin");
        }
        other => panic!("expected Known, got {other:?}"),
    }
}

#[test]
fn plugin_add_handles_irregular_casing() {
    for (short, expected) in [
        ("oauth", "OAuthPlugin"),
        ("openapi", "OpenApiPlugin"),
        ("rls", "RlsPlugin"),
        ("livereload", "LiveReloadPlugin"),
        ("graphql", "GraphqlPlugin"),
    ] {
        match plan_plugin_add(short) {
            PluginAddPlan::Known { struct_name, .. } => {
                assert_eq!(struct_name, expected, "wrong struct for `{short}`")
            }
            other => panic!("expected Known for `{short}`, got {other:?}"),
        }
    }
}

#[test]
fn plugin_add_unknown_name_falls_through_to_passthrough() {
    match plan_plugin_add("some-random-crate") {
        PluginAddPlan::Passthrough { krate } => assert_eq!(krate, "some-random-crate"),
        other => panic!("expected Passthrough, got {other:?}"),
    }
}

#[test]
fn plugin_add_wiring_hint_uses_struct_default() {
    match plan_plugin_add("admin") {
        PluginAddPlan::Known { wiring, .. } => {
            assert!(wiring.contains(".plugin("), "wiring: {wiring}");
            assert!(
                wiring.contains("AdminPlugin::default()"),
                "wiring: {wiring}"
            );
        }
        other => panic!("expected Known, got {other:?}"),
    }
}
