//! Authentication: who is the caller?
//!
//! The [`Authentication`] trait answers exactly that — examine the
//! request headers, return `Some(Identity)` if a known caller can be
//! identified, `None` if not. Permissions ([`crate::permission`])
//! then decide what that identity is allowed to do.
//!
//! ## Contract
//!
//! The types ([`Identity`], [`Authentication`]) are defined in
//! `umbral-core::auth_contract` and re-exported here so that
//! `umbral_rest::Identity` and `umbral_rest::Authentication` keep
//! resolving for existing consumers without import changes. This is the
//! gaps2 #76 fix: previously these types LIVED here, which forced
//! `umbral-auth` to depend on `umbral-rest`. Now both crates depend
//! inward on core.
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
//!
//! Session / Basic / Token / JWT specifics aren't baked into the
//! crate — they're 5-line `FnAuthentication` wrappers in your app
//! code, which avoids forcing a transitive dep on every auth scheme
//! onto users who only need one of them.
//!
//! ## Chained authentication
//!
//! `RestPlugin::authenticate` takes a single `Authentication`; if
//! you want a chain (try session first, fall back to Basic), build a
//! [`ChainAuthentication`] that walks each in order.

// Re-export the contract types from umbral-core so existing
// `umbral_rest::Identity` / `umbral_rest::Authentication` paths keep
// compiling without changes. The definitions moved to
// `umbral_core::auth_contract` as part of gaps2 #76.
pub use umbral::auth::{
    Authentication, ChainAuthentication, FnAuthentication, Identity, NoAuthentication,
    parse_basic_credentials,
};
