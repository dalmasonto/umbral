//! Verify that `SecurityPlugin::on_ready` fails closed on an empty
//! `SECRET_KEY` in production and warns (but allows boot) in Dev/Test.
//!
//! Because `umbra::settings::init` is `pub(crate)` and unreachable from
//! plugin tests, we exercise the inner validation logic via
//! `umbra_security::test_support::validate_secret_key`, which takes an
//! explicit `umbra::Settings` reference rather than reading the ambient
//! `SETTINGS` OnceLock.
//!
//! `umbra::Settings` derives `serde::Deserialize` with serde defaults on
//! every field, so `serde_json::from_value(json!({"key": "val", ...}))` is
//! the lightest way to construct one in tests without going through
//! `App::build()`.

use umbra::Settings;
use umbra_security::{SecurityConfig, test_support::validate_secret_key};

/// Deserialize a `Settings` from a JSON object, filling every unmentioned
/// field from serde defaults. Panics on invalid JSON — this is test code.
fn settings(json: serde_json::Value) -> Settings {
    serde_json::from_value(json).expect("Settings deserialization failed")
}

/// A `SecurityConfig` with CSRF and signed_csrf both enabled (the mode that
/// actually uses the secret). All other fields use their defaults.
fn csrf_signed_config() -> SecurityConfig {
    SecurityConfig {
        csrf: true,
        signed_csrf: true,
        ..Default::default()
    }
}

// ── empty secret_key + Prod → boot must fail ─────────────────────────────────

#[test]
fn empty_secret_key_prod_returns_err() {
    let s = settings(serde_json::json!({
        "secret_key": "",
        "environment": "Prod"
    }));
    let result = validate_secret_key(&s, &csrf_signed_config());
    assert!(
        result.is_err(),
        "expected Err for empty secret_key in Prod, got Ok"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("SECRET_KEY"),
        "error message should mention SECRET_KEY, got: {msg}"
    );
}

#[test]
fn whitespace_only_secret_key_prod_returns_err() {
    let s = settings(serde_json::json!({
        "secret_key": "   ",
        "environment": "Prod"
    }));
    let result = validate_secret_key(&s, &csrf_signed_config());
    assert!(
        result.is_err(),
        "expected Err for whitespace-only secret_key in Prod, got Ok"
    );
}

// ── empty secret_key + Dev → warn but allow boot ─────────────────────────────

#[test]
fn empty_secret_key_dev_returns_ok() {
    let s = settings(serde_json::json!({
        "secret_key": "",
        "environment": "Dev"
    }));
    // Should warn (can't assert the tracing output here) but must not error.
    let result = validate_secret_key(&s, &csrf_signed_config());
    assert!(
        result.is_ok(),
        "expected Ok for empty secret_key in Dev, got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn empty_secret_key_test_env_returns_ok() {
    let s = settings(serde_json::json!({
        "secret_key": "",
        "environment": "Test"
    }));
    let result = validate_secret_key(&s, &csrf_signed_config());
    assert!(
        result.is_ok(),
        "expected Ok for empty secret_key in Test environment, got: {:?}",
        result.unwrap_err()
    );
}

// ── non-empty secret_key → always Ok regardless of environment ───────────────

#[test]
fn non_empty_secret_key_prod_returns_ok() {
    let s = settings(serde_json::json!({
        "secret_key": "a-real-secret-key-that-is-long-enough",
        "environment": "Prod"
    }));
    let result = validate_secret_key(&s, &csrf_signed_config());
    assert!(
        result.is_ok(),
        "expected Ok for non-empty secret_key in Prod, got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn non_empty_secret_key_dev_returns_ok() {
    let s = settings(serde_json::json!({
        "secret_key": "dev-secret",
        "environment": "Dev"
    }));
    let result = validate_secret_key(&s, &csrf_signed_config());
    assert!(result.is_ok());
}

// ── CSRF disabled → empty secret is irrelevant ───────────────────────────────

#[test]
fn csrf_disabled_empty_secret_prod_returns_ok() {
    let s = settings(serde_json::json!({
        "secret_key": "",
        "environment": "Prod"
    }));
    let config = SecurityConfig {
        csrf: false,
        signed_csrf: true,
        ..Default::default()
    };
    // CSRF is off so the secret is never used; no error.
    let result = validate_secret_key(&s, &config);
    assert!(
        result.is_ok(),
        "expected Ok when csrf=false even with empty secret, got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn signed_csrf_disabled_empty_secret_prod_returns_ok() {
    let s = settings(serde_json::json!({
        "secret_key": "",
        "environment": "Prod"
    }));
    let config = SecurityConfig {
        csrf: true,
        signed_csrf: false,
        ..Default::default()
    };
    // Plain double-submit — secret unused; no error.
    let result = validate_secret_key(&s, &config);
    assert!(
        result.is_ok(),
        "expected Ok when signed_csrf=false even with empty secret, got: {:?}",
        result.unwrap_err()
    );
}
