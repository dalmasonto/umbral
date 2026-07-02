//! `Masked<T>` — an encrypt-at-rest field type for PII / secrets.
//!
//! ## What this is
//!
//! A `Masked<String>` is a `String`-shaped field that is stored
//! **encrypted** in the database (base64 ciphertext in a plain `TEXT`
//! column) and **redacted** everywhere it would otherwise leak — its
//! `Debug`, `Display`, and serde output are all `"••••••"`. The
//! plaintext is recoverable only through an explicit [`Masked::reveal`]
//! call, which needs the private key.
//!
//! The point is GDPR-style field encryption: mark a column
//! `phone: Masked<String>` and a stolen DB dump leaks ciphertext, not
//! phone numbers. `umbral-oauth` uses it for provider access/refresh
//! tokens.
//!
//! ## Crypto: anonymous sealed boxes (public-key encryption)
//!
//! Encryption uses an X25519 + XSalsa20-Poly1305 box (the RustCrypto
//! [`crypto_box`] primitive) in an **anonymous-sender** construction: a
//! fresh ephemeral keypair is generated per value, the box is sealed to
//! the configured *public* key, and the ephemeral public key + nonce are
//! prepended to the ciphertext. Anyone holding the public key can
//! encrypt; only the holder of the *private* key can decrypt. That
//! asymmetry buys two things:
//!
//! - a write-only tier can store PII it can never read, and
//! - **crypto-shredding**: destroying the private key renders every
//!   masked column permanently unrecoverable — a fast bulk erasure for
//!   "right to be forgotten".
//!
//! ## Keys
//!
//! The keyring is resolved once (ambient `OnceLock`, like the DB pool)
//! from `UMBRAL_MASK_PUBLIC_KEY` and the optional `UMBRAL_MASK_PRIVATE_KEY`
//! (both base64), or injected explicitly via [`set_mask_keyring`] (tests,
//! or an app that loads keys from a vault). Generate a keypair with
//! `cargo run -- maskkeygen`. Encryption needs only the public key;
//! [`Masked::reveal`] needs the private key and returns
//! [`MaskError::NoPrivateKey`] when it's absent.

use std::sync::OnceLock;

use base64::Engine as _;
use crypto_box::{
    SalsaBox,
    aead::{Aead, AeadCore},
};
use rand_core::OsRng;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const REDACTED: &str = "••••••";
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Errors from sealing / revealing a [`Masked`] value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaskError {
    /// No mask keyring is configured (neither `set_mask_keyring` was
    /// called nor `UMBRAL_MASK_PUBLIC_KEY` is set). Encryption can't run.
    NoKeyring,
    /// The keyring has a public key but no private key, so the value
    /// can be stored but not revealed. Set `UMBRAL_MASK_PRIVATE_KEY`.
    NoPrivateKey,
    /// A configured key was not valid base64 / not 32 bytes.
    BadKey(String),
    /// The stored ciphertext is malformed (too short / truncated).
    Malformed,
    /// Authenticated decryption failed (wrong key, or tampered data).
    Decrypt,
}

impl std::fmt::Display for MaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaskError::NoKeyring => f.write_str(
                "no mask keyring configured (set UMBRAL_MASK_PUBLIC_KEY or call set_mask_keyring)",
            ),
            MaskError::NoPrivateKey => f.write_str(
                "mask keyring has no private key; cannot reveal (set UMBRAL_MASK_PRIVATE_KEY)",
            ),
            MaskError::BadKey(why) => write!(f, "invalid mask key: {why}"),
            MaskError::Malformed => f.write_str("masked ciphertext is malformed"),
            MaskError::Decrypt => f.write_str("masked ciphertext failed to decrypt"),
        }
    }
}

impl std::error::Error for MaskError {}

// =========================================================================
// Keyring
// =========================================================================

/// A mask keyring: a recipient X25519 public key (always present) and an
/// optional secret key (present only on tiers that need to decrypt).
#[derive(Clone)]
pub struct MaskKeyring {
    public: crypto_box::PublicKey,
    secret: Option<crypto_box::SecretKey>,
}

