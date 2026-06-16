//! PKCE — Proof Key for Code Exchange (RFC 7636).
//!
//! PKCE binds the authorization request to the eventual token exchange: the
//! client mints a secret `code_verifier`, sends only its SHA-256 hash
//! (`code_challenge`) on the authorize redirect, then proves possession of
//! the secret by sending the verifier itself on the token exchange. An
//! attacker who intercepts the redirected `code` (a malicious app on the
//! same device, a leaky `Referer`, server logs) can't redeem it without the
//! verifier, which never left the client.
//!
//! We always use the **S256** method; `plain` is never emitted because it
//! offers no protection against an attacker who can read the authorize
//! request. Sending the challenge is safe even against a provider that
//! doesn't enforce PKCE — RFC 6749 §3.1 requires authorization servers to
//! ignore unrecognized request parameters.

use base64::Engine as _;
use rand::RngCore as _;
use sha2::{Digest, Sha256};

/// URL-safe base64 without padding — the exact alphabet RFC 7636 §4.1
/// mandates for both the verifier and the challenge.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Mint a fresh `code_verifier`: 32 CSPRNG bytes, base64url-encoded to 43
/// characters drawn entirely from the unreserved set (`[A-Za-z0-9-_]`, a
/// subset of what RFC 7636 §4.1 permits, within its 43..=128 length range).
pub fn generate_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    B64.encode(bytes)
}

/// Derive the S256 `code_challenge` for a verifier:
/// `BASE64URL-NOPAD(SHA256(ASCII(code_verifier)))`.
pub fn challenge_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    B64.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_rfc7636_test_vector() {
        // RFC 7636 Appendix B — the canonical worked example.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_s256(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_is_43_chars_of_unreserved_alphabet() {
        let v = generate_verifier();
        assert_eq!(v.len(), 43, "32 bytes base64url-nopad → 43 chars");
        assert!((43..=128).contains(&v.len()), "RFC 7636 §4.1 length range");
        assert!(
            v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
            "only the URL-safe base64 alphabet: {v}"
        );
    }

    #[test]
    fn verifiers_are_unique_per_call() {
        assert_ne!(
            generate_verifier(),
            generate_verifier(),
            "each flow must get a fresh secret"
        );
    }
}
