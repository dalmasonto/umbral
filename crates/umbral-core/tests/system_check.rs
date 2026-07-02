//! Coverage for the M4 system-check framework: the shape of
//! `framework_checks()`, the dot-delimited check-id convention, the
//! `settings.required` check's pass/fail surface, `run_all` over an
//! empty slice, and one end-to-end pass through `AppBuilder::build()`
//! that drives a real `BuildError::SystemCheckFailed` out of phase 4.
//!
//! The build-lifecycle test is intentionally the only call to
//! `App::builder().build()` in this file. `build()` writes to
//! process-wide OnceLocks (settings, db, backend); within one test
//! binary it can only succeed past phase 3 once. Cargo's parallel
//! runner gives each `tests/*.rs` file its own binary, so this file is
//! isolated from `tests/builder.rs` and friends.
//!
//! See `crates/umbral-core/src/check.rs` for the framework being tested.

use std::collections::HashMap;

use umbral_core::backend::SqliteBackend;
use umbral_core::check::{CheckContext, Severity, framework_checks, run_all};
use umbral_core::settings::{Environment, Settings};

/// The literal default secret_key from `crate::settings::default_secret_key`.
/// Duplicated here (rather than reaching for the private const in check.rs)
/// because that's the contract end users will be tripping over: if anyone
/// renames it, the test should fail loudly so the docs and check.rs stay
/// in sync.
const INSECURE_DEV_SECRET_KEY: &str = "umbral-insecure-dev-key-change-me";

/// Helper: build a Settings struct in one place so each test only has to
/// describe the deltas that matter for what it's asserting. The default
/// here mirrors the figment defaults the production loader emits.
fn make_settings(environment: Environment, secret_key: &str) -> Settings {
    Settings {
        database_url: "sqlite::memory:".to_string(),
        databases: HashMap::new(),
        max_form_body_bytes: Some(16 * 1024 * 1024),
        secret_key: secret_key.to_string(),
        environment,
        allowed_hosts: vec!["localhost".to_string(), "127.0.0.1".to_string()],
        log_level: "info".to_string(),
        db_max_connections: 10,
        db_acquire_timeout_secs: 30,
        db_min_connections: 0,
        db_idle_timeout_secs: Some(600),
        db_max_lifetime_secs: Some(1800),
        db_test_before_acquire: true,
        bind_addr: "127.0.0.1:8000".to_string(),
        time_zone: None,
        static_url: "/static/".to_string(),
        static_root: "staticfiles/".to_string(),
        extra: HashMap::new(),
    }
}

/// At M4 the framework ships at least the `settings.required` check;
/// `framework_checks()` returning an empty Vec would mean the catalogue
/// was wired off by accident.
#[test]
fn framework_checks_returns_non_empty_vec() {
    let checks = framework_checks();
    assert!(
        !checks.is_empty(),
        "framework_checks() should ship at least one check at M4; got an empty Vec",
    );
}

/// Pin the convention that every check id is a stable dot-delimited
/// string. Operators are expected to grep these out of logs and error
/// reports, so the format has to stay greppable: at minimum the id
/// contains a `.` separator, and the prefix is one of the recognised
/// namespaces.
#[test]
fn framework_check_ids_are_dot_delimited_stable_strings() {
    let allowed_prefixes = [
        "settings.",
        "backend.",
        "field.",
        "model.",
        "plugin.",
        "route.",
    ];
    for check in framework_checks() {
        assert!(
            check.id.contains('.'),
            "check id `{}` should be dot-delimited (`namespace.name`)",
            check.id,
        );
        assert!(
            allowed_prefixes
                .iter()
                .any(|prefix| check.id.starts_with(prefix)),
            "check id `{}` should start with a recognised namespace prefix \
             (one of {allowed_prefixes:?}); add the new namespace here if intentional",
            check.id,
        );
    }
}

