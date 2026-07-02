//! REST plugin extension — adapt `umbral-permissions` codenames into
//! `umbral-rest`'s viewset permission gates.
//!
//! Off by default. Pulled in by `umbral-permissions = { features =
//! ["rest"] }` so REST-free apps don't drag the `umbral-rest` dep
//! through the permissions crate.
//!
//! ## How it composes
//!
//! `umbral-rest` already speaks two contracts:
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
//! use umbral_permissions::rest::{HasPermission, WithPermissions};
//!
//! RestPlugin::default()
//!     .authenticate(WithPermissions::new(umbral_rest::SessionAuth))
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
use umbral::orm::Manager;
use umbral::orm::Predicate;
use umbral_auth::AuthUser;
use umbral_rest::auth::Authentication;
use umbral_rest::auth::Identity;
use umbral_rest::permission::{Action, Permission, PermissionError};

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

        // Pull `is_active` and `is_superuser` off the user row. A None
        // return here is "user vanished between authenticate and now,"
        // which shouldn't be fatal — fall through and treat as inactive
        // non-superuser (deny-by-default). The default `AuthUser` keys by
        // i64; custom user models can key by any string, so parse on the
        // way in and skip the lookup if the PK doesn't fit (the codename
        // grants below still work, since `user_perms` already speaks
        // strings).
        let (is_active, is_superuser) = match identity.user_id.parse::<i64>() {
            Ok(auth_user_id) => Manager::<AuthUser>::default()
                .filter(Predicate::<AuthUser>::col_eq("id", auth_user_id))
                .first()
                .await
                .ok()
                .flatten()
                .map(|u| (u.is_active, u.is_superuser && u.is_active))
                .unwrap_or((false, false)),
            Err(_) => (false, false),
        };
        // Store both flags so `HasPermission::check` can read them
        // without touching the database — it is intentionally sync.
        identity
            .extras
            .insert("is_active".to_string(), serde_json::Value::Bool(is_active));
        identity.extras.insert(
            "is_superuser".to_string(),
            serde_json::Value::Bool(is_superuser),
        );

        // Skip the perm-set DB read for superusers (they bypass every
        // codename check, so the list isn't load-bearing) and for
        // inactive users (their session is stale — deny-by-default, so
        // storing codenames would be misleading and wasteful).
        if is_active && !is_superuser {
            if let Ok(perms) = crate::user_perms(&identity.user_id).await {
                let arr: Vec<serde_json::Value> =
                    perms.into_iter().map(serde_json::Value::String).collect();
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

        // Inactive-user gate — must come before the superuser bypass so
        // that a deactivated superuser cannot slip through. If the
        // `is_active` key is absent (caller wired `HasPermission` without
        // `WithPermissions`), the key is simply missing and
        // `unwrap_or(true)` gives the benefit of the doubt — the
        // behaviour stays the same as before this fix for that wiring.
        // `WithPermissions` always populates it, so the safe default here
        // is `true` (don't add a surprise 403 for callers that populate
        // the extras themselves without this key).
        let is_active = id
            .extras
            .get("is_active")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !is_active {
            return Err(PermissionError::Forbidden);
        }

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
        make_identity(perms, is_super, true)
    }

    fn make_identity(perms: &[&str], is_super: bool, is_active: bool) -> Identity {
        let mut id = Identity::user(7);
        id.extras
            .insert("is_active".to_string(), serde_json::Value::Bool(is_active));
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

    // ---- gaps2 #75: inactive-user denial --------------------------------

    #[test]
    fn inactive_user_with_matching_codename_is_denied() {
        // Even though the codename is in the extras, an inactive user
        // must not be granted access.
        let perm = HasPermission::new("blog.publish_post");
        let id = make_identity(&["blog.publish_post"], false, false);
        assert!(
            matches!(
                perm.check(&Action::Create, Some(&id)),
                Err(PermissionError::Forbidden)
            ),
            "inactive user with matching codename must be denied"
        );
    }

    #[test]
    fn inactive_superuser_is_denied() {
        // An inactive superuser must NOT bypass permission checks.
        // `WithPermissions::authenticate` already stores `is_superuser =
        // false` for inactive superusers (it ANDs with `is_active`), but
        // the `is_active` gate here is the belt-and-suspenders defence
        // that catches the case where callers build the identity manually
        // and accidentally set both `is_active = false` and
        // `is_superuser = true`.
        let perm = HasPermission::new("blog.publish_post");
        let mut id = Identity::user(7);
        id.extras
            .insert("is_active".to_string(), serde_json::Value::Bool(false));
        id.extras
            .insert("is_superuser".to_string(), serde_json::Value::Bool(true));
        assert!(
            matches!(
                perm.check(&Action::Delete, Some(&id)),
                Err(PermissionError::Forbidden)
            ),
            "inactive superuser must be denied regardless of is_superuser flag"
        );
    }

    #[test]
    fn active_superuser_is_still_granted() {
        // Regression guard: the is_active gate must not break the normal
        // active-superuser path.
        let perm = HasPermission::new("blog.publish_post");
        let id = make_identity(&[], true, true);
        assert!(
            perm.check(&Action::Delete, Some(&id)).is_ok(),
            "active superuser must still bypass codename check"
        );
    }

    #[test]
    fn active_user_with_codename_is_granted() {
        // Regression guard: normal active-user codename grant must still work.
        let perm = HasPermission::new("blog.publish_post");
        let id = make_identity(&["blog.publish_post"], false, true);
        assert!(
            perm.check(&Action::Create, Some(&id)).is_ok(),
            "active user with matching codename must be granted"
        );
    }
}
