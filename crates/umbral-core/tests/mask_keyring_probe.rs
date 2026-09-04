//! gaps4 #93 (trap 1): `mask_keyring_configured()` is the boot-time probe a
//! plugin uses in `on_ready` to warn that a masked write (e.g. the OAuth
//! callback sealing provider tokens) will fail before a keyring is set.
//!
//! Its own binary because the mask `KEYRING` is a process-global `OnceLock`:
//! only a fresh process guarantees it starts unresolved, so the "not
//! configured → then configured" transition is observable deterministically.

use umbral_core::orm::{MaskKeyring, mask_keyring_configured, set_mask_keyring};

#[test]
fn probe_is_false_before_and_true_after_set() {
    // A fresh process with no UMBRAL_MASK_PUBLIC_KEY in the env: the probe
    // must report "not configured" without forcing the lazy OnceLock to
    // resolve (which would prevent the set_mask_keyring below from winning).
    assert!(
        std::env::var("UMBRAL_MASK_PUBLIC_KEY").is_err(),
        "this test assumes UMBRAL_MASK_PUBLIC_KEY is unset in the test env"
    );
    assert!(
        !mask_keyring_configured(),
        "no keyring set and no env key → probe reports not configured"
    );

    // Inject a keyring the way an app loading keys from a vault would.
    let (public_b64, secret_b64) = MaskKeyring::generate();
    let kr = MaskKeyring::from_base64(&public_b64, Some(&secret_b64)).expect("valid generated key");
    assert!(set_mask_keyring(kr), "keyring set once on a fresh process");

    assert!(
        mask_keyring_configured(),
        "after set_mask_keyring the probe reports configured"
    );
}