/// In Dev the insecure default is fine — that's the whole point of
/// having a dev default — so `settings.required` must stay silent. Run
/// the full framework catalogue (not just one check) to catch any other
/// check that mis-fires on a sane Dev profile.
#[test]
fn settings_required_passes_when_dev_environment() {
    let settings = make_settings(Environment::Dev, INSECURE_DEV_SECRET_KEY);
    let ctx = CheckContext {
        backend: &SqliteBackend,
        settings: &settings,
        provides_storage: true,
        registered_plugin_names: &[],
    };
    let findings = run_all(&ctx, &framework_checks());
    assert!(
        findings.is_empty(),
        "Dev profile with default secret_key should produce zero findings; got {findings:#?}",
    );
}

/// A Prod app that overrode the secret_key is the supported production
/// posture, so `settings.required` must report nothing. Other checks
/// (e.g. allowed_hosts) might still warn here, but no finding should be
/// at Severity::Error.
#[test]
fn settings_required_passes_when_secret_key_overridden_in_prod() {
    // A realistic strong key: not the dev default AND >= 32 chars (audit_2 H15).
    let settings = make_settings(
        Environment::Prod,
        "real-secret-not-the-default-0123456789abcdef",
    );
    let ctx = CheckContext {
        backend: &SqliteBackend,
        settings: &settings,
        provides_storage: true,
        registered_plugin_names: &[],
    };
    let findings = run_all(&ctx, &framework_checks());
    let errors: Vec<_> = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "Prod with overridden secret should produce zero Error-severity findings; got {errors:#?}",
    );
}

/// The core failure mode the check exists for: Prod + the literal dev
/// default secret_key. Must surface at least one finding tagged
/// `settings.required` at `Severity::Error`, since this is what the
/// builder turns into `BuildError::SystemCheckFailed`.
#[test]
fn settings_required_errors_when_default_secret_in_prod() {
    let settings = make_settings(Environment::Prod, INSECURE_DEV_SECRET_KEY);
    let ctx = CheckContext {
        backend: &SqliteBackend,
        settings: &settings,
        provides_storage: true,
        registered_plugin_names: &[],
    };
    let findings = run_all(&ctx, &framework_checks());

    let hit = findings
        .iter()
        .find(|f| f.check_id == "settings.required" && f.severity == Severity::Error);
    assert!(
        hit.is_some(),
        "Prod + default secret_key should produce a `settings.required` Error finding; got {findings:#?}",
    );
}

/// `run_all` over an empty slice should be a no-op returning an empty
/// Vec. Easy to break by accident if someone later adds an "always
/// include framework_checks" shortcut inside `run_all`.
#[test]
fn run_all_handles_empty_checks() {
    let settings = make_settings(Environment::Dev, INSECURE_DEV_SECRET_KEY);
    let ctx = CheckContext {
        backend: &SqliteBackend,
        settings: &settings,
        provides_storage: true,
        registered_plugin_names: &[],
    };
    let findings = run_all(&ctx, &[]);
    assert!(
        findings.is_empty(),
        "run_all over an empty slice should return an empty Vec; got {findings:#?}",
    );
}

/// End-to-end coverage: drive `App::builder().build()` with a Settings
/// configured for Prod and the literal dev-default secret_key. Phase 4
/// must reject the build and return `BuildError::SystemCheckFailed`,
/// and the carried findings must include the `settings.required` one.
///
/// This is the only call to `.build()` in this file by design: it
/// writes to process-wide OnceLocks, and within one test binary only
/// one such call can pass phase 3. See the file-level docstring.
#[tokio::test]
async fn system_check_failed_build_returns_build_error_system_check_failed() {
    let settings = make_settings(Environment::Prod, INSECURE_DEV_SECRET_KEY);
    let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");

    let result = umbral_core::app::App::builder()
        .settings(settings)
        .database("default", pool)
        .build();

    let err = result
        .err()
        .expect("Prod + default secret_key should fail build at phase 4");

    let findings = match err {
        umbral_core::app::BuildError::SystemCheckFailed { findings } => findings,
        other => panic!("expected BuildError::SystemCheckFailed, got {other:?}"),
    };

    assert!(
        findings
            .iter()
            .any(|f| f.check_id == "settings.required" && f.severity == Severity::Error),
        "SystemCheckFailed findings should include the settings.required Error finding; got {findings:#?}",
    );
}
