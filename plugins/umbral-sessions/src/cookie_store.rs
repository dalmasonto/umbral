//! `CookieStore` — a stateless, AEAD-encrypted session-in-cookie
//! [`SessionStore`] (Phase 2b).
//!
//! ## Design
//!
//! `DbStore` keeps the session row server-side and puts only an opaque token
//! in the cookie. `CookieStore` inverts that: there is NO server row at all.
//! The entire [`SessionRecord`] is serialised, encrypted, and stuffed into the
//! cookie value. Every request decrypts the cookie to recover the session —
//! zero DB round-trip on load OR save.
//!
//! ```text
//!   save:  record --serde_json--> bytes --XChaCha20Poly1305--> ciphertext
//!          cookie = base64url( nonce(24) || ciphertext )
//!   load:  base64url-decode -> split nonce||ct -> decrypt -> serde_json
//! ```
//!
//! ## Confidentiality + integrity
//!
//! XChaCha20Poly1305 is an AEAD cipher: the same operation that encrypts also
//! authenticates. A tampered or forged cookie fails the Poly1305 tag check on
//! decrypt, and `load` reports it as "no session" (`Ok(None)`) rather than
//! erroring the request — a client that hands us garbage is treated exactly
//! like a client with no cookie.
//!
//! The 256-bit key is derived from the app `secret_key` via SHA-256. A
//! stateless cookie session with an EMPTY (or insecure-default) key is
//! trivially forgeable (anyone can mint a valid-looking session). Two guards
//! cover this: `SessionsPlugin::on_ready` HARD-FAILS boot in `Prod` when a
//! secret-derived `CookieStore` is the active store and `secret_key` is empty
//! or the dev default (see `requires_ambient_secret`); and, independent of the
//! plugin path, `resolve_ambient_key` surfaces the empty-key state at first use
//! with a `tracing::warn!`/`error!` so it's visible in logs even in dev.
//!
//! ## Why XChaCha20Poly1305 and a random nonce
//!
//! The 24-byte (192-bit) nonce is large enough to pick at RANDOM for every
//! save with a negligible collision probability — no per-server counter to
//! persist, which a stateless store can't keep anyway. The classic
//! ChaCha20Poly1305 (96-bit nonce) would require nonce management we have no
//! place to store; XChaCha's extended nonce is purpose-built for this.

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::SessionError;
use crate::store::{SessionRecord, SessionStore};

/// Max encoded cookie size. Browsers cap a single cookie around 4 KB
/// (name + value + attributes); we budget the VALUE at 4096 bytes so the
/// whole `Set-Cookie` (with `HttpOnly; Secure; SameSite; Path; Max-Age`)
/// stays under the limit. A `save` that would exceed this fails loudly with
/// [`SessionError::CookieTooLarge`] rather than emitting a cookie the browser
/// silently drops.
const MAX_COOKIE_VALUE_BYTES: usize = 4096;

/// XChaCha20Poly1305 nonce length (bytes). 192-bit extended nonce — safe to
/// pick at random per save.
const NONCE_LEN: usize = 24;

/// Stateless, AEAD-encrypted session store. Holds the derived 256-bit key (or
/// resolves it lazily from the ambient `secret_key` on first use) and nothing
/// else; there is no DB handle because it never touches the database.
///
/// ## Why the key is resolved lazily
///
/// The documented wiring is `SessionsPlugin::default().store(CookieStore::new())`,
/// and that whole expression is evaluated as an argument to `App::builder()`
/// — i.e. BEFORE `App::build()` installs the ambient settings. If `new()`
/// captured `SHA-256(secret_key)` eagerly it would key off the EMPTY pre-boot
/// secret, then every request (which runs after boot, with the real secret)
/// would mint cookies under a different key. So a key derived from the ambient
/// secret is resolved on first cipher use (cached in a `OnceLock`), by which
/// point boot has run and the real `secret_key` is in place. An explicit key
/// from [`with_secret`] is pinned immediately (tests want determinism).
///
/// `Clone` is cheap. `Debug` is implemented by hand so the key bytes NEVER
/// appear in logs or panic output.
#[derive(Clone)]
pub struct CookieStore {
    /// `Some` when an explicit key was pinned via [`with_secret`]; `None` when
    /// the key is derived lazily from the ambient `secret_key` on first use.
    explicit_key: Option<[u8; 32]>,
    /// Lazily-derived ambient key cache. Only consulted when `explicit_key`
    /// is `None`. `Arc<OnceLock<_>>` so `Clone`d stores share the same cell.
    ambient_key: std::sync::Arc<std::sync::OnceLock<[u8; 32]>>,
}

