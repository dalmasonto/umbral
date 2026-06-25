//! The authentication identity contract — who is the caller?
//!
//! [`Identity`] and [`Authentication`] are the two types every auth
//! backend and every permission class speaks. They live here in
//! `umbral-core` (re-exported from the `umbral` facade at `umbral::auth`)
//! so that `umbral-auth` and `umbral-rest` both depend *inward* on core
//! rather than one depending on the other.
//!
//! This is the architectural fix for gaps2 #76: previously
//! `umbral-auth` depended on `umbral-rest` to get `Identity` and
//! `Authentication`, which forced REST into every app that used auth —
//! even REST-free HTML apps. After this move, `umbral-auth` names
//! `umbral::auth::*` (the facade path), and `umbral-rest` re-exports the
//! same types from here rather than defining them itself.
//!
//! ## Built-ins
//!
//! - [`NoAuthentication`] — always returns `None`. The default; every
//!   request looks anonymous. Pair with `AllowAny` for fully open
//!   endpoints.
//! - [`FnAuthentication`] — wraps an async closure of your shape.
//!   The escape hatch for session-cookie auth (against
//!   `umbral_auth::current_user`), HTTP Basic Auth, API key,
//!   JWT, and anything else.
//! - [`ChainAuthentication`] — try multiple backends in order; first
//!   success wins.
//!
//! Session / Basic / Token / JWT specifics aren't baked into the
//! crate — they're 5-line `FnAuthentication` wrappers in your app
//! code, which avoids forcing a transitive dep on every auth scheme
//! onto users who only need one of them.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::web::{HeaderMap, header};

/// Who the request belongs to, after authentication.
///
/// The shape is intentionally narrow: `user_id`, `is_staff`, and
/// `is_superuser` cover most permission checks. An `extras` map carries
/// app-specific bits (role names, organisation id, scope strings) for
/// custom permission impls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    /// The authenticated user's primary key, stringified so the same
    /// `Identity` shape works whether the active user model has an
    /// `i64`, `String`, or UUID primary key. Permission checks that
    /// need the typed PK back can parse on demand
    /// (`identity.user_id.parse::<i64>()`); the framework's own
    /// permissions plugin and session store already speak strings.
    pub user_id: String,
    /// Staff flag, mirroring Django's `User.is_staff`. Used by the
    /// built-in `IsStaff` permission class in `umbral-rest`.
    pub is_staff: bool,
    /// Superuser flag, mirroring Django's `User.is_superuser`. A
    /// superuser bypasses all permission checks in the built-in
    /// permission classes; custom permission impls can consult this
    /// field to grant unconditional access.
    #[serde(default)]
    pub is_superuser: bool,
    /// App-specific extras a permission check might want to consult.
    /// `umbral-auth` doesn't populate this; user-defined auth backends
    /// can stuff role names, organisation ids, etc. here.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub extras: std::collections::HashMap<String, serde_json::Value>,
}

impl Identity {
    /// Convenience constructor for a non-staff user. Accepts any
    /// stringifiable PK — `Identity::user(42)`, `Identity::user("42")`,
    /// or `Identity::user(uuid.to_string())` all work because the
    /// argument is `impl ToString`.
    pub fn user(user_id: impl ToString) -> Self {
        Self {
            user_id: user_id.to_string(),
            is_staff: false,
            is_superuser: false,
            extras: Default::default(),
        }
    }

    /// Promote to staff. Chainable.
    pub fn staff(mut self) -> Self {
        self.is_staff = true;
        self
    }

    /// Set the staff flag explicitly. Chainable.
    pub fn with_staff(mut self, is_staff: bool) -> Self {
        self.is_staff = is_staff;
        self
    }

    /// Set the superuser flag explicitly. Chainable.
    pub fn with_superuser(mut self, is_superuser: bool) -> Self {
        self.is_superuser = is_superuser;
        self
    }

    /// Insert an extras entry. Chainable.
    pub fn with_extra(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.extras.insert(key.into(), value);
        self
    }
}

