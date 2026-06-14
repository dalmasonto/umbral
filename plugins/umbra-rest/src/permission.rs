//! Permissions: what is the caller allowed to do?
//!
//! Authentication ([`crate::auth`]) identifies the caller. Permission
//! decides whether *that* caller can perform *this* action on *this*
//! resource. The two halves are intentionally split — different
//! resources want different auth backends sometimes, but more often
//! want different permission rules on top of one shared auth.
//!
//! ## The contract
//!
//! [`Permission::check`] is synchronous and takes:
//!
//! - The [`Action`] the caller is trying to perform (`List` /
//!   `Retrieve` / `Create` / `Update` / `Delete`).
//! - The `Option<&Identity>` authentication produced (`None` when
//!   the request is anonymous).
//!
//! Returns `Ok(())` to allow, `Err(PermissionError::Unauthenticated)`
//! to demand auth (401), or `Err(PermissionError::Forbidden)` to
//! deny an authenticated request (403).
//!
//! ## Built-ins
//!
//! - [`AllowAny`] — default. Every action allowed, anonymous OK.
//! - [`IsAuthenticated`] — require some identity for any action.
//! - [`IsStaff`] — require an identity with `is_staff = true`.
//! - [`ReadOnly`] — allow List/Retrieve to anyone, deny everything
//!   else.
//! - [`OrPermission`] — short-circuit OR over a list of permissions.
//! - [`AndPermission`] — AND over a list (every one must allow).
//!
//! Custom permission classes — `IsOwner`, scope checks, org-membership
//! filters — implement the trait directly and attach via
//! [`crate::ResourceConfig::permission`].

use crate::auth::Identity;

/// The operations a REST resource can expose. The five built-ins
/// (`List` / `Retrieve` / `Create` / `Update` / `Delete`) cover the
/// standard CRUD surface; `Custom(name)` is the DRF `@action` shape
/// for endpoints that don't fit CRUD (publish, archive, recent,
/// etc.).
///
/// Permission impls dispatch on this enum so one `Permission` can
/// vary behaviour by action — `ReadOnly` is the canonical case;
/// custom-action authors usually treat `Custom(_)` as a write
/// (because it isn't List/Retrieve), but a permission can also
/// special-case `Custom("recent")` if it wants.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    /// `GET /api/<table>/` — list all matching rows.
    List,
    /// `GET /api/<table>/<id>` — fetch one row.
    Retrieve,
    /// `POST /api/<table>/` — create a new row.
    Create,
    /// `PUT` / `PATCH /api/<table>/<id>` — modify an existing row.
    Update,
    /// `DELETE /api/<table>/<id>` — remove a row.
    Delete,
    /// A `@action`-style custom endpoint registered via
    /// [`crate::ResourceConfig::action`]. The string is the action
    /// name (e.g. `"publish"`, `"recent"`) and matches the URL
    /// segment.
    Custom(String),
}

impl Action {
    /// True for the read-only built-ins (List, Retrieve). Custom
    /// actions return false here — declare them with a permission
    /// that special-cases the name if you need read-only semantics.
    pub fn is_read(&self) -> bool {
        matches!(self, Action::List | Action::Retrieve)
    }

    /// True iff this is a custom (`@action`) endpoint with the given
    /// name. Useful in `Permission::check` impls that want to allow
    /// or deny specific custom actions.
    pub fn is_custom(&self, name: &str) -> bool {
        matches!(self, Action::Custom(n) if n == name)
    }
}

/// Permission denial. Mapped to 401 / 403 in the handler dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionError {
    /// "You need to authenticate." Surfaces as HTTP 401.
    Unauthenticated,
    /// "Authenticated, but not allowed." Surfaces as HTTP 403.
    Forbidden,
}

impl std::fmt::Display for PermissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthenticated => write!(f, "authentication required"),
            Self::Forbidden => write!(f, "forbidden"),
        }
    }
}

impl std::error::Error for PermissionError {}

/// The permission contract. Returns `Ok(())` to allow, an error to
/// deny. Sync because permission checks don't hit the database —
/// they walk an in-memory rule set against the (already-resolved)
/// `Identity`.
///
/// Takes `&Action` (not `Action`) because `Action::Custom(String)`
/// is the action variant for `@action`-style endpoints — passing by
/// reference avoids a clone per request.
pub trait Permission: Send + Sync + 'static {
    fn check(&self, action: &Action, identity: Option<&Identity>) -> Result<(), PermissionError>;

    /// True when this permission grants every action to anonymous
    /// callers (i.e. [`AllowAny`]). Used only by the boot-time security
    /// warning (WEB-1) that flags resources left wide open; defaults to
    /// `false` so a custom permission is never mistaken for open.
    fn is_open(&self) -> bool {
        false
    }
}

// =========================================================================
// Built-ins
// =========================================================================

/// Allow every action, anonymous OK. The default.
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAny;

impl Permission for AllowAny {
    fn check(&self, _action: &Action, _identity: Option<&Identity>) -> Result<(), PermissionError> {
        Ok(())
    }

    fn is_open(&self) -> bool {
        true
    }
}

/// Require ANY authenticated identity. Anonymous → 401.
#[derive(Debug, Default, Clone, Copy)]
pub struct IsAuthenticated;

impl Permission for IsAuthenticated {
    fn check(&self, _action: &Action, identity: Option<&Identity>) -> Result<(), PermissionError> {
        if identity.is_some() {
            Ok(())
        } else {
            Err(PermissionError::Unauthenticated)
        }
    }
}