impl std::fmt::Debug for CookieStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately opaque: never render the key.
        f.debug_struct("CookieStore")
            .field("key", &"<redacted 32-byte AEAD key>")
            .field(
                "source",
                &if self.explicit_key.is_some() {
                    "explicit"
                } else {
                    "ambient"
                },
            )
            .finish()
    }
}

/// Derive the 32-byte cipher key from a secret: `SHA-256(secret)`.
fn derive_key(secret: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

impl CookieStore {
    /// Build a `CookieStore` whose key is derived from the ambient app
    /// `secret_key`, resolved LAZILY on first cipher use (see the type doc for
    /// why eager resolution would break the builder-time wiring).
    ///
    /// The empty-key check also runs lazily, at the first load/save, by which
    /// point settings are installed: it warns in dev/test and error-shouts in
    /// prod. The actual hard-fail happens earlier, at boot, in the
    /// [`crate::SessionsPlugin`] `on_ready` check (which refuses to boot a
    /// secret-derived `CookieStore` on an empty / dev-default secret in `Prod`).
    pub fn new() -> Self {
        Self {
            explicit_key: None,
            ambient_key: std::sync::Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Construct from an explicit secret (the key is `SHA-256(secret)`),
    /// pinned immediately. Exposed for tests that want a deterministic key
    /// without the ambient-settings dance.
    pub fn with_secret(secret: &str) -> Self {
        Self {
            explicit_key: Some(derive_key(secret)),
            ambient_key: std::sync::Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Resolve the key from the ambient `secret_key`, deriving + caching it on
    /// first call. Emits the empty-key warning/error once, at resolution time.
    fn resolve_ambient_key(&self) -> [u8; 32] {
        *self.ambient_key.get_or_init(|| {
            let secret = umbral::settings::get_opt()
                .map(|s| s.secret_key.clone())
                .unwrap_or_default();

            if secret.trim().is_empty() {
                // Error-shout in prod, warn elsewhere. The hard-fail lives in
                // SessionsPlugin::on_ready (boot-time); this lazy log is the
                // in-request backstop for paths that skip the plugin boot check
                // (e.g. a store used directly in a test) — make the danger
                // impossible to miss in logs.
                match umbral::settings::get_opt().map(|s| &s.environment) {
                    Some(umbral::Environment::Prod) => {
                        tracing::error!(
                            "CookieStore: secret_key is EMPTY in production. Stateless cookie \
                             sessions are encrypted/authenticated with a key derived from an \
                             empty secret — they are TRIVIALLY FORGEABLE. Set `secret_key` in \
                             umbral.toml or via UMBRAL_SECRET_KEY before deploying."
                        );
                    }
                    _ => {
                        tracing::warn!(
                            "CookieStore: secret_key is empty — cookie sessions are derived \
                             from an empty key and are forgeable. Set `secret_key` before \
                             deploying."
                        );
                    }
                }
            }

            derive_key(&secret)
        })
    }

    /// The cipher instance for this store's key (explicit or lazily-ambient).
    fn cipher(&self) -> XChaCha20Poly1305 {
        let key = self
            .explicit_key
            .unwrap_or_else(|| self.resolve_ambient_key());
        XChaCha20Poly1305::new((&key).into())
    }
}

impl Default for CookieStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SessionStore for CookieStore {
    /// Decrypt the cookie value back into a [`SessionRecord`].
    ///
    /// Any failure along the chain — base64 decode, too-short blob, AEAD
    /// auth/decrypt failure (tampered or forged cookie), or malformed JSON —
    /// is reported as `Ok(None)` (no session), NOT an error. A bad cookie is
    /// indistinguishable from no cookie as far as the request is concerned.
    /// A successfully decrypted but EXPIRED record also yields `Ok(None)`
    /// (lazy expiry — there's no server row to delete).
    async fn load(&self, cookie_value: &str) -> Result<Option<SessionRecord>, SessionError> {
        // base64url-decode. Bad encoding -> no session.
        let blob = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(cookie_value) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };

        // Need at least the nonce plus a non-empty ciphertext+tag.
        if blob.len() <= NONCE_LEN {
            return Ok(None);
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = XNonce::from_slice(nonce_bytes);

        // AEAD decrypt. A tampered byte (in nonce OR ciphertext OR tag) fails
        // the Poly1305 check here -> treated as no session.
        let plaintext = match self.cipher().decrypt(nonce, ciphertext) {
            Ok(pt) => pt,
            Err(_) => return Ok(None),
        };

        // Recover the record. Malformed JSON -> no session.
        let record: SessionRecord = match serde_json::from_slice(&plaintext) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };

        // Lazy expiry: a past-due record is "no session".
        if record.expires_at < chrono::Utc::now() {
            return Ok(None);
        }

        Ok(Some(record))
    }

    /// Encrypt the record into a fresh cookie value and return it.
    ///
    /// `_token` is ignored — a stateless store derives nothing from the token
    /// (the cookie IS the session). Every save picks a fresh random nonce, so
    /// the same record encrypts to a different blob each time (which is fine;
    /// the browser just stores whatever we hand back).
    ///
    /// Fails with [`SessionError::CookieTooLarge`] if the encoded value would
    /// exceed [`MAX_COOKIE_VALUE_BYTES`] — better a loud error than a cookie
    /// the browser silently drops.
    async fn save(&self, _token: &str, record: &SessionRecord) -> Result<String, SessionError> {
        let plaintext = serde_json::to_vec(record)?;

        // Fresh random 24-byte nonce per save (XChaCha's extended nonce makes
        // random selection safe — no counter to keep).
        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom_fill(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher()
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|_| {
                // Encryption itself shouldn't fail for a sane plaintext; if it
                // does, surface it as an over-limit error rather than panic.
                SessionError::CookieTooLarge(plaintext.len())
            })?;

        // nonce || ciphertext, then base64url.
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&blob);

        if encoded.len() > MAX_COOKIE_VALUE_BYTES {
            return Err(SessionError::CookieTooLarge(encoded.len()));
        }

        Ok(encoded)
    }

    /// No-op: there is no server row to delete. Logout clears the cookie via
    /// the existing clear-cookie path in `session_layer` / the logout helper.
    async fn destroy(&self, _token: &str) -> Result<(), SessionError> {
        Ok(())
    }

    /// A `CookieStore` that derives its key from the ambient `secret_key`
    /// (i.e. built via [`CookieStore::new`], not [`CookieStore::with_secret`])
    /// is only as unforgeable as that secret. Report that dependency so the
    /// `SessionsPlugin` boot check can hard-fail an empty / insecure-default
    /// secret in production. An explicit-key store carries its own key and does
    /// not depend on the ambient secret.
    fn requires_ambient_secret(&self) -> bool {
        self.explicit_key.is_none()
    }
}

/// Fill `buf` with cryptographically-strong random bytes from the OS CSPRNG.
///
/// We use the `getrandom` crate directly rather than relying on a transitive
/// `aead`/`OsRng` feature so the nonce source is explicit and doesn't break if
/// the cipher crate's default features change. `getrandom` only fails when the
/// OS entropy source is unavailable, which is unrecoverable for a security
/// primitive — panic with a clear message rather than mint a predictable
/// nonce.
fn getrandom_fill(buf: &mut [u8]) {
    getrandom::getrandom(buf)
        .expect("CookieStore: OS CSPRNG unavailable — cannot mint a session nonce");
}
