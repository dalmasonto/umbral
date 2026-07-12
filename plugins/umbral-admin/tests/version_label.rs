//! The admin's version string (gaps3 #67).
//!
//! `base.html` and `login.html` hardcoded the literal `v0.0.1`. It stopped being true at
//! 0.0.2 and would have gone on being wrong forever, because nothing tied it to anything.
//! A hardcoded version is not a value — it is a claim that nobody is checking.
//!
//! Three behaviours, and the third is the one that actually matters to an operator: an
//! app should be able to advertise ITS version rather than the framework's, or advertise
//! none at all.

use umbral_admin::AdminPlugin;
use umbral_admin::branding::umbral_version_label;

/// The default is umbral's REAL version, derived from the crate — so it cannot rot.
#[test]
fn the_default_label_tracks_the_real_crate_version() {
    let label = umbral_version_label();
    assert_eq!(label, format!("umbral v{}", env!("CARGO_PKG_VERSION")));

    // The literal that was in the templates. If the workspace is ever genuinely at 0.0.1
    // again this assert is wrong, but it is the exact failure we are guarding against:
    // a version string that says 0.0.1 while the crate says something else.
    assert!(
        !label.contains("v0.0.1") || env!("CARGO_PKG_VERSION") == "0.0.1",
        "the label claims v0.0.1 while the crate is at {}",
        env!("CARGO_PKG_VERSION")
    );
}

/// `show_version(false)` hides it. An admin is a private surface, and telling every
/// visitor which framework version you run is free reconnaissance against a CVE list.
#[test]
fn show_version_false_hides_it() {
    let plugin = AdminPlugin::default().show_version(false);
    assert_eq!(
        plugin.branding_for_tests().version_label,
        None,
        "show_version(false) must clear the label entirely"
    );
}

/// The operator of a shop almost certainly does not want their staff login page
/// advertising the framework. They can advertise themselves instead.
#[test]
fn version_shows_the_apps_own_string_instead_of_umbrals() {
    let plugin = AdminPlugin::default().version("MyShop v2.3.1");
    assert_eq!(
        plugin.branding_for_tests().version_label.as_deref(),
        Some("MyShop v2.3.1")
    );

    // ...and `.version(...)` implies showing it, so the order of the two builders cannot
    // silently cancel the intent.
    let plugin = AdminPlugin::default()
        .show_version(false)
        .version("MyShop v2.3.1");
    assert_eq!(
        plugin.branding_for_tests().version_label.as_deref(),
        Some("MyShop v2.3.1"),
        "an explicit .version(...) after show_version(false) must win — the caller said \
         what they wanted second"
    );
}

/// On by default (unchanged behaviour), just no longer a lie.
#[test]
fn the_version_is_shown_by_default() {
    let plugin = AdminPlugin::default();
    assert_eq!(
        plugin.branding_for_tests().version_label,
        Some(umbral_version_label())
    );
}

/// A source-level guard. The bug was a LITERAL in a template, and a builder test cannot
/// see a template — so assert directly that no hardcoded version string has crept back in.
#[test]
fn no_template_hardcodes_a_version_literal() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    for entry in std::fs::read_dir(&dir).expect("templates dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read template");
        for line in body.lines() {
            // `docs/v0.0.1/...` is a documentation URL, not a version claim.
            if line.contains("docs/v0.0.1") {
                continue;
            }
            assert!(
                !line.contains("v0.0.1"),
                "{} hardcodes a version literal — use {{{{ admin_version }}}}:\n  {}",
                path.display(),
                line.trim()
            );
        }
    }
}
