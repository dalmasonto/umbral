//! gaps3 #28 [orm #3] — the built-in `AuthUser`'s privilege-bearing columns
//! must stay protected against mass-assignment on the untrusted dynamic write
//! path (REST create/update, admin form submit).
//!
//! The mechanism is `#[umbral(privileged)]` (deny-by-default: stripped from
//! `insert_json`/`update_json` unless the caller opts it back in with
//! `DynQuerySet::allow_privileged`, e.g. after verifying the requester is a
//! superuser) and `#[umbral(noform)]` (never taken from a form/JSON body).
//! `privileged_field.rs` in umbral-core proves the *mechanism*; this proves the
//! built-in `AuthUser` actually *uses* it, so a future refactor that drops an
//! attribute can't silently re-open the escalation (`{"is_superuser": true}`).

use umbral::orm::Model;
use umbral_auth::AuthUser;

fn field(name: &str) -> &'static umbral::orm::FieldSpec {
    <AuthUser as Model>::FIELDS
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("AuthUser has no field `{name}`"))
}

#[test]
fn auth_user_privilege_columns_are_default_deny() {
    // Escalation vectors: a client body must NOT be able to grant these.
    assert!(
        field("is_superuser").privileged,
        "AuthUser.is_superuser must be #[umbral(privileged)] (mass-assignment guard)"
    );
    assert!(
        field("is_staff").privileged,
        "AuthUser.is_staff must be #[umbral(privileged)] (mass-assignment guard)"
    );
}

#[test]
fn auth_user_password_hash_is_never_form_writable() {
    // The password hash is set only through the auth flows (register / change
    // password), never accepted from a form/JSON body.
    assert!(
        field("password_hash").noform,
        "AuthUser.password_hash must be #[umbral(noform)]"
    );
}

/// gaps3 #34 — `username` / `email` carry `#[umbral(trim, lowercase)]` so the
/// dynamic write path (admin form-submit, REST create/update) canonicalizes
/// them too, closing the #33 residual (those paths bypass the `create_user`
/// helper's explicit normalization). A refactor dropping the attribute would
/// silently re-open the case-variant-duplicate hole on the admin/REST surface.
#[test]
fn auth_user_identifier_columns_normalize_on_dynamic_writes() {
    for name in ["username", "email"] {
        let f = field(name);
        assert!(
            f.trim && f.lowercase,
            "AuthUser.{name} must be #[umbral(trim, lowercase)] so admin/REST writes canonicalize"
        );
    }
}