/// The authentication contract. Inspect headers, return an `Identity`
/// if recognised. Async because most real backends hit the DB.
///
/// Object-safe via `async-trait`'s `Pin<Box<...>>` desugaring; that's
/// what makes `Arc<dyn Authentication>` work in `RestPlugin`.
#[async_trait]
pub trait Authentication: Send + Sync + 'static {
    /// Try to identify the caller. `None` means "anonymous"; the
    /// permission check decides whether to allow that.
    ///
    /// Returning an error isn't part of the contract — auth backends
    /// should silently return `None` on invalid credentials and let
    /// the permission check produce a 403. The alternative
    /// (returning a typed error) leaks "which credential you tried"
    /// information to the client.
    async fn authenticate(&self, headers: &HeaderMap) -> Option<Identity>;

    /// OpenAPI `securitySchemes` entry this backend contributes —
    /// `Some((name, scheme_value))` for documented schemes, `None`
    /// to skip.
    ///
    /// `name` is the key under
    /// `components.securitySchemes.<name>`; consumers also reference
    /// it from operation-level `security: [{<name>: []}]` entries.
    /// `scheme_value` is the [OpenAPI 3.0 Security Scheme Object][1]
    /// serialised as a `serde_json::Value`.
    ///
    /// Default `None` — anonymous / no-auth backends contribute
    /// nothing. Concrete classes can override when they want to
    /// document their shape.
    ///
    /// [1]: https://spec.openapis.org/oas/v3.0.3#security-scheme-object
    fn security_scheme(&self) -> Option<(String, serde_json::Value)> {
        None
    }

    /// All `securitySchemes` entries the backend (and any children
    /// it might wrap) contributes. The default impl returns
    /// `self.security_scheme().into_iter().collect()` — fine for
    /// every leaf backend. `ChainAuthentication` overrides to walk
    /// every child so the OpenAPI plugin can publish the full list.
    fn security_schemes_all(&self) -> Vec<(String, serde_json::Value)> {
        self.security_scheme().into_iter().collect()
    }

    /// True when this backend never identifies anyone — every request is
    /// anonymous ([`NoAuthentication`]). Used only by the boot-time
    /// security warning (WEB-1); defaults to `false` so a real backend is
    /// never mistaken for the no-op.
    fn is_anonymous(&self) -> bool {
        false
    }
}

// =========================================================================
// Built-in: NoAuthentication — default. Always anonymous.
// =========================================================================

/// The do-nothing authenticator. Always returns `None`, so the
/// permission check sees anonymous. Default for `RestPlugin`
/// — opt in to real auth via `RestPlugin::authenticate`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAuthentication;

#[async_trait]
impl Authentication for NoAuthentication {
    async fn authenticate(&self, _headers: &HeaderMap) -> Option<Identity> {
        None
    }

    fn is_anonymous(&self) -> bool {
        true
    }
}

// =========================================================================
// Built-in: FnAuthentication — wrap any closure.
// =========================================================================

/// `Authentication` from a user-supplied async closure. Keeps the
/// shape pluggable without dragging session / basic / JWT crates into
/// `umbral-rest` itself.
///
/// ```ignore
/// // Session-cookie auth via umbral-sessions:
/// RestPlugin::default().authenticate(FnAuthentication::new(|headers| async move {
///     let user = umbral_auth::current_user(&headers).await.ok().flatten()?;
///     Some(Identity::user(user.id).with_staff(user.is_staff))
/// }));
///
/// // HTTP Basic Auth against umbral-auth:
/// RestPlugin::default().authenticate(FnAuthentication::new(|headers| async move {
///     let (user, pass) = umbral::auth::parse_basic_credentials(&headers)?;
///     let auth_user = umbral_auth::authenticate(&user, &pass).await.ok()?;
///     Some(Identity::user(auth_user.id).with_staff(auth_user.is_staff))
/// }));
/// ```
///
/// The closure takes an owned `HeaderMap` (cheap, internal Bytes
/// references). That lets the future capture the headers without
/// fighting lifetimes.
#[derive(Clone)]
pub struct FnAuthentication {
    f: Arc<
        dyn Fn(HeaderMap) -> Pin<Box<dyn std::future::Future<Output = Option<Identity>> + Send>>
            + Send
            + Sync,
    >,
}

impl std::fmt::Debug for FnAuthentication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FnAuthentication").finish_non_exhaustive()
    }
}

impl FnAuthentication {
    /// Wrap an async closure as an `Authentication`. The closure
    /// receives a cloned `HeaderMap` and returns `Option<Identity>`.
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(HeaderMap) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Option<Identity>> + Send + 'static,
    {
        Self {
            f: Arc::new(move |headers| Box::pin(f(headers))),
        }
    }
}

#[async_trait]
impl Authentication for FnAuthentication {
    async fn authenticate(&self, headers: &HeaderMap) -> Option<Identity> {
        (self.f)(headers.clone()).await
    }
}

// =========================================================================
// Built-in: ChainAuthentication — first-success wins.
// =========================================================================

/// Try multiple authentications in order. The first one that returns
/// `Some(Identity)` wins; if none succeed, the request is anonymous.
///
/// Common case: session-cookie for browsers, HTTP Basic Auth for
/// curl-style API consumers. Build via [`Self::new`]:
///
/// ```ignore
/// let auth = ChainAuthentication::new(vec![
///     Arc::new(session_auth) as Arc<dyn Authentication>,
///     Arc::new(basic_auth)   as Arc<dyn Authentication>,
/// ]);
/// RestPlugin::default().authenticate(auth);
/// ```
#[derive(Clone)]
pub struct ChainAuthentication {
    backends: Vec<Arc<dyn Authentication>>,
}

