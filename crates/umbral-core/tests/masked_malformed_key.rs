//! Tests that a present-but-malformed `UMBRAL_MASK_PUBLIC_KEY` surfaces an
//! error instead of silently disabling encryption and allowing plaintext
//! writes.
//!
//! These tests exercise `MaskKeyring::from_base64` and `from_env` directly,
//! because the ambient `OnceLock` is process-wide and can only be set once
//! per binary (set by the first test that calls `set_mask_keyring`). We
//! therefore keep the ambient-keyring path in `masked_roundtrip.rs` and test
//! the three construction cases here via the public builder API.

use base64::Engine as _;
use umbral::orm::{MaskError, MaskKeyring};

/// `UMBRAL_MASK_PUBLIC_KEY` is process-global. The two tests that mutate it
/// (`absent_env_key_returns_no_keyring` removes it; the `from_env` cases set
/// it) race under the default parallel test harness: one removes the var and
/// reads it back while the other sets it, so the absent-key test can observe a
/// malformed value and vice-versa. Serialising the env-mutating tests on this
/// lock makes the set/remove/read sequence single-owner-at-a-time. Mirrors the
/// `SUPERUSER_ENV_LOCK` pattern in `plugins/umbral-auth/tests/integration.rs`
/// (gaps2 #30 / #52). std::sync::Mutex because these are sync `#[test]`s.
static MASK_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// -------------------------------------------------------------------------
// Case 1: absent key → Err(NoKeyring), not Err(BadKey)
// -------------------------------------------------------------------------

#[test]
fn absent_env_key_returns_no_keyring() {
    let _env_guard = MASK_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Temporarily ensure the var is absent.
    let prev = std::env::var("UMBRAL_MASK_PUBLIC_KEY").ok();
    unsafe { std::env::remove_var("UMBRAL_MASK_PUBLIC_KEY") };
    let result = MaskKeyring::from_env();
    // Restore
    if let Some(v) = prev {
        unsafe { std::env::set_var("UMBRAL_MASK_PUBLIC_KEY", v) };
    }

    assert!(
        matches!(result, Err(MaskError::NoKeyring)),
        "absent key must return NoKeyring, not BadKey"
    );
}

// -------------------------------------------------------------------------
// Case 2: malformed key (bad base64) → BadKey error, NOT Ok(...)
// -------------------------------------------------------------------------

#[test]
fn bad_base64_public_key_is_bad_key_error() {
    let result = MaskKeyring::from_base64("this-is-not-valid-base64!!!", None);
    assert!(
        matches!(result, Err(MaskError::BadKey(_))),
        "bad base64 public key must return BadKey"
    );
}

#[test]
fn bad_base64_secret_key_is_bad_key_error() {
    // Valid 32-byte public key.
    let (pub_b64, _) = MaskKeyring::generate();
    // Malformed private key.
    let result = MaskKeyring::from_base64(&pub_b64, Some("not-valid-base64!!!"));
    assert!(
        matches!(result, Err(MaskError::BadKey(_))),
        "bad base64 secret key must return BadKey"
    );
}

// -------------------------------------------------------------------------
// Case 3: wrong byte length → BadKey with an informative message
// -------------------------------------------------------------------------

#[test]
fn wrong_length_public_key_is_bad_key_error() {
    // Valid base64 of 16 bytes (half the required 32).
    let short = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
    let result = MaskKeyring::from_base64(&short, None);
    match result {
        Err(MaskError::BadKey(msg)) => {
            assert!(
                msg.contains("32 bytes"),
                "error should mention expected length, got: {msg}"
            );
        }
        _ => panic!("wrong-length key must return Err(BadKey)"),
    }
}

// -------------------------------------------------------------------------
// Case 4: a correctly-formatted key still works end-to-end
// -------------------------------------------------------------------------

#[test]
fn valid_key_seals_and_opens() {
    let (pub_b64, sec_b64) = MaskKeyring::generate();
    let kr = MaskKeyring::from_base64(&pub_b64, Some(&sec_b64))
        .expect("valid keypair must not return an error");
    let sealed = kr.seal(b"sensitive-data");
    assert_ne!(
        sealed, "sensitive-data",
        "sealed form must not be plaintext"
    );
    assert_eq!(kr.open(&sealed).unwrap(), "sensitive-data");
}

// -------------------------------------------------------------------------
// Case 5: malformed key via from_env returns BadKey, not NoKeyring
// -------------------------------------------------------------------------

#[test]
fn malformed_env_key_returns_bad_key_not_no_keyring() {
    let _env_guard = MASK_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Set the env var to something that is not valid base64 / 32 bytes.
    let prev = std::env::var("UMBRAL_MASK_PUBLIC_KEY").ok();
    unsafe { std::env::set_var("UMBRAL_MASK_PUBLIC_KEY", "definitelynotavalidkey!!") };
    let result = MaskKeyring::from_env();
    // Restore regardless of outcome.
    unsafe { std::env::remove_var("UMBRAL_MASK_PUBLIC_KEY") };
    if let Some(v) = prev {
        unsafe { std::env::set_var("UMBRAL_MASK_PUBLIC_KEY", v) };
    }

    assert!(
        !matches!(result, Err(MaskError::NoKeyring)),
        "a present-but-malformed key must NOT return NoKeyring; \
         that misleads callers into thinking the key is absent"
    );
    assert!(
        matches!(result, Err(MaskError::BadKey(_))),
        "a present-but-malformed key must return BadKey"
    );
}
