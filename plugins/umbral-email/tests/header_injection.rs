//! Security: CRLF / header-injection in the email subject.
//!
//! A templated subject like `Re: {user_input}` where the input contains
//! `\r\nBcc: attacker@evil.com` is the classic SMTP header-injection /
//! Bcc-injection vector.  These tests assert that `compose` (and therefore
//! `send`) REJECTS any subject that contains a bare `\r`, `\n`, or any
//! ASCII control character < 0x20 (except horizontal tab, which RFC 5322
//! permits in folded headers).
//!
//! If lettre already guards this surface the tests pass by construction and
//! lock that contract so a lettre upgrade can't silently regress it.  If
//! lettre does NOT guard it, the tests surface the gap so the plugin can
//! add an explicit guard before the builder call.

use umbral_email::{EmailMessage, compose};

/// Helper: build the minimal From/to context and call `compose`.
fn try_subject(subject: &str) -> Result<(), Box<dyn std::error::Error>> {
    let msg = EmailMessage::new(subject, vec!["bob@example.com".into()]);
    compose("noreply@example.com", &msg)?;
    Ok(())
}

/// The classic CRLF injection: `\r\n` followed by a header line that
/// would be silently appended to the outgoing message.
#[test]
fn crlf_in_subject_is_rejected() {
    let malicious = "Hello\r\nBcc: attacker@evil.com";
    let result = try_subject(malicious);
    assert!(
        result.is_err(),
        "compose() must reject a subject containing CRLF; got Ok instead. \
         This means a Bcc-injection payload would be accepted silently."
    );
}

/// Bare `\n` (LF only) is also invalid in an RFC 5322 header value and
/// can still inject header lines through some SMTP implementations.
#[test]
fn lf_in_subject_is_rejected() {
    let malicious = "Hello\nBcc: attacker@evil.com";
    let result = try_subject(malicious);
    assert!(
        result.is_err(),
        "compose() must reject a subject containing a bare LF; got Ok instead."
    );
}

/// Bare `\r` (CR only) — invalid in header values for the same reason.
#[test]
fn cr_in_subject_is_rejected() {
    let malicious = "Hello\rBcc: attacker@evil.com";
    let result = try_subject(malicious);
    assert!(
        result.is_err(),
        "compose() must reject a subject containing a bare CR; got Ok instead."
    );
}

/// NUL bytes can confuse downstream SMTP parsers and are banned by
/// RFC 5321 in the command stream.
#[test]
fn null_byte_in_subject_is_rejected() {
    let malicious = "Hello\x00World";
    let result = try_subject(malicious);
    assert!(
        result.is_err(),
        "compose() must reject a subject containing a NUL byte; got Ok instead."
    );
}

/// Sanity check: a well-formed subject must still be accepted so the
/// guard doesn't break the happy path.
#[test]
fn normal_subject_is_accepted() {
    let result = try_subject("Re: Your order #1234 is ready!");
    assert!(
        result.is_ok(),
        "compose() must accept a clean subject; got Err: {result:?}"
    );
}

/// Unicode subjects (non-ASCII) are valid and must not be blocked.
#[test]
fn unicode_subject_is_accepted() {
    let result = try_subject("Ваш заказ готов 🎉");
    assert!(
        result.is_ok(),
        "compose() must accept a Unicode subject; got Err: {result:?}"
    );
}