/// Require an authenticated identity with `is_staff = true`.
/// Anonymous → 401. Non-staff authenticated → 403.
#[derive(Debug, Default, Clone, Copy)]
pub struct IsStaff;

impl Permission for IsStaff {
    fn check(&self, _action: &Action, identity: Option<&Identity>) -> Result<(), PermissionError> {
        match identity {
            None => Err(PermissionError::Unauthenticated),
            Some(id) if id.is_staff => Ok(()),
            Some(_) => Err(PermissionError::Forbidden),
        }
    }
}

/// Allow List / Retrieve to anyone (including anonymous), deny
/// Create / Update / Delete unconditionally. The canonical
/// public-read-private-write shape.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReadOnly;

impl Permission for ReadOnly {
    fn check(&self, action: &Action, _identity: Option<&Identity>) -> Result<(), PermissionError> {
        if action.is_read() {
            Ok(())
        } else {
            Err(PermissionError::Forbidden)
        }
    }
}

// =========================================================================
// Combinators: AND / OR
// =========================================================================

/// Pass when ANY underlying permission passes. Short-circuits on the
/// first success. Useful for "staff OR is the resource owner" rules.
pub struct OrPermission {
    perms: Vec<Box<dyn Permission>>,
}

impl OrPermission {
    pub fn new(perms: Vec<Box<dyn Permission>>) -> Self {
        Self { perms }
    }
}

impl Permission for OrPermission {
    fn check(&self, action: &Action, identity: Option<&Identity>) -> Result<(), PermissionError> {
        let mut last_err = PermissionError::Forbidden;
        for p in &self.perms {
            match p.check(action, identity) {
                Ok(()) => return Ok(()),
                Err(e) => last_err = e,
            }
        }
        // Preserve the last error so a chain of [IsAuthenticated, IsStaff]
        // on anonymous traffic surfaces as 401 (the IsAuthenticated
        // error), not 403.
        Err(last_err)
    }
}

/// Pass when EVERY underlying permission passes. Short-circuits on
/// the first failure.
pub struct AndPermission {
    perms: Vec<Box<dyn Permission>>,
}

impl AndPermission {
    pub fn new(perms: Vec<Box<dyn Permission>>) -> Self {
        Self { perms }
    }
}

impl Permission for AndPermission {
    fn check(&self, action: &Action, identity: Option<&Identity>) -> Result<(), PermissionError> {
        for p in &self.perms {
            p.check(action, identity)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> Identity {
        Identity::user(1)
    }
    fn admin() -> Identity {
        Identity::user(2).staff()
    }

    #[test]
    fn allow_any_lets_everything_through() {
        for action in [
            Action::List,
            Action::Retrieve,
            Action::Create,
            Action::Update,
            Action::Delete,
        ] {
            assert!(AllowAny.check(&action, None).is_ok());
            assert!(AllowAny.check(&action, Some(&alice())).is_ok());
        }
    }

    #[test]
    fn is_authenticated_demands_identity() {
        assert_eq!(
            IsAuthenticated.check(&Action::List, None),
            Err(PermissionError::Unauthenticated)
        );
        assert!(IsAuthenticated.check(&Action::List, Some(&alice())).is_ok());
    }

    #[test]
    fn is_staff_requires_staff_flag() {
        assert_eq!(
            IsStaff.check(&Action::List, None),
            Err(PermissionError::Unauthenticated)
        );
        assert_eq!(
            IsStaff.check(&Action::List, Some(&alice())),
            Err(PermissionError::Forbidden)
        );
        assert!(IsStaff.check(&Action::List, Some(&admin())).is_ok());
    }

    #[test]
    fn read_only_allows_reads_denies_writes() {
        for read_action in [Action::List, Action::Retrieve] {
            assert!(ReadOnly.check(&read_action, None).is_ok());
            assert!(ReadOnly.check(&read_action, Some(&admin())).is_ok());
        }
        for write_action in [Action::Create, Action::Update, Action::Delete] {
            assert_eq!(
                ReadOnly.check(&write_action, Some(&admin())),
                Err(PermissionError::Forbidden)
            );
        }
        // ReadOnly's "is_read" check returns false for Custom — a
        // custom action is treated as a write by default and gets
        // denied.
        assert_eq!(
            ReadOnly.check(&Action::Custom("publish".into()), Some(&admin())),
            Err(PermissionError::Forbidden)
        );
    }

    #[test]
    fn or_permission_short_circuits_on_success() {
        let perm = OrPermission::new(vec![Box::new(IsStaff), Box::new(IsAuthenticated)]);
        // Alice isn't staff but is authenticated → OR passes.
        assert!(perm.check(&Action::List, Some(&alice())).is_ok());
        // Anonymous fails both, surfaces as Unauthenticated (the last
        // error from IsAuthenticated).
        assert_eq!(
            perm.check(&Action::List, None),
            Err(PermissionError::Unauthenticated)
        );
    }

    #[test]
    fn and_permission_requires_all() {
        let perm = AndPermission::new(vec![Box::new(IsAuthenticated), Box::new(IsStaff)]);
        // Alice authenticated but not staff → fails on IsStaff.
        assert_eq!(
            perm.check(&Action::List, Some(&alice())),
            Err(PermissionError::Forbidden)
        );
        // Admin satisfies both.
        assert!(perm.check(&Action::List, Some(&admin())).is_ok());
    }

    #[test]
    fn action_is_custom_matches_the_name() {
        let a = Action::Custom("publish".into());
        assert!(a.is_custom("publish"));
        assert!(!a.is_custom("recent"));
        assert!(!Action::List.is_custom("publish"));
    }
}
