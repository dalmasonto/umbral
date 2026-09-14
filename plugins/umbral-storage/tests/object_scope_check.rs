//! `security.object_scope` boot check for storage media routes (IDOR design
//! spec, gaps5 #101 / tf#322).
//!
//! Behavioral over the real public path: build a `StoragePlugin` through its
//! builder, ask for `system_checks()`, and run the returned closure through
//! `run_all` with a `CheckContext` — the call `App::build` makes in phase 4.
//! The check is pure over the media-side config, so a `CheckContext` shell is
//! enough (the media dir need not exist for the check to fire).

use umbral::backend::SqliteBackend;
use umbral::check::{CheckContext, Severity, run_all};
use umbral::plugin::Plugin;
use umbral_storage::StoragePlugin;

fn findings_for(plugin: &StoragePlugin, strict: bool) -> Vec<(Severity, String)> {
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
fn ungated_media_route_warns() {
    let plugin = StoragePlugin::new().media("/media", "./media");
    let hits = findings_for(&plugin, false);
    assert_eq!(hits.len(), 1, "expected one finding; got {hits:#?}");
    assert_eq!(hits[0].0, Severity::Warning);
    assert!(
        hits[0].1.contains("/media"),
        "finding must name the mount; got {:?}",
        hits[0].1
    );
}

#[test]
fn no_media_side_no_finding() {
    // A static-only (or empty) plugin has no media route → no IDOR surface.
    let plugin = StoragePlugin::new();
    assert!(
        findings_for(&plugin, false).is_empty(),
        "no media mount means nothing to flag"
    );
}

#[test]
fn media_access_owner_clears_the_warning() {
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_access_owner();
    assert!(
        findings_for(&plugin, false).is_empty(),
        "an owner gate must clear the warning"
    );
}

#[test]
fn media_signed_urls_clears_the_warning() {
    // Signed URLs are a gate too — the upgrade over the old gaps4 #17 warning,
    // which only counted the closure gate.
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_signed_urls();
    assert!(
        findings_for(&plugin, false).is_empty(),
        "a signed-URL requirement is a gate and must clear the warning"
    );
}

#[test]
fn media_public_clears_the_warning() {
    let plugin = StoragePlugin::new()
        .media("/media", "./media")
        .media_public();
    assert!(
        findings_for(&plugin, false).is_empty(),
        "an explicit media_public() opt-out must clear the warning"
    );
}

#[test]
fn strict_mode_escalates_to_error() {
    let plugin = StoragePlugin::new().media("/media", "./media");
    let hits = findings_for(&plugin, true);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].0,
        Severity::Error,
        "strict_object_scope escalates the finding to Error"
    );
}