impl MaskKeyring {
    /// Build a keyring from base64-encoded keys. The public key is
    /// required; the secret key is optional (a write-only tier omits it).
    pub fn from_base64(public_b64: &str, secret_b64: Option<&str>) -> Result<Self, MaskError> {
        let public = decode_key(public_b64)?;
        let public = crypto_box::PublicKey::from(public);
        let secret = match secret_b64 {
            Some(s) if !s.is_empty() => Some(crypto_box::SecretKey::from(decode_key(s)?)),
            _ => None,
        };
        Ok(Self { public, secret })
    }

    /// Read `UMBRAL_MASK_PUBLIC_KEY` (+ optional `UMBRAL_MASK_PRIVATE_KEY`)
    /// from the environment. Returns `Err(NoKeyring)` if the public key
    /// is unset.
    pub fn from_env() -> Result<Self, MaskError> {
        let public = std::env::var("UMBRAL_MASK_PUBLIC_KEY").map_err(|_| MaskError::NoKeyring)?;
        let secret = std::env::var("UMBRAL_MASK_PRIVATE_KEY").ok();
        Self::from_base64(&public, secret.as_deref())
    }

    /// Generate a fresh keypair, returned as `(public_b64, secret_b64)`
    /// for printing into env vars. Used by the `maskkeygen` command.
    pub fn generate() -> (String, String) {
        let secret = crypto_box::SecretKey::generate(&mut OsRng);
        let public = secret.public_key();
        (B64.encode(public.as_bytes()), B64.encode(secret.to_bytes()))
    }

    /// Seal `plaintext` to this keyring's public key, returning base64
    /// ciphertext. Needs only the public key.
    pub fn seal(&self, plaintext: &[u8]) -> String {
        // Anonymous-sender sealed box: ephemeral keypair per message, box
        // sealed to the recipient public key, ephemeral public key +
        // nonce prepended so the recipient can reconstruct the box.
        let eph_secret = crypto_box::SecretKey::generate(&mut OsRng);
        let eph_public = eph_secret.public_key();
        let salsa = SalsaBox::new(&self.public, &eph_secret);
        let nonce = SalsaBox::generate_nonce(&mut OsRng);
        let ciphertext = salsa
            .encrypt(&nonce, plaintext)
            .expect("XSalsa20-Poly1305 encryption is infallible for in-memory plaintext");
        let mut out = Vec::with_capacity(32 + nonce.len() + ciphertext.len());
        out.extend_from_slice(eph_public.as_bytes());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ciphertext);
        B64.encode(out)
    }

    /// Open base64 ciphertext produced by [`Self::seal`]. Needs the
    /// private key.
    pub fn open(&self, b64_ciphertext: &str) -> Result<String, MaskError> {
        let secret = self.secret.as_ref().ok_or(MaskError::NoPrivateKey)?;
        let sealed = B64
            .decode(b64_ciphertext)
            .map_err(|_| MaskError::Malformed)?;
        if sealed.len() < 32 + 24 {
            return Err(MaskError::Malformed);
        }
        let eph_public: [u8; 32] = sealed[..32].try_into().map_err(|_| MaskError::Malformed)?;
        let eph_public = crypto_box::PublicKey::from(eph_public);
        let nonce = crypto_box::Nonce::from_slice(&sealed[32..56]);
        let ciphertext = &sealed[56..];
        let salsa = SalsaBox::new(&eph_public, secret);
        let plaintext = salsa
            .decrypt(nonce, ciphertext)
            .map_err(|_| MaskError::Decrypt)?;
        String::from_utf8(plaintext).map_err(|_| MaskError::Decrypt)
    }

    /// Whether this keyring can decrypt (has a private key).
    pub fn can_reveal(&self) -> bool {
        self.secret.is_some()
    }
}

