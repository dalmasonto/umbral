//! Opaque DB-backed bearer tokens.
//!
//! A user can hold any number of named tokens (laptop, CI, iOS, …).
//! Each token is a long random string with a `umbral_` prefix; only
//! its SHA-256 digest hits the database. Plaintext is shown ONCE at
//! creation; lookups go plaintext → digest → row, so a database leak
//! does not surrender live tokens.
//!
//! ## Lifecycle
//!
//! 1. [`AuthToken::create_for`] generates a token, persists the
//!    digest, returns the model row + the plaintext key.
//! 2. The caller stores the plaintext somewhere (cookie, response
//!    body, env file). The plaintext is never recoverable from the
//!    row alone.
//! 3. Every request that authenticates via `Authorization: Bearer
//!    <key>` runs through [`AuthToken::lookup`]: digest the bearer,
//!    look up by the unique `key_hash` index, return the row.
//! 4. [`AuthToken::revoke`] deletes the row; the next request with
//!    that plaintext fails the lookup and the caller is treated as
//!    anonymous.
//!
//! ## Why hash at rest
//!
//! Classic token tables store plaintext. The modern recommendation is to
//! hash the token so a DB read leak (backup, SQL injection, exposed dump)
//! doesn't hand
//! the attacker live API keys. The lookup cost is one SHA-256 per
//! request, which is microseconds. The only ergonomic loss is "show
//! me my token again" — which is the security boundary working as
//! intended.
//!
//! ## Custom user models
//!
//! `AuthToken` FKs against [`crate::AuthUser`]. Apps using a custom
//! `UserModel` need their own token model + their own
//! `Authentication` impl. The [`crate::BearerAuthentication`] class
//! and this model are convenience defaults for the built-in user.

use crate::AuthUser;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use umbral::orm::ForeignKey;

/// The token prefix. Lets a developer eyeball that a 50-char string
/// is an umbral bearer token (the same trick GitHub uses with `ghp_`).
/// Also lets log scrubbers grep for accidentally-committed tokens.
pub const TOKEN_PREFIX: &str = "umbral_";

/// One row per active bearer token. A user can hold any number of
/// these (one per device / per environment / per CI runner). The
/// `name` column is the human label shown in admin / management
/// listings; it has no functional role.
///
/// The `key_hash` column carries `base64(sha256(plaintext))` (43
/// chars, URL-safe, no padding) under a UNIQUE index so a digest
/// collision is forbidden. The plaintext lives only in memory at
/// creation time and in whatever client storage the caller chose.
///
/// `last_used_at` is updated best-effort on every successful lookup
/// (a failure to write does not fail the auth). Useful for "this
/// token has not been used in 90 days, prune it" cleanup jobs.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct AuthToken {
    pub id: i64,
    /// Owning user. FK against `auth_user.id`. `ON DELETE CASCADE`
    /// — when a user row is deleted, every token they hold goes
    /// with them. Otherwise revoking a user would leave orphan
    /// tokens that no longer match any user via the auth lookup,
    /// silently failing 401 instead of cleanly disappearing.
    #[umbral(on_delete = "cascade")]
    pub user_id: ForeignKey<AuthUser>,
    /// `base64(sha256(plaintext))` — 43 chars, URL-safe, no pad. The
    /// UNIQUE constraint protects against the (cryptographically
    /// negligible) chance of two random keys hashing to the same
    /// digest, and lets the lookup path stop at the first match.
    #[umbral(max_length = 64, unique)]
    pub key_hash: String,
    /// Human label. Shown in admin listings and the management
    /// CLI; never used for lookup. Defaults to "default" when the
    /// caller does not name the token.
    #[umbral(max_length = 80)]
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// Last time this token authenticated a request. NULL until
    /// the first successful lookup.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// The plaintext key returned at creation time. Wraps the raw
/// string so a caller sees the type and remembers this value is
/// not recoverable from the database.
#[derive(Debug, Clone)]
pub struct PlaintextToken(pub String);

impl std::fmt::Display for PlaintextToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Generate a random opaque bearer token. 32 bytes of OS-provided
/// randomness, URL-safe base64 encoded, with the `umbral_` prefix.
/// The final string is ~50 chars and contains no `=` padding so it
/// drops straight into `Authorization: Bearer …` without escaping.
fn generate_plaintext() -> String {
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(buf))
}

