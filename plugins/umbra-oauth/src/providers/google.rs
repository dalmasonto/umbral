//! Google provider (OpenID Connect).
//!
//! Identity comes from the OIDC **userinfo** endpoint (a bearer GET),
//! not by parsing the `id_token` JWT — simpler and avoids shipping a JWT
//! verifier. `access_type=offline` + `prompt=consent` are set so Google
//! returns a refresh token, which is what later API access (Drive, …)
//! needs.

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::{Identity, OAuthError, OAuthProvider, TokenSet};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const DEFAULT_SCOPES: &str = "openid email profile";

/// The Google social provider. Construct with [`GoogleProvider::new`] or
/// [`GoogleProvider::from_env`]; widen the granted scopes with
/// [`GoogleProvider::scopes`] (e.g. to add Drive later).
#[derive(Clone)]
pub struct GoogleProvider {
    client_id: String,
    client_secret: String,
    scopes: String,
}

impl GoogleProvider {
    /// New provider from explicit credentials.
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            scopes: DEFAULT_SCOPES.to_string(),
        }
    }

    /// Build from `UMBRA_OAUTH_GOOGLE_CLIENT_ID` /
    /// `UMBRA_OAUTH_GOOGLE_CLIENT_SECRET`. Returns `None` if either is
    /// unset, so a consumer can register the provider conditionally.
    pub fn from_env() -> Option<Self> {
        let id = std::env::var("UMBRA_OAUTH_GOOGLE_CLIENT_ID").ok()?;
        let secret = std::env::var("UMBRA_OAUTH_GOOGLE_CLIENT_SECRET").ok()?;
        Some(Self::new(id, secret))
    }

    /// Override the requested scopes (default: `"openid email profile"`).
    pub fn scopes(mut self, scopes: impl Into<String>) -> Self {
        self.scopes = scopes.into();
        self
    }
}

#[derive(Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

/// Parse Google's token-endpoint JSON into a [`TokenSet`]. Pure — unit
/// tested against sample bodies.
fn parse_token(body: &str) -> Result<TokenSet, OAuthError> {
    // Don't interpolate the serde error: on a non-JSON body (e.g. an HTML
    // error page, or a body that echoes credentials) its message can carry
    // a fragment of the raw response into logs / surfaced errors.
    let r: GoogleTokenResponse = serde_json::from_str(body).map_err(|_| {
        OAuthError::Provider("google token response was not valid JSON".to_string())
    })?;
    Ok(TokenSet {
        access_token: r.access_token,
        refresh_token: r.refresh_token,
        expires_in: r.expires_in,
        scopes: r.scope.unwrap_or_default(),
    })
}

#[derive(Deserialize)]
struct GoogleUserinfo {
    sub: String,
    email: Option<String>,
    email_verified: Option<bool>,
    name: Option<String>,
}

/// Parse Google's userinfo JSON into an [`Identity`]. Pure.
fn parse_identity(body: &str) -> Result<Identity, OAuthError> {
    let u: GoogleUserinfo = serde_json::from_str(body)
        .map_err(|e| OAuthError::Provider(format!("google userinfo parse: {e}")))?;
    Ok(Identity {
        uid: u.sub,
        email: u.email,
        email_verified: u.email_verified.unwrap_or(false),
        display_name: u.name,
    })
}

#[async_trait]
impl OAuthProvider for GoogleProvider {
    fn key(&self) -> &'static str {
        "google"
    }

    fn label(&self) -> &'static str {
        "Google"
    }

    fn authorize_url(&self, state: &str, redirect_uri: &str, code_challenge: &str) -> String {
        let mut url = url::Url::parse(AUTH_URL).expect("AUTH_URL is a valid URL");
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &self.scopes)
            .append_pair("state", state)
            // PKCE (RFC 7636): bind this request to the token exchange.
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", "S256")
            // Ask for a refresh token (offline access) and force the
            // consent screen so Google re-issues one on re-link.
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
        url.to_string()
    }

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenSet, OAuthError> {
        let resp = reqwest::Client::new()
            .post(TOKEN_URL)
            .form(&[
                ("code", code),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("redirect_uri", redirect_uri),
                ("grant_type", "authorization_code"),
                // PKCE: prove we began this flow.
                ("code_verifier", code_verifier),
            ])
            .send()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;
        let body = resp
            .text()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;
        parse_token(&body)
    }

    async fn fetch_identity(&self, tokens: &TokenSet) -> Result<Identity, OAuthError> {
        let resp = reqwest::Client::new()
            .get(USERINFO_URL)
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;
        let body = resp
            .text()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;
        parse_identity(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_carries_state_scope_and_offline() {
        let p = GoogleProvider::new("client123", "secret");
        let url = p.authorize_url(
            "xyz-state",
            "https://app.example/oauth/google/callback",
            "chal-LENGE_123",
        );
        assert!(url.starts_with(AUTH_URL));
        assert!(url.contains("client_id=client123"));
        assert!(url.contains("state=xyz-state"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("response_type=code"));
        // PKCE challenge is carried with the S256 method.
        assert!(url.contains("code_challenge=chal-LENGE_123"));
        assert!(url.contains("code_challenge_method=S256"));
        // redirect_uri is percent-encoded.
        assert!(url.contains("redirect_uri=https%3A%2F%2Fapp.example%2Foauth%2Fgoogle%2Fcallback"));
    }

    #[test]
    fn parses_token_with_and_without_refresh() {
        let with_refresh = parse_token(
            r#"{"access_token":"at","refresh_token":"rt","expires_in":3599,"scope":"openid email"}"#,
        )
        .unwrap();
        assert_eq!(with_refresh.access_token, "at");
        assert_eq!(with_refresh.refresh_token.as_deref(), Some("rt"));
        assert_eq!(with_refresh.expires_in, Some(3599));

        let no_refresh = parse_token(r#"{"access_token":"at2"}"#).unwrap();
        assert_eq!(no_refresh.access_token, "at2");
        assert_eq!(no_refresh.refresh_token, None);
    }

    #[test]
    fn token_parse_error_does_not_leak_body() {
        // A non-JSON body that happens to contain a secret-looking string.
        let body = "<html>error: leaked_secret_abc123</html>";
        let err = parse_token(body).unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("leaked_secret_abc123"),
            "error must not echo the response body: {msg}"
        );
    }

    #[test]
    fn parses_identity_with_verified_email() {
        let id = parse_identity(
            r#"{"sub":"108200","email":"ada@example.com","email_verified":true,"name":"Ada"}"#,
        )
        .unwrap();
        assert_eq!(id.uid, "108200");
        assert_eq!(id.email.as_deref(), Some("ada@example.com"));
        assert!(id.email_verified);
        assert_eq!(id.display_name.as_deref(), Some("Ada"));
    }

    #[test]
    fn unverified_email_defaults_false_when_absent() {
        let id = parse_identity(r#"{"sub":"1","email":"x@y.z"}"#).unwrap();
        assert!(!id.email_verified);
    }
}
