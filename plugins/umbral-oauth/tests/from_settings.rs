//! gaps4 #46: `OAuthPlugin::from_settings` + `provider_opt` — the
//! constructor that replaces the hand-rolled env plumbing every consumer
//! wrote (12 lines of `extra_str(...).zip(...)` + a block expression
//! inside `.plugin({ ... })`).
//!
//! Pure construction tests: no App boot, no HTTP. `Settings.extra` is a
//! public flattened map, so the tests build settings directly instead of
//! going through env vars (which are process-global and race across
//! parallel tests).

use umbral_oauth::OAuthPlugin;
use umbral_oauth::providers::GoogleProvider;

fn settings_with(pairs: &[(&str, &str)]) -> umbral::Settings {
    let mut settings = umbral::Settings::from_env().expect("figment defaults");
    for (k, v) in pairs {
        settings
            .extra
            .insert(k.to_string(), umbral::toml::Value::String(v.to_string()));
    }
    settings
}

/// Both credential halves present → the provider is registered; the
/// redirect base comes from the conventional key.
#[test]
fn from_settings_wires_fully_configured_providers() {
    let settings = settings_with(&[
        ("oauth_redirect_base", "https://app.example.com"),
        ("oauth_google_client_id", "gid"),
        ("oauth_google_client_secret", "gsecret"),
    ]);
    let plugin = OAuthPlugin::from_settings(&settings);
    assert_eq!(plugin.provider_keys(), vec!["google"]);
}

/// Half a credential is a typo, not a choice — the provider is skipped
/// (with a warning), and a missing pair is simply absent.
#[test]
fn from_settings_skips_half_configured_and_absent_providers() {
    let settings = settings_with(&[
        ("oauth_redirect_base", "https://app.example.com"),
        // Google: id only, no secret → skipped.
        ("oauth_google_client_id", "gid"),
        // GitHub: nothing → absent.
    ]);
    let plugin = OAuthPlugin::from_settings(&settings);
    assert!(
        plugin.provider_keys().is_empty(),
        "half-configured and unconfigured providers both yield nothing"
    );
}

/// `provider_opt(None)` chains through unchanged; `Some` registers — the
/// straight-line alternative to the `{ let mut plugin = ...; if ... }`
/// block expression.
#[test]
fn provider_opt_chains_none_and_some() {
    let plugin = OAuthPlugin::new("http://localhost:8000")
        .provider_opt(None::<GoogleProvider>)
        .provider_opt(Some(GoogleProvider::new("id", "secret")));
    assert_eq!(plugin.provider_keys(), vec!["google"]);
}
