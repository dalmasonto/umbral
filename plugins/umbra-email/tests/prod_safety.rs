//! Safety tests for the console backend in non-Dev/Test environments.
//!
//! The console backend is a dev-only seam. In production it must refuse
//! to deliver rather than printing message bodies (which may carry
//! password-reset tokens, magic-link URLs, or other secrets) to stderr
//! where log aggregators will ingest them.
//!
//! ## Design note: no `App::build()` here
//!
//! These tests exercise the send-time environment guard in `send()`,
//! which reads `umbra::settings::get_opt()`. When `App::build()` has
//! not been called, `get_opt()` returns `None`. The guard treats an
//! absent settings as "environment unknown — fail closed", so
//! `ConsoleBackendInProduction` is returned without needing a Prod
//! `App` at all.
//!
//! The `integration.rs` binary *does* call `App::build()` (with the
//! default Dev environment), and because each `tests/*.rs` file is its
//! own executable the two OnceLocks never collide.

use umbra_email::{EmailError, EmailMessage, send};

/// Without `App::build()` the ambient settings are absent
/// (`get_opt()` returns `None`). The console backend must treat an
/// absent environment as "not Dev/Test" and fail closed, returning
/// `EmailError::ConsoleBackendInProduction` instead of printing the
/// body to stderr.
///
/// This is the critical prod-safety invariant: misconfigured deployments
/// (SMTP credentials absent, App never fully booted) must never silently
/// leak message bodies.
#[tokio::test]
async fn console_backend_fails_closed_when_environment_is_unknown() {
    // Force console mode: no SMTP host.
    unsafe {
        std::env::remove_var("UMBRA_EMAIL_SMTP_HOST");
        std::env::remove_var("UMBRA_EMAIL_DEFAULT_FROM");
    }

    let msg = EmailMessage::new(
        "Reset your password",
        vec!["alice@example.com".into()],
    )
    .from("noreply@example.com")
    .text_body("Your reset token is: SECRET-TOKEN-abc123");

    let err = send(&msg)
        .await
        .expect_err("console backend must refuse when environment is unknown/non-Dev");

    assert!(
        matches!(err, EmailError::ConsoleBackendInProduction),
        "expected ConsoleBackendInProduction, got {err:?}"
    );
}

/// The `ConsoleBackendInProduction` error display must NOT carry any
/// fragment of the email body. The whole point of the fail-closed guard
/// is to prevent secrets from leaking; the error message itself must be
/// clean.
#[tokio::test]
async fn console_backend_error_does_not_contain_message_body() {
    unsafe {
        std::env::remove_var("UMBRA_EMAIL_SMTP_HOST");
        std::env::remove_var("UMBRA_EMAIL_DEFAULT_FROM");
    }

    let secret_token = "SECRET-TOKEN-xyz789";
    let msg = EmailMessage::new("Reset", vec!["bob@example.com".into()])
        .from("noreply@example.com")
        .text_body(format!("Token: {secret_token}"));

    let err = send(&msg)
        .await
        .expect_err("console backend must refuse when environment is unknown/non-Dev");

    let display = format!("{err}");
    assert!(
        !display.contains(secret_token),
        "error display must not contain the message body / token; got: {display}"
    );
}

/// Pre-send validation (missing From, empty To) must still run before
/// the backend guard — those checks are independent of the environment
/// and must surface as their own typed errors.
#[tokio::test]
async fn missing_from_checked_before_backend_guard() {
    unsafe {
        std::env::remove_var("UMBRA_EMAIL_SMTP_HOST");
    }

    let msg = EmailMessage::new("Subject", vec!["bob@example.com".into()])
        .text_body("Hi");
    // No `.from(...)` and no UMBRA_EMAIL_DEFAULT_FROM → MissingFrom,
    // not ConsoleBackendInProduction.
    let err = send(&msg).await.expect_err("missing From should error");
    assert!(
        matches!(err, EmailError::MissingFrom),
        "expected MissingFrom (pre-backend check), got {err:?}"
    );
}
