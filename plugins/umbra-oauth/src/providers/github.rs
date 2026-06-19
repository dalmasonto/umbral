//! GitHub provider.
//!
//! Two GitHub quirks the flow handles: the token endpoint returns
//! form-encoded data unless you send `Accept: application/json`, and the
//! API requires a `User-Agent` header. A user's email may be private on
//! `/user`, so the verified primary email is read from `/user/emails`.
//! GitHub OAuth apps don't issue refresh tokens, so `refresh_token` is
//! always `None`.

use async_trait::async_trait;
use serde::Deserialize;

use crate::provider::{Identity, OAuthError, OAuthProvider, TokenSet};

const AUTH_URL: &str = "https://github.com/login/oauth/authorize";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const USER_URL: &str = "https://api.github.com/user";
const EMAILS_URL: &str = "https://api.github.com/user/emails";
const USER_AGENT: &str = "umbra-oauth";
const DEFAULT_SCOPES: &str = "read:user user:email";

/// The GitHub social provider. Construct with [`GitHubProvider::new`] or
/// [`GitHubProvider::from_env`].
#[derive(Clone)]
pub struct GitHubProvider {
    client_id: String,
    client_secret: String,
    scopes: String,
}

impl GitHubProvider {
    /// New provider from explicit credentials.
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            scopes: DEFAULT_SCOPES.to_string(),
        }
    }

    /// Build from `UMBRA_OAUTH_GITHUB_CLIENT_ID` /
    /// `UMBRA_OAUTH_GITHUB_CLIENT_SECRET`. `None` if either is unset.
    pub fn from_env() -> Option<Self> {
        let id = std::env::var("UMBRA_OAUTH_GITHUB_CLIENT_ID").ok()?;
        let secret = std::env::var("UMBRA_OAUTH_GITHUB_CLIENT_SECRET").ok()?;
        Some(Self::new(id, secret))
    }

    /// Override the requested scopes (default: `"read:user user:email"`).
    pub fn scopes(mut self, scopes: impl Into<String>) -> Self {
        self.scopes = scopes.into();
        self
    }
}

#[derive(Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
    scope: Option<String>,
}

/// Parse GitHub's token JSON (returned when `Accept: application/json`).
fn parse_token(body: &str) -> Result<TokenSet, OAuthError> {
    // Don't interpolate the serde error: on a non-JSON body (e.g. an HTML
    // error page, or a body that echoes credentials) its message can carry
    // a fragment of the raw response into logs / surfaced errors.
    let r: GitHubTokenResponse = serde_json::from_str(body).map_err(|_| {
        OAuthError::Provider("github token response was not valid JSON".to_string())
    })?;
    Ok(TokenSet {
        access_token: r.access_token,
        // OAuth apps don't issue refresh tokens.
        refresh_token: None,
        expires_in: None,
        scopes: r.scope.unwrap_or_default(),
    })
}

#[derive(Deserialize)]
struct GitHubUser {
    id: i64,
    login: String,
    name: Option<String>,
    email: Option<String>,
}

#[derive(Deserialize)]
struct GitHubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

/// Parse `/user` into `(uid, login, name, public_email)`.
fn parse_user(body: &str) -> Result<(String, String, Option<String>, Option<String>), OAuthError> {
    let u: GitHubUser = serde_json::from_str(body)
        .map_err(|e| OAuthError::Provider(format!("github user parse: {e}")))?;
    Ok((u.id.to_string(), u.login, u.name, u.email))
}

/// Pick the primary verified email from `/user/emails`, else any
/// verified one. Returns `(email, verified)` — `None` if no verified
/// address exists.
fn pick_verified_email(body: &str) -> Option<(String, bool)> {
    let emails: Vec<GitHubEmail> = serde_json::from_str(body).ok()?;
    emails
        .iter()
        .find(|e| e.primary && e.verified)
        .or_else(|| emails.iter().find(|e| e.verified))
        .map(|e| (e.email.clone(), true))
}

#[async_trait]
impl OAuthProvider for GitHubProvider {
    fn key(&self) -> &'static str {
        "github"
    }

    fn label(&self) -> &'static str {
        "GitHub"
    }

    fn authorize_url(&self, state: &str, redirect_uri: &str, code_challenge: &str) -> String {
        let mut url = url::Url::parse(AUTH_URL).expect("AUTH_URL is a valid URL");
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &self.scopes)
            .append_pair("state", state)
            // PKCE (RFC 7636): bind this request to the token exchange.
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", "S256");
        url.to_string()
    }

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenSet, OAuthError> {
        let resp = crate::http::http_client()
            .post(TOKEN_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("code", code),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("redirect_uri", redirect_uri),
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
        let client = crate::http::http_client();
        let user_body = client
            .get(USER_URL)
            .bearer_auth(&tokens.access_token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .send()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?
            .text()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;
        let (uid, login, name, public_email) = parse_user(&user_body)?;

        // Email may be private on /user — resolve the verified primary
        // from /user/emails. A failure there isn't fatal: fall back to
        // the public email (unverified).
        let (email, email_verified) = match client
            .get(EMAILS_URL)
            .bearer_auth(&tokens.access_token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => {
                let body = resp.text().await.unwrap_or_default();
                match pick_verified_email(&body) {
                    Some((e, v)) => (Some(e), v),
                    None => (public_email, false),
                }
            }
            Err(_) => (public_email, false),
        };

        Ok(Identity {
            uid,
            email,
            email_verified,
            display_name: name.or(Some(login)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_carries_state_and_scope() {
        let p = GitHubProvider::new("ghid", "ghsecret");
        let url = p.authorize_url(
            "st8",
            "https://app.example/oauth/github/callback",
            "chal-LENGE_123",
        );
        assert!(url.starts_with(AUTH_URL));
        assert!(url.contains("client_id=ghid"));
        assert!(url.contains("state=st8"));
        assert!(url.contains("scope=read%3Auser+user%3Aemail"));
        // PKCE challenge is carried with the S256 method.
        assert!(url.contains("code_challenge=chal-LENGE_123"));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn parses_token_without_refresh() {
        let t = parse_token(
            r#"{"access_token":"gho_abc","scope":"read:user,user:email","token_type":"bearer"}"#,
        )
        .unwrap();
        assert_eq!(t.access_token, "gho_abc");
        assert_eq!(t.refresh_token, None);
        assert_eq!(t.scopes, "read:user,user:email");
    }

    #[test]
    fn parses_user_numeric_id_to_string() {
        let (uid, login, name, email) =
            parse_user(r#"{"id":583231,"login":"octocat","name":"The Octocat","email":null}"#)
                .unwrap();
        assert_eq!(uid, "583231");
        assert_eq!(login, "octocat");
        assert_eq!(name.as_deref(), Some("The Octocat"));
        assert_eq!(email, None);
    }

    #[test]
    fn picks_primary_verified_email() {
        let body = r#"[
            {"email":"alt@example.com","primary":false,"verified":true},
            {"email":"main@example.com","primary":true,"verified":true},
            {"email":"old@example.com","primary":false,"verified":false}
        ]"#;
        assert_eq!(
            pick_verified_email(body),
            Some(("main@example.com".to_string(), true))
        );
    }

    #[test]
    fn no_verified_email_returns_none() {
        let body = r#"[{"email":"x@y.z","primary":true,"verified":false}]"#;
        assert_eq!(pick_verified_email(body), None);
    }
}
