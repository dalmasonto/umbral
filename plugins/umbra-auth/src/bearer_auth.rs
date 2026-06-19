//! `BearerAuthentication` — the DRF-shape token authenticator.
//!
//! Reads `Authorization: Bearer <key>` from the request headers,
//! hashes the plaintext, looks the digest up in [`AuthToken`], then
//! hydrates an [`Identity`] from the owning [`AuthUser`].
//!
//! This class produces the same [`Identity`] shape every other
//! authentication backend produces, so the permission classes in
//! `umbra-rest` (`IsAuthenticated`, `IsStaff`, …) compose with it
//! without any extra code.
//!
//! ## Wiring
//!
//! ```ignore
//! use umbra_auth::BearerAuthentication;
//! use umbra_rest::RestPlugin;
//!
//! RestPlugin::default()
//!     .authenticate(BearerAuthentication::default())
//! ```
//!
//! Stack with `SessionAuthentication` if you want browsers to send
//! a cookie and curl to send a token:
//!
//! ```ignore
//! use umbra_rest::ChainAuthentication;
//! use std::sync::Arc;
//!
//! let auth = ChainAuthentication::new(vec![
//!     Arc::new(umbra_sessions::SessionAuthentication::default()),
//!     Arc::new(BearerAuthentication::default()),
//! ]);
//! RestPlugin::default().authenticate(auth);
//! ```
//!
//! ## What an unrecognised token does
//!
//! Returns `None`, the same as "no `Authorization` header at all".
//! The permission class then sees an anonymous request and produces
//! 401 / 403 as it would otherwise. The contract in `umbra-rest`
//! deliberately keeps `Authentication` from leaking "your token is
//! invalid" vs "you sent no token" — that would let an attacker
//! distinguish "the system knows about this user" from "the system
//! knows nothing about you", which is a credential-enumeration
//! leak.

use crate::{AuthUser, auth_user, token::AuthToken};
use async_trait::async_trait;
use umbra::web::{HeaderMap, header};
use umbra_rest::{Authentication, Identity};

/// The bearer-token authenticator. Stateless — every request reads
/// the header, looks up the token row, hydrates the user. The
/// per-request DB cost is two indexed lookups (token by digest, user
/// by PK) plus one best-effort `last_used_at` update.
#[derive(Debug, Default, Clone, Copy)]
pub struct BearerAuthentication;

impl BearerAuthentication {
    /// Convenience constructor; identical to `Default::default()`.
    pub fn new() -> Self {
        Self
    }
}

/// Parse `Authorization: Bearer <key>`. Returns the trimmed key on
/// match, `None` on a missing header, missing scheme, or malformed
/// value. Public so test code and custom auth backends can re-use
/// the same parser shape.
pub fn parse_bearer_header(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[async_trait]
impl Authentication for BearerAuthentication {
    async fn authenticate(&self, headers: &HeaderMap) -> Option<Identity> {
        let plaintext = parse_bearer_header(headers)?;
        let token = AuthToken::lookup(plaintext).await.ok().flatten()?;
        let user: AuthUser = AuthUser::objects()
            .filter(auth_user::ID.eq(token.user_id.id()) & auth_user::IS_ACTIVE.eq(true))
            .first()
            .await
            .ok()
            .flatten()?;
        // Best-effort stat update. `touch_last_used` swallows its
        // own errors, so awaiting inline can't downgrade the user
        // to anonymous on a transient DB hiccup. Per-request cost
        // is three indexed queries (token lookup, user select,
        // last_used_at update). If profiling later shows the
        // UPDATE on the hot path, this moves into a background
        // task — for now we keep umbra-auth tokio-free.
        token.touch_last_used().await;
        // `id_string()` so the polymorphic-PK refactor flows
        // through cleanly even if AuthUser is later swapped for
        // a custom user model (the trait method's default does
        // `Display`).
        Some(
            Identity::user(crate::UserModel::id_string(&user))
                .with_staff(user.is_staff)
                .with_superuser(user.is_superuser)
                .with_extra("auth", serde_json::json!("bearer")),
        )
    }

    fn security_scheme(&self) -> Option<(String, serde_json::Value)> {
        // Standard "bearer token in the Authorization header" shape.
        // `bearerFormat = "umbra"` signals our opaque-token format
        // (the `umbra_` prefix + 43 url-safe base64 chars) so
        // generated clients know not to try parsing as JWT.
        Some((
            "BearerAuth".to_string(),
            serde_json::json!({
                "type": "http",
                "scheme": "bearer",
                "bearerFormat": "umbra",
                "description": "umbra bearer token. Header: `Authorization: Bearer umbra_<token>`."
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use umbra::web::HeaderMap;

    fn h(value: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(header::AUTHORIZATION, value.parse().unwrap());
        m
    }

    #[test]
    fn parses_canonical_bearer() {
        assert_eq!(
            parse_bearer_header(&h("Bearer umbra_abc")),
            Some("umbra_abc")
        );
    }

    #[test]
    fn parses_lowercase_scheme() {
        // Some clients lowercase the scheme. RFC 7235 §2.1 says the
        // scheme is case-insensitive; we accept both casings.
        assert_eq!(
            parse_bearer_header(&h("bearer umbra_abc")),
            Some("umbra_abc")
        );
    }

    #[test]
    fn rejects_basic_scheme() {
        assert_eq!(parse_bearer_header(&h("Basic dXNlcjpwYXNz")), None);
    }

    #[test]
    fn rejects_missing_header() {
        let m = HeaderMap::new();
        assert_eq!(parse_bearer_header(&m), None);
    }

    #[test]
    fn rejects_empty_token() {
        assert_eq!(parse_bearer_header(&h("Bearer ")), None);
        assert_eq!(parse_bearer_header(&h("Bearer    ")), None);
    }
}