fn decode_key(b64: &str) -> Result<[u8; 32], MaskError> {
    let bytes = B64
        .decode(b64.trim())
        .map_err(|e| MaskError::BadKey(e.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| MaskError::BadKey("key is not 32 bytes".to_string()))
}

/// Ambient keyring state. Stores `Ok(Some(keyring))` when correctly
/// configured, `Ok(None)` when masking is simply not configured (the key
/// env-var is absent), and `Err(MaskError::BadKey(...))` when a key env-var
/// IS present but is malformed (bad base64 or wrong byte length). The third
/// state is the bug-fix: a malformed key must never silently resolve to
/// `None` and allow plaintext writes.
static KEYRING: OnceLock<Result<Option<MaskKeyring>, MaskError>> = OnceLock::new();

/// Inject the ambient mask keyring (tests, or an app loading keys from a
/// vault). Returns `false` if the keyring was already resolved. Mirrors
/// `crate::storage::set_storage`'s set-once discipline.
pub fn set_mask_keyring(keyring: MaskKeyring) -> bool {
    KEYRING.set(Ok(Some(keyring))).is_ok()
}

/// The ambient keyring, lazily resolved from the environment on first access
/// if not explicitly set.
///
/// Returns:
/// - `Ok(Some(kr))` — correctly configured, use `kr` to seal/open.
/// - `Ok(None)` — masking not configured (env-var absent); callers return
///   `Err(MaskError::NoKeyring)`.
/// - `Err(e)` — key IS present but malformed; callers propagate `e` so the
///   caller sees `BadKey(...)` rather than the misleading `NoKeyring`.
fn keyring() -> Result<Option<&'static MaskKeyring>, &'static MaskError> {
    KEYRING
        .get_or_init(|| match MaskKeyring::from_env() {
            Ok(k) => Ok(Some(k)),
            // No public key in the env → masking is simply not configured.
            // That's an expected, silent state.
            Err(MaskError::NoKeyring) => Ok(None),
            // A key IS present but couldn't be parsed. Store the error so
            // every subsequent seal/open returns BadKey, not the misleading
            // NoKeyring. Also log once so operators see it in the startup
            // logs without having to trigger an actual write.
            Err(e) => {
                tracing::error!(
                    "UMBRAL_MASK_PUBLIC_KEY/UMBRAL_MASK_PRIVATE_KEY is set but could not be \
                     parsed ({e}); all Masked<T> seal/reveal calls will fail with BadKey. \
                     Fix the key or unset the variable."
                );
                Err(e)
            }
        })
        .as_ref()
        .map(|opt| opt.as_ref())
        .map_err(|e| e)
}

/// Seal plaintext with the ambient keyring. `pub(crate)` so the write path
/// (`orm::write`) can seal a masked column supplied as a raw JSON/form string
/// on the dynamic REST/admin write paths, not just the typed `Serialize` path.
pub(crate) fn ambient_seal(plaintext: &str) -> Result<String, MaskError> {
    match keyring() {
        Ok(Some(k)) => Ok(k.seal(plaintext.as_bytes())),
        Ok(None) => Err(MaskError::NoKeyring),
        Err(e) => Err(e.clone()),
    }
}

/// Open ciphertext with the ambient keyring.
fn ambient_open(ciphertext: &str) -> Result<String, MaskError> {
    match keyring() {
        Ok(Some(k)) => k.open(ciphertext),
        Ok(None) => Err(MaskError::NoKeyring),
        Err(e) => Err(e.clone()),
    }
}

// =========================================================================
// Masked<T>
// =========================================================================

/// An encrypt-at-rest string field. Plaintext when freshly constructed,
/// ciphertext once loaded from the DB. Redacted in `Debug` / `Display` /
/// serde output; reveal the plaintext with [`Masked::reveal`].
///
/// The type parameter is currently fixed to `String` in practice
/// (`Masked<String>`); it exists so the API can widen to other revealed
/// types later without a breaking rename.
#[derive(Clone)]
pub struct Masked<T = String> {
    inner: MaskInner,
    _marker: std::marker::PhantomData<T>,
}

#[derive(Clone)]
enum MaskInner {
    /// In-memory plaintext that has not been sealed yet. Sealed on the
    /// write path.
    Plain(String),
    /// Base64 ciphertext, as stored in the DB. Decrypted lazily on
    /// `reveal()`.
    Sealed(String),
}

impl<T> Masked<T> {
    /// Construct from plaintext. The value is sealed when it's written to
    /// the database, not now.
    pub fn new(plaintext: impl Into<String>) -> Self {
        Self {
            inner: MaskInner::Plain(plaintext.into()),
            _marker: std::marker::PhantomData,
        }
    }

    /// Reveal the plaintext. For an in-memory value this is the value
    /// itself; for a value loaded from the DB this decrypts it (needs the
    /// private key).
    pub fn reveal(&self) -> Result<String, MaskError> {
        match &self.inner {
            MaskInner::Plain(p) => Ok(p.clone()),
            MaskInner::Sealed(c) => ambient_open(c),
        }
    }

    /// Whether the ambient keyring can reveal this value (has a private
    /// key). An in-memory plaintext is always revealable.
    pub fn is_revealable(&self) -> bool {
        match &self.inner {
            MaskInner::Plain(_) => true,
            MaskInner::Sealed(_) => keyring()
                .ok()
                .and_then(|opt| opt)
                .map(MaskKeyring::can_reveal)
                .unwrap_or(false),
        }
    }

    /// The stored representation — base64 ciphertext for a sealed value,
    /// or the freshly-sealed ciphertext for an in-memory value. This is
    /// what the sqlx `Encode` path binds.
    fn to_stored(&self) -> Result<String, MaskError> {
        match &self.inner {
            MaskInner::Plain(p) => ambient_seal(p),
            MaskInner::Sealed(c) => Ok(c.clone()),
        }
    }
}

impl<T> Default for Masked<T> {
    /// An empty masked value (empty plaintext). Encodes to a sealed empty
    /// string on write.
    fn default() -> Self {
        Masked::new(String::new())
    }
}

impl<T> std::fmt::Debug for Masked<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Masked(••••••)")
    }
}