/// SHA-256 the plaintext and encode the digest in URL-safe base64.
/// Public so a caller running `AuthToken::lookup` against a custom
/// query can compute the storage form themselves.
pub fn digest_token(plaintext: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

impl AuthToken {
    /// Mint a new bearer token for `user`. Returns the persisted
    /// row plus the plaintext key — the caller is responsible for
    /// surfacing the plaintext to whoever needs it (response body,
    /// admin "copy this" UI). The plaintext is not recoverable from
    /// the row alone after this call returns.
    ///
    /// `name` is a free-form label shown in admin and management
    /// listings. Pass `""` to default to `"default"`.
    pub async fn create_for(
        user: &AuthUser,
        name: &str,
    ) -> Result<(Self, PlaintextToken), crate::AuthError> {
        let plaintext = generate_plaintext();
        let key_hash = digest_token(&plaintext);
        let label = if name.is_empty() { "default" } else { name };
        let row = AuthToken::objects()
            .create(AuthToken {
                id: 0, // ignored; the ORM assigns
                user_id: ForeignKey::new(user.id),
                key_hash,
                name: label.to_string(),
                created_at: Utc::now(),
                last_used_at: None,
            })
            .await?;
        Ok((row, PlaintextToken(plaintext)))
    }

    /// Look up the token row a plaintext key resolves to. Returns
    /// `Ok(None)` for an unrecognised token (the auth backend will
    /// then treat the request as anonymous); `Err` only on a DB
    /// failure.
    pub async fn lookup(plaintext: &str) -> Result<Option<Self>, crate::AuthError> {
        let key_hash = digest_token(plaintext);
        let row = AuthToken::objects()
            .filter(auth_token::KEY_HASH.eq(key_hash))
            .first()
            .await?;
        Ok(row)
    }

    /// Revoke this token by deleting its row. The next request that
    /// carries this plaintext fails [`AuthToken::lookup`] and is
    /// treated as anonymous.
    pub async fn revoke(&self) -> Result<(), crate::AuthError> {
        AuthToken::objects()
            .filter(auth_token::ID.eq(self.id))
            .delete()
            .await?;
        Ok(())
    }

    /// How stale `last_used_at` must be before a bump actually writes.
    ///
    /// gaps4 #15: without this, every authenticated bearer request issued a
    /// `last_used_at` UPDATE — a write on every read, and the hottest write in
    /// a token-auth API. The column exists so cleanup jobs can prune stale
    /// tokens; minute-granularity is ample for that, and coalescing turns
    /// "a write per request" into "at most one write per minute per token".
    const TOUCH_COALESCE: chrono::Duration = chrono::Duration::seconds(60);

    /// Best-effort `last_used_at` bump. Called by
    /// [`crate::BearerAuthentication`] after a successful lookup so
    /// the column is fresh for cleanup jobs. Failures here are
    /// swallowed — a stat update should never fail the request that
    /// triggered it.
    ///
    /// Coalesced (gaps4 #15): the write is skipped when the stored value is
    /// already within [`Self::TOUCH_COALESCE`] of now, so a busy token isn't
    /// re-written on every request.
    pub(crate) async fn touch_last_used(&self) {
        let now = Utc::now();
        if let Some(prev) = self.last_used_at {
            if now.signed_duration_since(prev) < Self::TOUCH_COALESCE {
                return;
            }
        }
        let mut delta = serde_json::Map::new();
        delta.insert("last_used_at".to_string(), serde_json::json!(now));
        let _ = AuthToken::objects()
            .filter(auth_token::ID.eq(self.id))
            .update_values(delta)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_plaintext_has_prefix_and_decent_length() {
        let t = generate_plaintext();
        assert!(t.starts_with(TOKEN_PREFIX), "missing prefix: {t}");
        // prefix (6) + base64(32 bytes, no padding) = 6 + 43 = 49
        assert_eq!(t.len(), TOKEN_PREFIX.len() + 43, "unexpected length: {t}");
    }

    #[test]
    fn generated_plaintext_is_unique_per_call() {
        let a = generate_plaintext();
        let b = generate_plaintext();
        assert_ne!(
            a, b,
            "two consecutive tokens collided (statistically impossible)"
        );
    }

    #[test]
    fn digest_is_deterministic_and_unique() {
        let a = digest_token("umbral_AAAAA");
        let b = digest_token("umbral_AAAAA");
        let c = digest_token("umbral_BBBBB");
        assert_eq!(a, b, "digest is supposed to be deterministic");
        assert_ne!(a, c, "different inputs must produce different digests");
        // Base64 of 32 bytes, no padding -> 43 chars.
        assert_eq!(a.len(), 43, "unexpected digest length: {a}");
    }
}
