//! Custom authentication backends for the shop example.
//!
//! Demonstrates two things on top of the framework primitives:
//!
//! 1. **`TokenSchemeAuthentication`** — a custom `Authentication`
//!    implementation that reads `Authorization: Token <key>` instead
//!    of the canonical `Bearer <key>`. Some legacy APIs (older
//!    token-auth schemes, GitHub's older PAT shape) use
//!    this prefix; the framework lets you plug it in without
//!    forking the bearer backend.
//!
//! 2. **`session_authentication()`** — a `FnAuthentication` that
//!    pulls the current `AuthUser` off the session cookie. This is
//!    the "escape hatch" path the auth.rs docs describe: a small
//!    closure does the work instead of a whole struct.
//!
//! Both produce the same `Identity` shape every other backend
//! produces, so the built-in permission classes (`IsAuthenticated`,
//! `IsStaff`, `OrPermission`, etc.) compose with them without any
//! extra glue.

use async_trait::async_trait;
use umbral::web::{HeaderMap, header};
use umbral_auth::{AuthUser, UserModel, auth_user, token::AuthToken};
use umbral_rest::{Authentication, FnAuthentication, Identity};

// ===========================================================================
// 1. Custom `Token <key>` scheme
// ===========================================================================

/// Same lookup as `umbral_auth::BearerAuthentication` but reads
/// `Authorization: Token <key>` instead of `Bearer <key>`.
///
/// Useful when integrating with API consumers that have hard-coded
/// the older "Token" prefix and you can't change them.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokenSchemeAuthentication;

impl TokenSchemeAuthentication {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self
    }
}

/// Parse `Authorization: Token <key>`. Returns the trimmed key on
/// match, `None` on a missing header, missing scheme, or malformed
/// value. Mirrors `umbral_auth::parse_bearer_header` for the
/// `Bearer` scheme.
fn parse_token_header(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = raw
        .strip_prefix("Token ")
        .or_else(|| raw.strip_prefix("token "))?;
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[async_trait]
impl Authentication for TokenSchemeAuthentication {
    async fn authenticate(&self, headers: &HeaderMap) -> Option<Identity> {
        let plaintext = parse_token_header(headers)?;
        let token = AuthToken::lookup(plaintext).await.ok().flatten()?;
        let user: AuthUser = AuthUser::objects()
            .filter(auth_user::ID.eq(token.user_id.id()) & auth_user::IS_ACTIVE.eq(true))
            .first()
            .await
            .ok()
            .flatten()?;
        // The `last_used_at` stat update on AuthToken is a crate-
        // private helper inside umbral-auth; an external custom auth
        // backend either skips the update (this demo) or reaches
        // for an ORM-level update on the row.
        //
        // `id_string()` is the polymorphic UserModel-level
        // stringifier — stays correct even if the app swaps
        // AuthUser for a custom user model with a non-i64 PK.
        Some(
            Identity::user(UserModel::id_string(&user))
                .with_staff(user.is_staff)
                .with_extra("auth", serde_json::json!("token-scheme")),
        )
    }

    fn security_scheme(&self) -> Option<(String, serde_json::Value)> {
        // OpenAPI doesn't have a first-class "Token" prefix, so we
        // describe it as a custom apiKey scheme in the Authorization
        // header — that way generated clients send the literal value
        // verbatim instead of trying to wrap it with `Bearer`.
        Some((
            "TokenAuth".to_string(),
            serde_json::json!({
                "type": "apiKey",
                "in": "header",
                "name": "Authorization",
                "description": "Custom token scheme. Header: `Authorization: Token <token>`."
            }),
        ))
    }
}

// ===========================================================================
// 2. Session-cookie authentication via FnAuthentication
// ===========================================================================

/// Build a session-cookie `Authentication`. Returns the `Identity`
/// for the user whose session is referenced by the cookie, or `None`
/// for an anonymous request.
///
/// This is the same shape `umbral-sessions` would ship as
/// `SessionAuthentication` — but writing it inline shows that
/// `FnAuthentication` is a one-liner escape hatch when you don't
/// want a whole new struct.
pub fn session_authentication() -> FnAuthentication {
    FnAuthentication::new(|headers| async move {
        let user = umbral_auth::current_user(&headers).await.ok().flatten()?;
        Some(
            Identity::user(user.id_string())
                .with_staff(user.is_staff())
                .with_extra("auth", serde_json::json!("session")),
        )
    })
}