impl<T> std::fmt::Display for Masked<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> From<String> for Masked<T> {
    fn from(plaintext: String) -> Self {
        Masked::new(plaintext)
    }
}

impl<T> From<&str> for Masked<T> {
    fn from(plaintext: &str) -> Self {
        Masked::new(plaintext)
    }
}

// ---- serde: redact on the way out, treat input as plaintext ----

impl<T> Serialize for Masked<T> {
    /// Serialize as the **stored ciphertext** (sealing in-memory plaintext
    /// first). This is load-bearing: the ORM write path binds values via
    /// `serde_json::to_value(instance)`, so the serialized form *is* what
    /// lands in the column — it must be ciphertext, not plaintext and not
    /// the redaction marker. The plaintext therefore never leaves the
    /// process through serde; a REST response carries an opaque encrypted
    /// blob (hide the field with the REST serializer's `.hide(...)` for a
    /// clean response). `Debug` / `Display` stay redacted for logs.
    ///
    /// Fails the serialize if no keyring is configured — that surfaces a
    /// missing `UMBRAL_MASK_PUBLIC_KEY` loudly at write time instead of
    /// silently storing plaintext.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let stored = self.to_stored().map_err(serde::ser::Error::custom)?;
        s.serialize_str(&stored)
    }
}

impl<'de, T> Deserialize<'de> for Masked<T> {
    /// A JSON / form string is read as new plaintext (to be sealed on
    /// write). The redaction marker round-trips to an empty value so a
    /// REST client echoing a redacted field back doesn't overwrite the
    /// stored ciphertext with `"••••••"`.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s == REDACTED {
            // Echoed-back redaction: treat as "no change" → empty plain.
            Ok(Masked::new(String::new()))
        } else {
            Ok(Masked::new(s))
        }
    }
}

// ---- sqlx: encrypt on encode, store ciphertext on decode ----

macro_rules! impl_masked_sqlx {
    ($db:ty, $valueref:ty, $argbuf:ty) => {
        impl<T> sqlx::Type<$db> for Masked<T> {
            fn type_info() -> <$db as sqlx::Database>::TypeInfo {
                <String as sqlx::Type<$db>>::type_info()
            }
            fn compatible(ty: &<$db as sqlx::Database>::TypeInfo) -> bool {
                <String as sqlx::Type<$db>>::compatible(ty)
            }
        }

        impl<'r, T> sqlx::Decode<'r, $db> for Masked<T> {
            fn decode(value: $valueref) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                let ciphertext = <String as sqlx::Decode<$db>>::decode(value)?;
                Ok(Masked {
                    inner: MaskInner::Sealed(ciphertext),
                    _marker: std::marker::PhantomData,
                })
            }
        }

        impl<'q, T> sqlx::Encode<'q, $db> for Masked<T> {
            fn encode_by_ref(
                &self,
                buf: &mut $argbuf,
            ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
                let stored = self.to_stored()?;
                <String as sqlx::Encode<'q, $db>>::encode_by_ref(&stored, buf)
            }
        }
    };
}

