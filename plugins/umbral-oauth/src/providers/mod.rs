//! Built-in [`OAuthProvider`](crate::provider::OAuthProvider)
//! implementations. Each is a thin adapter over the provider's
//! authorize / token / userinfo endpoints; add a new social login by
//! implementing the trait the same way.

pub mod github;
pub mod google;

pub use github::GitHubProvider;
pub use google::GoogleProvider;
