//! The `SocialAccount` model — one external identity linked to an
//! `AuthUser`.
//!
//! A social account is an **extension** of the user, never a
//! replacement: it FKs to `auth_user` by id and carries the provider's
//! identity plus its OAuth tokens. A user keeps their `username`; they
//! may have several social accounts (at most one per provider, enforced
//! by the `(provider, provider_uid)` unique constraint).
//!
//! Tokens are stored in [`Masked`] columns — encrypted at rest — so a
//! database dump never leaks a live access/refresh token.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;
use umbral_auth::AuthUser;

/// One linked external identity (a Google / GitHub / … account) for an
/// `AuthUser`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    plugin = "oauth",
    display = "Social accounts",
    icon = "link",
    unique_together = [["provider", "provider_uid"]]
)]
pub struct SocialAccount {
    pub id: i64,

    /// The umbral user this identity is linked to. Deleting the user
    /// deletes their social accounts.
    #[umbral(on_delete = "cascade")]
    pub user: ForeignKey<AuthUser>,

    /// Provider key, e.g. `"google"` / `"github"`. Matches the
    /// `OAuthProvider::key` of the provider that created the row.
    #[umbral(index, max_length = 40)]
    pub provider: String,

    /// The provider's stable unique id for this account (the OIDC `sub`,
    /// the GitHub numeric id, …). Unique within a provider.
    #[umbral(max_length = 255)]
    pub provider_uid: String,

    /// The email the provider reported, if any. Used for create-or-link.
    #[umbral(max_length = 320)]
    pub provider_email: Option<String>,

    /// Whether the provider asserts the email is verified. Email-based
    /// auto-linking only happens when this is true (anti-takeover).
    #[umbral(default = "false")]
    pub email_verified: bool,

    /// The OAuth access token — encrypted at rest.
    #[umbral(noform)]
    pub access_token: Masked<String>,

    /// The OAuth refresh token, if the provider issued one — encrypted
    /// at rest. `None` for providers / grants without refresh.
    #[umbral(noform)]
    pub refresh_token: Option<Masked<String>>,

    /// Space-separated granted scopes (e.g. the Drive scope once the app
    /// asks for API access beyond identity).
    #[umbral(max_length = 1000)]
    pub scopes: String,

    /// When the access token expires, if known.
    pub expires_at: Option<DateTime<Utc>>,

    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
}