impl_masked_sqlx!(
    sqlx::Sqlite,
    sqlx::sqlite::SqliteValueRef<'r>,
    <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>
);
impl_masked_sqlx!(
    sqlx::Postgres,
    sqlx::postgres::PgValueRef<'r>,
    <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'q>
);

#[cfg(test)]
mod tests {
    use super::*;

    fn test_keyring() -> MaskKeyring {
        let (public, secret) = MaskKeyring::generate();
        MaskKeyring::from_base64(&public, Some(&secret)).unwrap()
    }

    #[test]
    fn seal_open_round_trips() {
        let kr = test_keyring();
        let sealed = kr.seal(b"+254712345678");
        assert_ne!(sealed, "+254712345678", "stored form is not plaintext");
        assert_eq!(kr.open(&sealed).unwrap(), "+254712345678");
    }

    #[test]
    fn each_seal_is_distinct_ciphertext() {
        // Fresh ephemeral keypair + nonce per call → two encryptions of
        // the same plaintext differ, but both decrypt to it.
        let kr = test_keyring();
        let a = kr.seal(b"secret");
        let b = kr.seal(b"secret");
        assert_ne!(a, b, "ephemeral keypair makes ciphertext non-deterministic");
        assert_eq!(kr.open(&a).unwrap(), "secret");
        assert_eq!(kr.open(&b).unwrap(), "secret");
    }

    #[test]
    fn public_key_only_cannot_open() {
        let (public, secret) = MaskKeyring::generate();
        let write_only = MaskKeyring::from_base64(&public, None).unwrap();
        let sealed = write_only.seal(b"pii");
        assert_eq!(write_only.open(&sealed), Err(MaskError::NoPrivateKey));
        // The full keyring (with the private key) can still read it.
        let full = MaskKeyring::from_base64(&public, Some(&secret)).unwrap();
        assert_eq!(full.open(&sealed).unwrap(), "pii");
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let a = test_keyring();
        let b = test_keyring();
        let sealed = a.seal(b"private");
        assert_eq!(b.open(&sealed), Err(MaskError::Decrypt));
    }

    #[test]
    fn masked_redacts_in_debug_and_display() {
        // Logs never leak: Debug + Display are redacted with no keyring
        // involved at all.
        let m: Masked = Masked::new("0712-secret");
        assert_eq!(m.to_string(), REDACTED, "Display is redacted");
        assert!(format!("{m:?}").contains("••••••"), "Debug is redacted");
    }

    #[test]
    fn serialize_emits_ciphertext_not_plaintext() {
        // The ORM write path binds via `serde_json::to_value`, so serde
        // must emit the sealed ciphertext (not plaintext, not the
        // redaction marker). This needs the ambient keyring.
        let (public, secret) = MaskKeyring::generate();
        set_mask_keyring(MaskKeyring::from_base64(&public, Some(&secret)).unwrap());
        let m: Masked = Masked::new("0712-secret");
        let json = serde_json::to_string(&m).unwrap();
        assert!(
            !json.contains("0712-secret"),
            "serialized form must not be the plaintext"
        );
        assert_ne!(
            json,
            format!("\"{REDACTED}\""),
            "serialized form is ciphertext, not the redaction marker"
        );
    }

    #[test]
    fn in_memory_plaintext_reveals_without_keyring() {
        // A freshly-constructed Masked is plaintext in memory: reveal
        // returns it directly, no decryption (so no keyring needed).
        let m: Masked = Masked::new("hello");
        assert_eq!(m.reveal().unwrap(), "hello");
    }
}