impl std::fmt::Debug for ChainAuthentication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChainAuthentication")
            .field("backends_count", &self.backends.len())
            .finish()
    }
}

impl ChainAuthentication {
    /// Build a chain. Order matters — first to succeed wins.
    pub fn new(backends: Vec<Arc<dyn Authentication>>) -> Self {
        Self { backends }
    }
}

#[async_trait]
impl Authentication for ChainAuthentication {
    async fn authenticate(&self, headers: &HeaderMap) -> Option<Identity> {
        for backend in &self.backends {
            if let Some(id) = backend.authenticate(headers).await {
                return Some(id);
            }
        }
        None
    }

    fn security_scheme(&self) -> Option<(String, serde_json::Value)> {
        // Returns the first child's contribution for callers that
        // only want one. The full walk lives on
        // `security_schemes_all` below — the OpenAPI plugin uses
        // that path so the spec publishes every scheme the chain
        // accepts.
        self.backends.iter().find_map(|b| b.security_scheme())
    }

    fn security_schemes_all(&self) -> Vec<(String, serde_json::Value)> {
        self.backends
            .iter()
            .flat_map(|b| b.security_schemes_all())
            .collect()
    }
}

// =========================================================================
// Helper: HTTP Basic Auth credential extraction.
// =========================================================================

/// Parse a `Basic <base64(user:pass)>` Authorization header into
/// `(username, password)`. Returns `None` if the header is missing,
/// malformed, or not Basic.
///
/// Provided as a free function so user-supplied `FnAuthentication`
/// closures (the recommended way to ship HTTP Basic Auth) can reach
/// it without re-implementing the boring base64 + colon-split logic.
pub fn parse_basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let header = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = header.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::header::AUTHORIZATION;

    fn headers_with(name: &str, value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            crate::web::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        h
    }

    #[tokio::test]
    async fn no_authentication_always_returns_none() {
        let headers = HeaderMap::new();
        assert!(NoAuthentication.authenticate(&headers).await.is_none());
    }

    #[tokio::test]
    async fn fn_authentication_invokes_closure() {
        let auth = FnAuthentication::new(|_headers| async move { Some(Identity::user(42)) });
        let id = auth.authenticate(&HeaderMap::new()).await.unwrap();
        assert_eq!(id.user_id, "42");
        assert!(!id.is_staff);
    }

    #[tokio::test]
    async fn chain_authentication_first_success_wins() {
        let first = FnAuthentication::new(|_| async move { None });
        let second = FnAuthentication::new(|_| async move { Some(Identity::user(7).staff()) });
        let third = FnAuthentication::new(|_| async move { Some(Identity::user(99)) });
        let chain = ChainAuthentication::new(vec![
            Arc::new(first) as Arc<dyn Authentication>,
            Arc::new(second) as Arc<dyn Authentication>,
            Arc::new(third) as Arc<dyn Authentication>,
        ]);
        let id = chain.authenticate(&HeaderMap::new()).await.unwrap();
        // Second wins, third never runs.
        assert_eq!(id.user_id, "7");
        assert!(id.is_staff);
    }

    #[tokio::test]
    async fn chain_authentication_returns_none_when_all_fail() {
        let chain = ChainAuthentication::new(vec![
            Arc::new(NoAuthentication) as Arc<dyn Authentication>,
            Arc::new(NoAuthentication) as Arc<dyn Authentication>,
        ]);
        assert!(chain.authenticate(&HeaderMap::new()).await.is_none());
    }

    #[test]
    fn parse_basic_credentials_extracts_user_and_pass() {
        // "alice:secret" base64-encoded
        let headers = headers_with(AUTHORIZATION.as_str(), "Basic YWxpY2U6c2VjcmV0");
        let (user, pass) = parse_basic_credentials(&headers).unwrap();
        assert_eq!(user, "alice");
        assert_eq!(pass, "secret");
    }

    #[test]
    fn parse_basic_credentials_returns_none_for_missing_header() {
        assert!(parse_basic_credentials(&HeaderMap::new()).is_none());
    }

    #[test]
    fn parse_basic_credentials_returns_none_for_wrong_scheme() {
        let headers = headers_with(AUTHORIZATION.as_str(), "Bearer abc");
        assert!(parse_basic_credentials(&headers).is_none());
    }

    #[test]
    fn parse_basic_credentials_returns_none_for_invalid_base64() {
        let headers = headers_with(AUTHORIZATION.as_str(), "Basic !!!notbase64");
        assert!(parse_basic_credentials(&headers).is_none());
    }
}
