//! `security.object_scope` boot check for GraphQL mutations (IDOR design spec,
//! gaps5 #101 / tf#322).
//!
//! Behavioral over the real public path: build a `GraphqlPlugin` through its
//! builder, ask for `system_checks()`, and run the returned closure through
//! `run_all` with a `CheckContext` — the same call `App::build` makes. The
//! check is pure over the plugin's `mutable` / `owned_by` / ack lists (no DB,
//! no model registry), so a `CheckContext` shell is all that's needed.

use umbral::backend::SqliteBackend;
use umbral::check::{CheckContext, Severity, run_all};
use umbral::plugin::Plugin;
use umbral_graphql::GraphqlPlugin;

fn findings_for(plugin: &GraphqlPlugin, strict: bool) -> Vec<(Severity, String)> {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let ctx = CheckContext {
        backend: &SqliteBackend,
        settings: &settings,
        provides_storage: true,
        registered_plugin_names: &[],
        strict_object_scope: strict,
    };
    let checks = plugin.system_checks();
    run_all(&ctx, &checks)
        .into_iter()
        .filter(|f| f.check_id == "security.object_scope")
        .map(|f| (f.severity, f.message))
        .collect()
}

#[test]
fn mutable_model_without_owned_by_warns() {
    let plugin = GraphqlPlugin::new().expose("post").mutable("post");
    let hits = findings_for(&plugin, false);
    assert_eq!(hits.len(), 1, "expected one finding; got {hits:#?}");
    assert_eq!(hits[0].0, Severity::Warning);
    assert!(
        hits[0].1.contains("post"),
        "finding must name the model; got {:?}",
        hits[0].1
    );
}

#[test]
fn read_only_exposed_model_does_not_warn() {
    // Exposed but NOT mutable — a read surface is not the IDOR write surface
    // the check exists for, so nothing is flagged.
    let plugin = GraphqlPlugin::new().expose("post");
    assert!(
        findings_for(&plugin, false).is_empty(),
        "a read-only exposed model must not warn"
    );
}

#[test]
fn owned_by_clears_the_warning() {
    let plugin = GraphqlPlugin::new()
        .expose("post")
        .mutable("post")
        .owned_by("post", "author");
    assert!(
        findings_for(&plugin, false).is_empty(),
        "an owned_by row scope must clear the warning"
    );
}

#[test]
fn unscoped_ok_clears_the_warning() {
    let plugin = GraphqlPlugin::new()
        .expose("tag")
        .mutable("tag")
        .unscoped_ok("tag");
    assert!(
        findings_for(&plugin, false).is_empty(),
        "an unscoped_ok acknowledgement must clear the warning"
    );
}

#[test]
fn rls_backed_clears_the_warning() {
    let plugin = GraphqlPlugin::new()
        .expose("invoice")
        .mutable("invoice")
        .rls_backed("invoice");
    assert!(
        findings_for(&plugin, false).is_empty(),
        "an rls_backed acknowledgement must clear the warning"
    );
}

#[test]
fn strict_mode_escalates_to_error() {
    let plugin = GraphqlPlugin::new().expose("post").mutable("post");
    let hits = findings_for(&plugin, true);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].0,
        Severity::Error,
        "strict_object_scope escalates the finding to Error"
    );
}

#[test]
fn only_unscoped_mutable_models_are_flagged() {
    // Two mutable models: one scoped, one not. Only the unscoped one warns.
    let plugin = GraphqlPlugin::new()
        .expose("post")
        .mutable("post")
        .owned_by("post", "author")
        .expose("comment")
        .mutable("comment");
    let hits = findings_for(&plugin, false);
    assert_eq!(
        hits.len(),
        1,
        "only the unscoped model warns; got {hits:#?}"
    );
    assert!(hits[0].1.contains("comment"));
}
