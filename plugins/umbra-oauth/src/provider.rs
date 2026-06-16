//! The `OAuthProvider` abstraction — what every social provider must
//! supply so the flow routes stay provider-agnostic.
//!
//! A provider does three things: build the authorize URL to redirect the
//! user to, exchange the returned `code` for tokens, and fetch the
//! identity (uid + email) those tokens represent. Google and GitHub
//! implement this trait (see `providers/`); a third party adds a new
//! login by implementing it too.

use async_trait::async_trait;

/// Tokens returned by a provider's token endpoint.
#[derive(Clone)]
pub struct TokenSet {
    /// The OAuth access token (bearer for API calls + the identity fetch).
    pub access_token: String,
    /// The refresh token, if the provider issued one.
    pub refresh_token: Option<String>,
    /// Seconds until the access token expires, if reported.
    pub expires_in: Option<i64>,
    /// Space-separated granted scopes.
    pub scopes: String,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"***")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "***"))
            .field("expires_in", &self.expires_in)
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// The identity a provider resolves a [`TokenSet`] to.
#[derive(Debug, Clone)]
pub struct Identity {
    /// The provider's stable unique id for the account (OIDC `sub`,
    /// GitHub numeric id, …).
    pub uid: String,
    /// The account's email, if the provider exposes one.
    pub email: Option<String>,
    /// Whether the provider asserts the email is verified. Gates
    /// email-based auto-linking.
    pub email_verified: bool,
    /// A human display name, if available (used when auto-provisioning).
    pub display_name: Option<String>,
}

/// Errors from an OAuth exchange / identity fetch.
#[derive(Debug)]
pub enum OAuthError {
    /// The provider isn't registered on this `OAuthPlugin`.
    UnknownProvider(String),
    /// The provider has no client id / secret configured.
    NotConfigured(String),
    /// The `state` parameter didn't match the one in the session
    /// (CSRF / forged callback).
    StateMismatch,
    /// A network / HTTP error talking to the provider.
    Http(String),
    /// The provider returned an error or an unparseable response.
    Provider(String),
    /// A database error persisting the linked account / user.
    Database(String),
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthError::UnknownProvider(p) => write!(f, "unknown oauth provider `{p}`"),
            OAuthError::NotConfigured(p) => write!(f, "oauth provider `{p}` is not configured"),
            OAuthError::StateMismatch => f.write_str("oauth state mismatch (possible CSRF)"),
            OAuthError::Http(e) => write!(f, "oauth http error: {e}"),
            OAuthError::Provider(e) => write!(f, "oauth provider error: {e}"),
            OAuthError::Database(e) => write!(f, "oauth database error: {e}"),
        }
    }
}

impl std::error::Error for OAuthError {}

/// One social provider. Implementations are stateless apart from the
/// client credentials they're constructed with.
#[async_trait]
pub trait OAuthProvider: Send + Sync {
    /// The provider key, e.g. `"google"`. Used in route paths
    /// (`/oauth/<key>/login`) and stored on `SocialAccount.provider`.
    fn key(&self) -> &'static str;

    /// A human label for buttons / admin, e.g. `"Google"`.
    fn label(&self) -> &'static str;

    /// Build the URL to redirect the user to, carrying the CSRF `state`
    /// and the callback `redirect_uri`.
    fn authorize_url(&self, state: &str, redirect_uri: &str) -> String;

    /// Exchange an authorization `code` for tokens.
    async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<TokenSet, OAuthError>;

    /// Resolve a token set to the account identity.
    async fn fetch_identity(&self, tokens: &TokenSet) -> Result<Identity, OAuthError>;
}
