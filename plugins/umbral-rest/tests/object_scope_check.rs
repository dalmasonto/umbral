//! `security.object_scope` boot check for REST resources (IDOR design spec,
//! gaps5 #101 / tf#322).
//!
//! Behavioral over the REAL public path: build a `RestPlugin` through its
//! builder exactly as an app would, ask it for `system_checks()`, and run the
//! returned closure through `run_all` with a `CheckContext` — the same call
//! `App::build` makes in phase 4. We assert on the findings the closure emits,
//! not on internal state.
//!
//! The check is pure over the plugin's configured resources (no DB, no model
//! registry, no `App::build`), so these tests need only a `CheckContext` shell.
//! The strict-mode → `BuildError::SystemCheckFailed` path is exercised end to
//! end in `object_scope_strict_build.rs`.

use umbral::backend::SqliteBackend;
use umbral::check::{CheckContext, Severity, run_all};
use umbral::plugin::Plugin;
use umbral_rest::{Action, ResourceConfig, RestPlugin};

/// Run a plugin's `system_checks()` and return the findings, at the given
/// strictness. `strict = false` is the default Warning posture; `true` is the
/// `UMBRAL_STRICT_OBJECT_SCOPE` escalation.
fn findings_for(plugin: &RestPlugin, strict: bool) -> Vec<(Severity, String)> {
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
fn write_enabled_unscoped_resource_warns() {
    // A resource with no `.views(...)` exposes every action (back-compat
    // default), so it is write-enabled; with no scope and no marker it is the
    // IDOR surface the check exists to flag.
    let plugin = RestPlugin::new().resource(ResourceConfig::new("order"));
    let hits = findings_for(&plugin, false);
    assert_eq!(hits.len(), 1, "expected exactly one finding; got {hits:#?}");
    assert_eq!(hits[0].0, Severity::Warning, "default severity is Warning");
    assert!(
        hits[0].1.contains("order"),
        "the finding must name the table; got {:?}",
        hits[0].1
    );
}

#[test]
fn owned_by_clears_the_warning() {
    let plugin = RestPlugin::new().resource(ResourceConfig::new("order").owned_by("owner_id"));
    assert!(
        findings_for(&plugin, false).is_empty(),
        "a scoped resource must not warn"
    );
}

#[test]
fn scope_writes_clears_the_warning() {
    // A write-only scope constrains exactly the write actions the check cares
    // about, so it counts as scoped too.
    let plugin =
        RestPlugin::new().resource(ResourceConfig::new("post").owned_by_for_writes("author_id"));
    assert!(
        findings_for(&plugin, false).is_empty(),
        "a write-only scope must clear the warning"
    );
}

#[test]
fn rls_backed_clears_the_warning() {
    let plugin = RestPlugin::new().resource(ResourceConfig::new("invoice").rls_backed());
    assert!(
        findings_for(&plugin, false).is_empty(),
        "an rls_backed() acknowledgement must clear the warning"
    );
}

#[test]
fn unscoped_ok_clears_the_warning() {
    let plugin = RestPlugin::new()
        .resource(ResourceConfig::new("changelog").unscoped_ok("public append-only feed"));
    assert!(
        findings_for(&plugin, false).is_empty(),
        "an unscoped_ok() acknowledgement must clear the warning"
    );
}

#[test]
fn read_only_resource_does_not_warn() {
    // `.views([List, Retrieve])` exposes NO write action → not write-enabled →
    // no IDOR surface, so no finding even without a scope.
    let plugin = RestPlugin::new()
        .resource(ResourceConfig::new("country").views([Action::List, Action::Retrieve]));
    assert!(
        findings_for(&plugin, false).is_empty(),
        "a read-only resource is not a write surface and must not warn"
    );
}

#[test]
fn a_write_action_in_views_still_warns() {
    // `.views([List, Create])` still exposes a write, so an unscoped resource
    // with that view set is flagged.
    let plugin = RestPlugin::new()
        .resource(ResourceConfig::new("order").views([Action::List, Action::Create]));
    assert_eq!(
        findings_for(&plugin, false).len(),
        1,
        "a view set containing Create is write-enabled and must warn"
    );
}

#[test]
fn strict_mode_escalates_to_error() {
    let plugin = RestPlugin::new().resource(ResourceConfig::new("order"));
    let hits = findings_for(&plugin, true);
    assert_eq!(hits.len(), 1, "still one finding under strict mode");
    assert_eq!(
        hits[0].0,
        Severity::Error,
        "strict_object_scope escalates the finding to Error"
    );
}

#[test]
fn repeated_resource_calls_dedupe_to_one_finding() {
    // Two `.resource("order")` calls (the additive-merge API) must not double
    // the finding.
    let plugin = RestPlugin::new()
        .resource(ResourceConfig::new("order"))
        .resource(ResourceConfig::new("order").hide("secret_note"));
    assert_eq!(
        findings_for(&plugin, false).len(),
        1,
        "the same table configured twice yields one finding, not two"
    );
}

#[test]
fn a_later_resource_call_can_add_the_scope() {
    // The check reads the fully-merged config: a scope added in a second
    // `.resource(...)` call for the same table clears the warning.
    let plugin = RestPlugin::new()
        .resource(ResourceConfig::new("order"))
        .resource(ResourceConfig::new("order").owned_by("owner_id"));
    assert!(
        findings_for(&plugin, false).is_empty(),
        "a scope added by a later .resource() call must clear the warning"
    );
}
