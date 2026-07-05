//! Coverage for the `plugin.security_missing` system check (gaps2 #25,
//! scaffold-independent half): a Warning fires when `auth` or `sessions`
//! is registered but `security` is not, and stays silent otherwise.
//!
//! The check is purely a function of `ctx.registered_plugin_names`, so we
//! drive it via `run_all` without an `App::builder().build()` call. That
//! keeps the OnceLock constraint out of this binary entirely — no test
//! here touches the process-wide registries.
//!
//! See `crates/umbral-core/src/check.rs::plugin_security_missing`.

use std::collections::HashMap;

use umbral_core::backend::SqliteBackend;
use umbral_core::check::{CheckContext, Severity, framework_checks, run_all};
use umbral_core::settings::{Environment, Settings};

/// Minimal Settings that pass every other framework check so the only
/// findings we see are from `plugin.security_missing`.
fn safe_settings() -> Settings {
    Settings {
        database_url: "sqlite::memory:".to_string(),
        databases: HashMap::new(),
        max_form_body_bytes: Some(16 * 1024 * 1024),
        secret_key: "not-the-insecure-default-so-other-checks-stay-quiet".to_string(),
        environment: Environment::Dev,
        allowed_hosts: vec!["localhost".to_string(), "127.0.0.1".to_string()],
        log_level: "info".to_string(),
        db_max_connections: 10,
        db_acquire_timeout_secs: 30,
        db_min_connections: 0,
        db_idle_timeout_secs: Some(600),
        db_max_lifetime_secs: Some(1800),
        db_test_before_acquire: true,
        bind_addr: "127.0.0.1:8000".to_string(),
        trusted_proxy_hops: 0,
        time_zone: None,
        static_url: "/static/".to_string(),
        static_root: "staticfiles/".to_string(),
        extra: HashMap::new(),
    }
}

fn make_ctx<'a>(settings: &'a Settings, names: &'a [&'a str]) -> CheckContext<'a> {
    CheckContext {
        backend: &SqliteBackend,
        settings,
        provides_storage: true,
        registered_plugin_names: names,
    }
}

// ---------------------------------------------------------------------------
// (a) auth present, security absent → Warning
// ---------------------------------------------------------------------------

#[test]
fn warns_when_auth_mounted_without_security() {
    let settings = safe_settings();
    let ctx = make_ctx(&settings, &["auth"]);
    let findings = run_all(&ctx, &framework_checks());
    let hit = findings
        .iter()
        .find(|f| f.check_id == "plugin.security_missing" && f.severity == Severity::Warning);
    assert!(
        hit.is_some(),
        "auth without security should produce a plugin.security_missing Warning; got {findings:#?}",
    );
}

// ---------------------------------------------------------------------------
// (b) sessions present, security absent → Warning
// ---------------------------------------------------------------------------

#[test]
fn warns_when_sessions_mounted_without_security() {
    let settings = safe_settings();
    let ctx = make_ctx(&settings, &["sessions"]);
    let findings = run_all(&ctx, &framework_checks());
    let hit = findings
        .iter()
        .find(|f| f.check_id == "plugin.security_missing" && f.severity == Severity::Warning);
    assert!(
        hit.is_some(),
        "sessions without security should produce a plugin.security_missing Warning; got {findings:#?}",
    );
}

// ---------------------------------------------------------------------------
// (c) auth + sessions present, security absent → Warning (one finding)
// ---------------------------------------------------------------------------

#[test]
fn warns_when_auth_and_sessions_mounted_without_security() {
    let settings = safe_settings();
    let ctx = make_ctx(&settings, &["auth", "sessions"]);
    let findings = run_all(&ctx, &framework_checks());
    let hits: Vec<_> = findings
        .iter()
        .filter(|f| f.check_id == "plugin.security_missing" && f.severity == Severity::Warning)
        .collect();
    assert!(
        !hits.is_empty(),
        "auth + sessions without security should warn; got {findings:#?}",
    );
    assert_eq!(
        hits.len(),
        1,
        "should produce exactly one plugin.security_missing Warning, not {}",
        hits.len(),
    );
}

// ---------------------------------------------------------------------------
// (d) auth + security → no warning
// ---------------------------------------------------------------------------

#[test]
fn no_warning_when_auth_and_security_both_present() {
    let settings = safe_settings();
    let ctx = make_ctx(&settings, &["auth", "security"]);
    let findings = run_all(&ctx, &framework_checks());
    let hit = findings
        .iter()
        .find(|f| f.check_id == "plugin.security_missing");
    assert!(
        hit.is_none(),
        "auth + security should produce no plugin.security_missing finding; got {findings:#?}",
    );
}

// ---------------------------------------------------------------------------
// (e) sessions + security → no warning
// ---------------------------------------------------------------------------

#[test]
fn no_warning_when_sessions_and_security_both_present() {
    let settings = safe_settings();
    let ctx = make_ctx(&settings, &["sessions", "security"]);
    let findings = run_all(&ctx, &framework_checks());
    let hit = findings
        .iter()
        .find(|f| f.check_id == "plugin.security_missing");
    assert!(
        hit.is_none(),
        "sessions + security should produce no plugin.security_missing finding; got {findings:#?}",
    );
}

// ---------------------------------------------------------------------------
// (f) neither auth nor sessions → no warning
// ---------------------------------------------------------------------------

#[test]
fn no_warning_when_neither_auth_nor_sessions_present() {
    let settings = safe_settings();
    let ctx = make_ctx(&settings, &["admin", "rest"]);
    let findings = run_all(&ctx, &framework_checks());
    let hit = findings
        .iter()
        .find(|f| f.check_id == "plugin.security_missing");
    assert!(
        hit.is_none(),
        "no auth/sessions → no plugin.security_missing finding; got {findings:#?}",
    );
}

// ---------------------------------------------------------------------------
// (g) empty plugin list → no warning
// ---------------------------------------------------------------------------

#[test]
fn no_warning_when_no_plugins_registered() {
    let settings = safe_settings();
    let ctx = make_ctx(&settings, &[]);
    let findings = run_all(&ctx, &framework_checks());
    let hit = findings
        .iter()
        .find(|f| f.check_id == "plugin.security_missing");
    assert!(
        hit.is_none(),
        "empty plugin list → no plugin.security_missing finding; got {findings:#?}",
    );
}

// ---------------------------------------------------------------------------
// (h) warning is NOT an error — boot continues (severity check)
// ---------------------------------------------------------------------------

#[test]
fn warning_is_not_an_error_severity() {
    let settings = safe_settings();
    let ctx = make_ctx(&settings, &["auth"]);
    let findings = run_all(&ctx, &framework_checks());
    let hit = findings
        .iter()
        .find(|f| f.check_id == "plugin.security_missing")
        .expect("auth without security should produce a finding");
    assert_eq!(
        hit.severity,
        Severity::Warning,
        "plugin.security_missing must be Warning, not Error — boot should continue",
    );
}
