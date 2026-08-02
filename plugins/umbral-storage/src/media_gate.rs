//! Media access-gate primitives: key extraction, HMAC-signed time-bounded
//! URLs (gaps4 #56), and the composite allow decision shared by the
//! filesystem serve path and the non-FS proxy route (gaps4 #58).

use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::MediaAccessFn;

/// Path-segment percent-encode set: everything except the RFC-3986
/// unreserved characters (alphanumerics + `-` `.` `_` `~`), so a spaced
/// filename encodes to `a%20b.jpg` rather than the over-escaped
/// `a%20b%2Ejpg` that `NON_ALPHANUMERIC` alone produces.
const KEY_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The requested media key for a path under `mount_prefix`, percent-decoded
/// to the same string `FileField` stores and the backend resolves
/// (`/media/a%20b.jpg` → `a b.jpg`). Invalid UTF-8 after decoding keeps the
/// raw string, which matches no stored key — the gate then fails CLOSED.
pub(crate) fn key_for_path(path: &str, mount_prefix: &str) -> String {
    let raw_key = path
        .strip_prefix(mount_prefix)
        .unwrap_or(path)
        .trim_start_matches('/');
    percent_encoding::percent_decode_str(raw_key)
        .decode_utf8()
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| raw_key.to_string())
}

/// `hex(HMAC-SHA256(secret, "<key>:<exp>"))` — the signature bound into a
/// signed media URL. The same primitive the security plugin signs CSRF
/// tokens with; binding both the key AND the expiry means a signature is
/// valid for exactly one file until exactly one instant.
fn sign(secret: &str, key: &str, exp: i64) -> String {
    let mut mac =
        <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(key.as_bytes());
    mac.update(b":");
    mac.update(exp.to_string().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Build a time-bounded signed URL for a media key on the given mount
/// (gaps4 #56). The returned path — `<mount>/<key>?exp=<unix>&sig=<hex>` —
/// is served only until `exp`, and only by a plugin configured with
/// [`StoragePlugin::media_signed_urls`](crate::StoragePlugin::media_signed_urls).
///
/// The secret is the app's `settings.secret_key`; the key is percent-encoded
/// into the path while the signature is computed over the DECODED key, so a
/// key with spaces round-trips through the gate's decode step.
///
/// This is how you hand a private filesystem-backed file to someone for a
/// bounded window without a session — the link IS the access grant.
pub fn signed_media_url(mount: &str, key: &str, ttl: std::time::Duration) -> String {
    let secret = umbral::settings::get_opt()
        .map(|s| s.secret_key.clone())
        .unwrap_or_default();
    let exp = (chrono::Utc::now()
        + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(0)))
    .timestamp();
    let sig = sign(&secret, key, exp);
    // Percent-encode each path segment of the key (keeps `/` separators).
    let encoded: Vec<String> = key
        .split('/')
        .map(|seg| percent_encoding::utf8_percent_encode(seg, KEY_SEGMENT).to_string())
        .collect();
    format!(
        "{}/{}?exp={}&sig={}",
        mount.trim_end_matches('/'),
        encoded.join("/"),
        exp,
        sig
    )
}

/// Verify a signed request: `exp` in the future AND `sig` matches
/// `sign(secret, key, exp)` in constant time. `query` is the raw request
/// query string; `key` is the already-decoded media key.
fn signed_request_ok(query: Option<&str>, key: &str) -> bool {
    let Some(query) = query else { return false };
    let mut exp: Option<i64> = None;
    let mut sig: Option<&str> = None;
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("exp", v)) => exp = v.parse().ok(),
            Some(("sig", v)) => sig = Some(v),
            _ => {}
        }
    }
    let (Some(exp), Some(sig)) = (exp, sig) else {
        return false;
    };
    if exp < chrono::Utc::now().timestamp() {
        return false; // expired
    }
    let secret = umbral::settings::get_opt()
        .map(|s| s.secret_key.clone())
        .unwrap_or_default();
    let expected = sign(&secret, key, exp);
    // Constant-time compare — a byte-by-byte `==` leaks where the first
    // mismatch is, which is a signature-forgery oracle.
    expected.as_bytes().ct_eq(sig.as_bytes()).into()
}

/// The composite allow decision for a media request, shared by the FS serve
/// path and the non-FS proxy. A valid SIGNED url grants access outright (the
/// signature IS the grant); otherwise the custom gate closure decides; a
/// signed-only mount with no valid signature denies.
pub(crate) async fn allows(
    signed_urls: bool,
    access: &Option<MediaAccessFn>,
    query: Option<&str>,
    headers: &http::HeaderMap,
    key: &str,
) -> bool {
    if signed_urls && signed_request_ok(query, key) {
        return true;
    }
    if let Some(access) = access {
        return access(headers, key).await;
    }
    // Signed-urls on but no valid signature, and no closure → deny.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_is_stable_and_key_exp_bound() {
        let a = sign("secret", "a/b.jpg", 100);
        assert_eq!(a, sign("secret", "a/b.jpg", 100), "deterministic");
        assert_ne!(a, sign("secret", "a/b.jpg", 101), "bound to exp");
        assert_ne!(a, sign("secret", "other.jpg", 100), "bound to key");
        assert_ne!(a, sign("other", "a/b.jpg", 100), "bound to secret");
    }

    #[test]
    fn key_for_path_decodes_and_strips_mount() {
        assert_eq!(key_for_path("/media/a%20b.jpg", "/media"), "a b.jpg");
        assert_eq!(key_for_path("/media/2026/r.txt", "/media"), "2026/r.txt");
    }

    // Without App::build the ambient secret is "", so sign() and
    // signed_request_ok() agree on the same empty secret — the round-trip
    // still holds and the key/exp binding is what's under test.
    fn query_for(key: &str, exp: i64) -> String {
        format!("exp={exp}&sig={}", sign("", key, exp))
    }

    #[test]
    fn signed_request_ok_accepts_a_valid_future_signature() {
        let exp = chrono::Utc::now().timestamp() + 300;
        assert!(signed_request_ok(Some(&query_for("f.jpg", exp)), "f.jpg"));
    }

    #[test]
    fn signed_request_ok_rejects_expired_tampered_and_missing() {
        let past = chrono::Utc::now().timestamp() - 1;
        assert!(
            !signed_request_ok(Some(&query_for("f.jpg", past)), "f.jpg"),
            "expired"
        );

        let future = chrono::Utc::now().timestamp() + 300;
        // Signature for a DIFFERENT key must not validate this one (the
        // classic "edit the path, keep the sig" forgery).
        let q = query_for("other.jpg", future);
        assert!(!signed_request_ok(Some(&q), "f.jpg"), "wrong key");

        // A flipped signature byte fails the constant-time compare.
        let mut tampered = query_for("f.jpg", future);
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'a' { 'b' } else { 'a' });
        assert!(!signed_request_ok(Some(&tampered), "f.jpg"), "tampered sig");

        assert!(!signed_request_ok(None, "f.jpg"), "no query");
        assert!(
            !signed_request_ok(Some("exp=9999999999"), "f.jpg"),
            "no sig param"
        );
    }

    #[test]
    fn signed_media_url_round_trips_through_the_verifier() {
        let url = signed_media_url("/media", "a b.jpg", std::time::Duration::from_secs(300));
        // Mount + percent-encoded key + query.
        assert!(url.starts_with("/media/a%20b.jpg?exp="));
        let query = url.split_once('?').unwrap().1;
        // The verifier sees the DECODED key.
        assert!(
            signed_request_ok(Some(query), "a b.jpg"),
            "a freshly minted url must verify; got {url}"
        );
    }
}
