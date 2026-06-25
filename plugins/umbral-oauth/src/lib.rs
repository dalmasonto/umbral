//! `umbral-oauth`: OAuth / social authentication for umbral.
//! Social login and account connection.
//!
//! Layered on `umbral-auth`: it adds a `SocialAccount` table that links
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
//! Provider tokens are stored in [`umbral::orm::Masked`] columns, so a DB
//! dump never leaks a live token.
//!
//! ## Wiring
//!
//! ```rust,ignore
//! use umbral_oauth::OAuthPlugin;
//! use umbral_oauth::providers::{GoogleProvider, GitHubProvider};
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

mod http;
pub mod models;
pub mod pkce;
pub mod policy;
pub mod provider;
pub mod providers;
mod routes;

use std::sync::{Arc, OnceLock};

use umbral::migrate::ModelMeta;
use umbral::plugin::{ApiEndpoint, AppContext, Plugin, PluginError};
use umbral::web::Router;

pub use models::SocialAccount;
pub use provider::{Identity, OAuthError, OAuthProvider, TokenSet};

/// The provider keys that were registered on the `OAuthPlugin`, published
/// at boot so the UI can show a button only for a *configured* provider
/// (a `from_env()` provider with no credentials is never registered, so
/// it never appears here). Read it with [`available_providers`].
static REGISTERED_PROVIDERS: OnceLock<Vec<&'static str>> = OnceLock::new();

/// The provider keys available for login/connect (e.g. `["google"]`).
/// Empty until the `OAuthPlugin` has booted. Use this to render only the
/// social buttons that will actually work.
pub fn available_providers() -> Vec<&'static str> {
    REGISTERED_PROVIDERS.get().cloned().unwrap_or_default()
}

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
    /// Allowlisted prefixes for the SPA `?next=` return URL. A login
    /// started with `?next=<url>` is honored only when `<url>` begins
    /// with one of these; the callback then returns a bearer token in
    /// the URL fragment instead of establishing a cookie session. Empty
    /// (the default) disables token mode entirely — `?next=` is ignored
    /// and the flow keeps its server-rendered session behavior.
    allowed_returns: Vec<String>,
}

/// The relative paths for one provider's flow endpoints. Single source
/// of truth shared by the `/oauth/providers` discovery endpoint and
/// [`OAuthPlugin::api_endpoints`] so the two can never drift.
pub(crate) struct ProviderLinks {
    pub key: &'static str,
    pub label: &'static str,
    pub login: String,
    pub connect: String,
    pub callback: String,
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
            allowed_returns: Vec::new(),
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

    /// Allow a SPA return URL (prefix) for token-mode login. Call once
    /// per trusted callback URL your single-page app serves. A login
    /// started as `GET /oauth/{provider}/login?next=<url>` is honored
    /// only when `<url>` starts with one of these prefixes; the OAuth
    /// callback then redirects to `<url>#token=<bearer>&token_type=Bearer`
    /// instead of setting a session cookie, so a separate-origin SPA can
    /// pick the token out of the fragment and call the REST API with
    /// `Authorization: Bearer`.
    ///
    /// This allowlist is the open-redirect / token-theft defense: a
    /// `next` that matches nothing is rejected with `400`, so an attacker
    /// can't point the flow at `https://evil.example` and harvest a
    /// freshly minted token. With no allowlist set, `?next=` is ignored.
    pub fn allow_return(mut self, url_prefix: impl Into<String>) -> Self {
        self.allowed_returns.push(url_prefix.into());
        self
    }

    /// Whether `next` is a permitted SPA return URL (prefix match
    /// against the [`allow_return`](Self::allow_return) allowlist).
    pub(crate) fn is_allowed_return(&self, next: &str) -> bool {
        self.allowed_returns
            .iter()
            .any(|p| allowed_return_matches(p, next))
    }

    /// The flow-endpoint paths for every registered provider. The one
    /// source of truth behind both discovery surfaces.
    pub(crate) fn provider_links(&self) -> Vec<ProviderLinks> {
        self.providers
            .iter()
            .map(|p| {
                let key = p.key();
                ProviderLinks {
                    key,
                    label: p.label(),
                    login: format!("/oauth/{key}/login"),
                    connect: format!("/oauth/{key}/connect"),
                    callback: format!("/oauth/{key}/callback"),
                }
            })
            .collect()
    }

    /// Absolute form of a relative flow path, joined onto `redirect_base`
    /// (the app's authoritative public origin). Used by the discovery
    /// endpoint's `url` fields.
    pub(crate) fn absolute(&self, path: &str) -> String {
        format!("{}{}", self.redirect_base.trim_end_matches('/'), path)
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

fn allowed_return_matches(allowed: &str, next: &str) -> bool {
    let (Ok(allowed), Ok(next)) = (url::Url::parse(allowed), url::Url::parse(next)) else {
        return false;
    };
    if allowed.scheme() != next.scheme()
        || allowed.host_str() != next.host_str()
        || allowed.port_or_known_default() != next.port_or_known_default()
    {
        return false;
    }
    let allowed_path = allowed.path().trim_end_matches('/');
    next.path() == allowed_path || next.path().starts_with(&format!("{allowed_path}/"))
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

    /// Advertise each provider's `login` and `connect` entry points for
    /// service discovery (a REST API root, etc.). The `callback` is
    /// omitted here — it's the provider's redirect target, not a
    /// client-callable action — but `GET /oauth/providers` still lists
    /// it for setup convenience.
    fn api_endpoints(&self) -> Vec<ApiEndpoint> {
        let mut out = Vec::new();
        for link in self.provider_links() {
            out.push(ApiEndpoint {
                group: "oauth".to_string(),
                name: format!("{}.login", link.key),
                method: "GET".to_string(),
                path: link.login,
                label: format!("Sign in with {}", link.label),
            });
            out.push(ApiEndpoint {
                group: "oauth".to_string(),
                name: format!("{}.connect", link.key),
                method: "GET".to_string(),
                path: link.connect,
                label: format!("Connect {}", link.label),
            });
        }
        out
    }

    /// Publish the registered provider keys so the UI can show a button
    /// only for a configured provider (see [`available_providers`]).
    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        let keys: Vec<&'static str> = self.providers.iter().map(|p| p.key()).collect();
        if keys.is_empty() {
            tracing::warn!(
                "oauth: no providers registered — check that UMBRAL_OAUTH_<PROVIDER>_CLIENT_ID \
                 and _CLIENT_SECRET are set (in the environment or a .env in the launch \
                 directory). The social sign-in buttons stay hidden until at least one is set."
            );
        } else {
            tracing::info!("oauth: registered providers: {keys:?}");
        }
        let _ = REGISTERED_PROVIDERS.set(keys);
        Ok(())
    }
}
