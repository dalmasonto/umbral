//! Service-discovery surface: the `GET /oauth/providers` endpoint and
//! the `Plugin::api_endpoints()` advertisement. Both are auto-built from
//! the registered providers, so a SPA can fetch the provider links and a
//! REST API root can list them without hardcoding paths.
//!
//! These exercise the real public surface: the discovery test drives the
//! actual mounted route (`Plugin::routes()` → oneshot), and the
//! endpoint test reads what the plugin advertises to the framework.

use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use umbral::plugin::Plugin;
use umbral_oauth::{Identity, OAuthError, OAuthPlugin, OAuthProvider, TokenSet};

/// A provider stub. Only `key`/`label` matter for discovery; the flow
/// methods are never reached by these tests.
struct FakeProvider {
    key: &'static str,
    label: &'static str,
}

#[async_trait]
impl OAuthProvider for FakeProvider {
    fn key(&self) -> &'static str {
        self.key
    }
    fn label(&self) -> &'static str {
        self.label
    }
    fn authorize_url(&self, _state: &str, _redirect_uri: &str, _code_challenge: &str) -> String {
        unreachable!("discovery tests never start a flow")
    }
    async fn exchange_code(
        &self,
        _code: &str,
        _redirect_uri: &str,
        _code_verifier: &str,
    ) -> Result<TokenSet, OAuthError> {
        unreachable!("discovery tests never exchange a code")
    }
    async fn fetch_identity(&self, _tokens: &TokenSet) -> Result<Identity, OAuthError> {
        unreachable!("discovery tests never fetch identity")
    }
}

fn plugin() -> OAuthPlugin {
    OAuthPlugin::new("https://api.example.com")
        .provider(FakeProvider {
            key: "google",
            label: "Google",
        })
        .provider(FakeProvider {
            key: "github",
            label: "GitHub",
        })
}

/// `GET /oauth/providers` returns each configured provider with relative
/// `path` and absolute `url` (joined onto `redirect_base`) for login,
/// connect, and callback — auto-generated, no hardcoded list.
#[tokio::test]
async fn providers_endpoint_lists_configured_providers() {
    let router = plugin().routes();
    let req = Request::builder()
        .uri("/oauth/providers")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();

    let providers = body["providers"].as_array().expect("providers array");
    assert_eq!(providers.len(), 2, "both providers listed");

    let google = providers
        .iter()
        .find(|p| p["key"] == "google")
        .expect("google present");
    assert_eq!(google["label"], "Google");
    assert_eq!(google["login"]["path"], "/oauth/google/login");
    assert_eq!(
        google["login"]["url"],
        "https://api.example.com/oauth/google/login"
    );
    assert_eq!(google["connect"]["path"], "/oauth/google/connect");
    assert_eq!(
        google["callback"]["url"],
        "https://api.example.com/oauth/google/callback"
    );
}

/// `api_endpoints()` advertises a `login` and a `connect` row per
/// provider (no `callback` — it's the provider's redirect target, not a
/// client action), grouped under `"oauth"` with relative paths.
#[tokio::test]
async fn api_endpoints_advertises_login_and_connect() {
    let endpoints = plugin().api_endpoints();
    // 2 providers × {login, connect}.
    assert_eq!(endpoints.len(), 4);

    let google_login = endpoints
        .iter()
        .find(|e| e.name == "google.login")
        .expect("google.login advertised");
    assert_eq!(google_login.group, "oauth");
    assert_eq!(google_login.method, "GET");
    assert_eq!(google_login.path, "/oauth/google/login");
    assert_eq!(google_login.label, "Sign in with Google");

    let github_connect = endpoints
        .iter()
        .find(|e| e.name == "github.connect")
        .expect("github.connect advertised");
    assert_eq!(github_connect.path, "/oauth/github/connect");
    assert_eq!(github_connect.label, "Connect GitHub");

    // No callback rows in the advertised set.
    assert!(
        !endpoints.iter().any(|e| e.name.ends_with(".callback")),
        "callback must not be advertised as a client endpoint"
    );
}
