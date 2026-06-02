//! REST plugin extension — adapt `umbra-permissions` codenames into
//! `umbra-rest`'s viewset permission gates.
//!
//! Off by default. Pulled in by `umbra-permissions = { features =
//! ["rest"] }` so REST-free apps don't drag the `umbra-rest` dep
//! through the permissions crate.
//!
//! ## How it composes
//!
//! `umbra-rest` already speaks two contracts:
//!
//! - [`Authentication`] — async, resolves the request into an
//!   [`Identity`].
//! - [`Permission`] — sync, decides whether an action is allowed for
//!   a given identity. Sync because the upstream design pre-resolves
//!   everything the check needs into the identity at auth time.
//!
//! This module bridges them:
//!
//! - [`WithPermissions<A>`] decorates an inner `Authentication` to
//!   stuff the user's permission codenames (and `is_superuser` flag)
//!   into [`Identity::extras`] under the keys `permissions` and
//!   `is_superuser`. One DB read per authenticated request, on top
//!   of whatever the inner authenticator already does.
//! - [`HasPermission`] is the sync `Permission` impl that reads
//!   those extras and decides allow / deny.
//!
//! Pair them in the REST plugin builder:
//!
//! ```ignore
//! use umbra_permissions::rest::{HasPermission, WithPermissions};
//!
//! RestPlugin::default()
//!     .authenticate(WithPermissions::new(umbra_rest::SessionAuth))
//!     .resource(
//!         ResourceConfig::new("post")
//!             .permission(HasPermission::new("blog.publish_post")),
//!     )
//! ```
//!
//! Superuser bypass is automatic — a user with `is_superuser = true`
//! passes every `HasPermission` check, mirroring
//! [`crate::has_perm_for_superuser`].
//!
//! ## Why a decorator, not a one-shot Authentication
//!
//! The decorator shape (`WithPermissions::new(inner)`) keeps the
//! inner authenticator pluggable: session cookies, basic auth, JWT,
//! anything that implements `Authentication` works underneath. The
//! permissions layer is purely additive.

use std::sync::Arc;

use async_trait::async_trait;
use umbra::orm::Manager;
use umbra::orm::Predicate;
use umbra_auth::AuthUser;
use umbra_rest::auth::Authentication;
use umbra_rest::auth::Identity;
use umbra_rest::permission::{Action, Permission, PermissionError};

/// REST `Authentication` adapter that decorates `A` with the
/// caller's permission codenames + superuser flag in
/// `Identity::extras`. See the module docs for the composition
/// pattern.
///
/// The inner authenticator runs first; if it returns `None` (anon),
/// no DB lookup happens. If it returns `Some(identity)`, this layer
/// loads `is_superuser` from `auth_user` and the full permission
/// set via [`crate::user_perms`], then merges both into `extras`.
pub struct WithPermissions<A: Authentication> {
    inner: Arc<A>,
}

