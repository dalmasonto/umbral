//! Fields that must never reach a client, on any transport.
//!
//! # Why this lives in core and not in a plugin
//!
//! It used to live in `umbral-rest` (`HARD_DENIED_FIELDS`), which meant `password_hash` was
//! safe only if you happened to have mounted REST. `umbral-openapi` had to reach across and
//! call `umbral_rest::is_hidden` to stay consistent with it — a dependency between two
//! *optional, swappable* plugins, which is the shape of a rule living in the wrong place.
//! And `umbral-graphql`, added later, inherited none of it: it exposed every column of every
//! model it was pointed at, so `.expose("auth_user")` would have served password hashes.
//!
//! Secrecy is a property of the **data**, not of the transport. `password_hash` is not
//! confidential because of which door you walk through to reach it. So the rule belongs at
//! the centre, where every plugin — including ones nobody has written yet — inherits it
//! without having to remember to.
//!
//! This is the read-path twin of [`crate::migrate::Column::privileged`], which is already a
//! model-level, default-deny guard for the *write* path (mass assignment). Disclosure had
//! no equivalent. Now it does.
//!
//! # Hard-denied, not merely hidden
//!
//! There is deliberately **no unlock**. A plugin's own `hide()` list is configuration and can
//! be reconfigured; this cannot. The whole value of a tier with no escape hatch is that
//! nobody can reach for it at 2am under a deadline. An admin UI that needs to show *whether*
//! a password is set should show "set / not set" — never the hash. Django's admin has made
//! exactly this call for twenty years.
//!
//! See `planning/gaps3.md` for the broader `#[umbral(private)]` / `#[umbral(secret)]` design
//! this is the first, narrow slice of.

/// Column names that never appear in a serialized response, regardless of any plugin's
/// `expose` / `hide` configuration.
///
/// Matched on the column name alone, across every table: a model that names a column
/// `password_hash` means the same thing whatever the table is called, and the failure mode
/// of matching too broadly (a field is missing from an API) is survivable in a way that the
/// failure mode of matching too narrowly (a password hash on the wire) is not.
pub const HARD_DENIED_FIELDS: &[&str] = &["password_hash"];

/// Whether `field` may never be serialized. See [`HARD_DENIED_FIELDS`].
pub fn is_hard_denied_field(field: &str) -> bool {
    HARD_DENIED_FIELDS.contains(&field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_is_denied_on_every_table() {
        // The denylist is keyed on the column name, not (table, column) — a model that
        // calls a column `password_hash` means the same thing wherever it lives.
        assert!(is_hard_denied_field("password_hash"));
    }

    #[test]
    fn ordinary_fields_are_not_denied() {
        // A denylist that denied everything would be "secure" and useless.
        assert!(!is_hard_denied_field("name"));
        assert!(!is_hard_denied_field("password_changed_at"));
    }
}
