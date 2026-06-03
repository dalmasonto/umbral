//! `SessionAuthentication` — the cookie-session adapter for
//! `umbra-rest`.
//!
//! Reads the session cookie from the request headers, calls
//! [`current_user`] to hydrate the `AuthUser` row, returns an
//! [`Identity`] for the permission classes to inspect.
//!
//! Wire it on a `RestPlugin`:
//!
//! ```ignore
//! use umbra_rest::RestPlugin;
//! use umbra_sessions::SessionAuthentication;
//!
//! RestPlugin::default()
//!     .authenticate(SessionAuthentication::default())
//! ```
//!
//! Or chain with bearer-token auth so browsers send a cookie and
//! curl sends a token:
//!
//! ```ignore
//! use umbra_rest::{ChainAuthentication, RestPlugin};
//! use std::sync::Arc;
//!
//! let auth = ChainAuthentication::new(vec![
//!     Arc::new(SessionAuthentication::default()),
//!     Arc::new(umbra_auth::BearerAuthentication::default()),
//! ]);
//! RestPlugin::default().authenticate(auth);
//! ```
//!
//! ## Why this lives in umbra-sessions
//!
//! `umbra-sessions` already owns the cookie layout, the `Session`
//! model, and the `current_user` lookup. Putting the auth-class
//! adapter here is the dep arrow already in play — `umbra-sessions`
//! depends on `umbra-auth` and `umbra-rest`, not the other way
//! around. Anonymous sessions resolve to `None` (no `user_id`); the
//! permission layer then chooses how to treat the anonymous caller.

use crate::current_user;
use async_trait::async_trait;
use umbra::web::HeaderMap;
use umbra_rest::{Authentication, Identity};

/// The session-cookie authenticator. Zero state — every request
/// reads the cookie, runs [`current_user`] (cookie → session row →
/// user row), returns an [`Identity`] or `None`.
///
/// The per-request DB cost is whatever `current_user` does today:
/// one session lookup by hashed token + one `auth_user` lookup by
/// id, both single-row indexed queries.
#[derive(Debug, Default, Clone, Copy)]
pub struct SessionAuthentication;

impl SessionAuthentication {
    /// Convenience constructor; identical to `Default::default()`.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Authentication for SessionAuthentication {
    async fn authenticate(&self, headers: &HeaderMap) -> Option<Identity> {
        let user = current_user(headers).await.ok().flatten()?;
        Some(
            Identity::user(user.id)
                .with_staff(user.is_staff)
                .with_extra("auth", serde_json::json!("session")),
        )
    }
}