impl<A: Authentication> WithPermissions<A> {
    /// Wrap an inner authenticator. The result is itself an
    /// `Authentication`, ready to hand to `RestPlugin::authenticate`.
    pub fn new(inner: A) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

#[async_trait]
impl<A: Authentication> Authentication for WithPermissions<A> {
    async fn authenticate(&self, headers: &http::HeaderMap) -> Option<Identity> {
        let mut identity = self.inner.authenticate(headers).await?;

        // Pull `is_superuser` off the user row. A None return here
        // is "user vanished between authenticate and now," which
        // shouldn't be fatal — fall through and treat as non-super.
        let is_superuser = Manager::<AuthUser>::default()
            .filter(Predicate::<AuthUser>::col_eq("id", identity.user_id))
            .first()
            .await
            .ok()
            .flatten()
            .map(|u| u.is_superuser)
            .unwrap_or(false);
        identity
            .extras
            .insert("is_superuser".to_string(), serde_json::Value::Bool(is_superuser));

        // Skip the perm-set DB read entirely for superusers — they
        // bypass every codename check, so the codename list isn't
        // load-bearing for them.
        if !is_superuser {
            if let Ok(perms) = crate::user_perms(identity.user_id).await {
                let arr: Vec<serde_json::Value> = perms
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect();
                identity
                    .extras
                    .insert("permissions".to_string(), serde_json::Value::Array(arr));
            }
        }

        Some(identity)
    }
}

/// REST `Permission` impl that checks one `app_label.codename`
/// against the identity's pre-loaded permission set.
///
/// Pair with [`WithPermissions`] on the `Authentication` side so the
/// extras map is populated; this `check` itself is sync and never
/// hits the database.
#[derive(Debug, Clone)]
pub struct HasPermission {
    codename: String,
}

impl HasPermission {
    /// Build a permission gate keyed on `"<app_label>.<codename>"` —
    /// for example `"blog.publish_post"`. The codename uses the
    /// same shape that [`crate::has_perm`] expects, so a single
    /// string works both as a REST resource gate and as a direct
    /// `has_perm` call in handler code.
    pub fn new(codename: impl Into<String>) -> Self {
        Self {
            codename: codename.into(),
        }
    }
}

impl Permission for HasPermission {
    fn check(&self, _action: &Action, identity: Option<&Identity>) -> Result<(), PermissionError> {
        let Some(id) = identity else {
            return Err(PermissionError::Unauthenticated);
        };

        // Superuser bypass — mirrors `has_perm_for_superuser`. The
        // flag was set by `WithPermissions::authenticate`; if the
        // user wired `HasPermission` against a different
        // authenticator that doesn't populate `extras`, fall
        // through to the codename check (which will also miss and
        // produce a 403 — defensive default).
        let is_superuser = id
            .extras
            .get("is_superuser")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_superuser {
            return Ok(());
        }

        let allowed = id
            .extras
            .get("permissions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .any(|v| v.as_str().is_some_and(|s| s == self.codename))
            })
            .unwrap_or(false);

        if allowed {
            Ok(())
        } else {
            Err(PermissionError::Forbidden)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_identity_with_perms(perms: &[&str], is_super: bool) -> Identity {
        let mut id = Identity::user(7);
        id.extras.insert(
            "is_superuser".to_string(),
            serde_json::Value::Bool(is_super),
        );
        id.extras.insert(
            "permissions".to_string(),
            serde_json::Value::Array(
                perms
                    .iter()
                    .map(|s| serde_json::Value::String(s.to_string()))
                    .collect(),
            ),
        );
        id
    }

    #[test]
    fn has_permission_allows_when_codename_in_extras() {
        let perm = HasPermission::new("blog.publish_post");
        let id = make_identity_with_perms(&["blog.publish_post", "blog.view_post"], false);
        assert!(perm.check(&Action::Create, Some(&id)).is_ok());
    }

    #[test]
    fn has_permission_denies_when_codename_missing() {
        let perm = HasPermission::new("blog.publish_post");
        let id = make_identity_with_perms(&["blog.view_post"], false);
        assert!(matches!(
            perm.check(&Action::Create, Some(&id)),
            Err(PermissionError::Forbidden)
        ));
    }

    #[test]
    fn has_permission_unauthenticated_for_anon() {
        let perm = HasPermission::new("blog.publish_post");
        assert!(matches!(
            perm.check(&Action::Create, None),
            Err(PermissionError::Unauthenticated)
        ));
    }

    #[test]
    fn superuser_bypasses_codename_check() {
        // No matching codename, but is_superuser = true → allowed.
        let perm = HasPermission::new("blog.publish_post");
        let id = make_identity_with_perms(&[], true);
        assert!(perm.check(&Action::Delete, Some(&id)).is_ok());
    }

    #[test]
    fn missing_extras_treated_as_unauthorised() {
        // Identity has no `permissions` / `is_superuser` keys at all
        // (e.g. the caller wired `HasPermission` without
        // `WithPermissions` to populate them). The check should
        // 403, not panic.
        let perm = HasPermission::new("blog.publish_post");
        let id = Identity::user(7);
        assert!(matches!(
            perm.check(&Action::Create, Some(&id)),
            Err(PermissionError::Forbidden)
        ));
    }
}
