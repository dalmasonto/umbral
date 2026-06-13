//! `umbra-oauth` — OAuth / social authentication for umbra, the
//! django-allauth equivalent.
//!
//! Layered on `umbra-auth`: it adds a `SocialAccount` table that links
//! external identities (Google, GitHub, …) to an `AuthUser` **without
//! replacing the username** — a social account is an extension row, and
//! a user can link several. The same flow does double duty:
//!
//! - **Social login** — "Sign in with Google" resolves (or creates) an
//!   `AuthUser` and establishes a session.
//! - **Account connection** — a logged-in user attaches a provider
//!   (e.g. "Connect GitHub") to their existing account, which is how the
//!   app later gets API access (Drive, repos, …) on their behalf.
//!
//! Provider tokens are stored in [`umbra::orm::Masked`] columns, so a DB
//! dump never leaks a live token.
//!
//! ## Wiring
//!
//! ```rust,ignore
//! use umbra_oauth::OAuthPlugin;
//! use umbra_oauth::providers::{GoogleProvider, GitHubProvider};
//!
//! App::builder()
//!     .plugin(AuthPlugin::<AuthUser>::default())
//!     .plugin(
//!         OAuthPlugin::new("https://example.com")
//!             .provider(GoogleProvider::from_env())   // login + connect
//!             .provider(GitHubProvider::from_env())   // connect
//!             .login_redirect("/dashboard"),
//!     )
//! ```

pub mod models;
pub mod policy;
pub mod provider;
pub mod providers;
mod routes;

use std::sync::Arc;

use umbra::migrate::ModelMeta;
use umbra::plugin::Plugin;
use umbra::web::Router;

pub use models::SocialAccount;
pub use provider::{Identity, OAuthError, OAuthProvider, TokenSet};

/// The OAuth plugin. Holds the registered providers plus where to send
/// the browser back to. Build it with [`OAuthPlugin::new`] + the chained
/// setters, then register a provider per social login you support.
#[derive(Clone)]
pub struct OAuthPlugin {
    /// Public base URL of this app (scheme + host[:port]), used to build
    /// each provider's `redirect_uri` as `{base}/oauth/{key}/callback`.
    redirect_base: String,
    /// Where to send the browser after a successful login / connect.
    login_redirect: String,
    /// The registered providers, keyed by `provider.key()`.
    providers: Vec<Arc<dyn OAuthProvider>>,
}

impl OAuthPlugin {
    /// New plugin. `redirect_base` is this app's public origin
    /// (e.g. `"https://example.com"` or `"http://localhost:8000"`) — the
    /// per-provider callback URL is `{redirect_base}/oauth/{key}/callback`
    /// and must match what's registered in the provider's console.
    pub fn new(redirect_base: impl Into<String>) -> Self {
        Self {
            redirect_base: redirect_base.into(),
            login_redirect: "/".to_string(),
            providers: Vec::new(),
        }
    }

    /// Register a social provider (Google, GitHub, …).
    pub fn provider(mut self, provider: impl OAuthProvider + 'static) -> Self {
        self.providers.push(Arc::new(provider));
        self
    }

    /// Where to redirect after a successful login / connect. Defaults to
    /// `"/"`.
    pub fn login_redirect(mut self, path: impl Into<String>) -> Self {
        self.login_redirect = path.into();
        self
    }

    /// The callback URL for a provider key.
    pub(crate) fn redirect_uri(&self, provider_key: &str) -> String {
        format!(
            "{}/oauth/{}/callback",
            self.redirect_base.trim_end_matches('/'),
            provider_key
        )
    }

    /// Look up a registered provider by key.
    pub(crate) fn lookup(&self, key: &str) -> Option<&Arc<dyn OAuthProvider>> {
        self.providers.iter().find(|p| p.key() == key)
    }
}

impl Plugin for OAuthPlugin {
    fn name(&self) -> &'static str {
        "oauth"
    }

    /// Depends on `auth`: the `SocialAccount.user` FK targets
    /// `auth_user`, so the auth plugin's migration must run first.
    fn dependencies(&self) -> &'static [&'static str] {
        &["auth"]
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![ModelMeta::for_::<SocialAccount>()]
    }

    fn routes(&self) -> Router {
        routes::router(self.clone())
    }
}
