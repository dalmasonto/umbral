//! `ScopeDecision::RestrictIn` binds its values as STRINGS — the scope hooks
//! speak strings because `Identity` ids are strings by contract. But the
//! scoped column is usually a BIGINT id/FK, and Postgres refuses the mixed
//! comparison outright: `operator does not exist: bigint = text` — every
//! scoped LIST endpoint 500s. SQLite coerces silently, which is why the
//! membership_scope suite (SQLite-backed) never caught it; it surfaced the
//! first time TaskFlow's scoped REST ran against a real Postgres.
//!
//! The fix types the bound values from the COLUMN's `SqlType`:
//!   * integer-family column → parse and bind i64s. A value that does not
//!     parse can never match an integer column, so it is dropped rather than
//!     poisoning the query's types; if EVERY value drops, that is an empty
//!     membership and must stay a deny-all (never unconstrained).
//!   * text/unknown column → bind the strings unchanged (String-PK models
//!     keep working exactly as before).
//!
//! `typed_restrict_in_values` is the pure decision kernel, tested here
//! without a database.

use umbral::orm::SqlType;
use umbral_rest::typed_restrict_in_values;

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn integer_columns_bind_parsed_i64s() {
    for ty in [
        SqlType::BigInt,
        SqlType::Integer,
        SqlType::SmallInt,
        SqlType::ForeignKey,
    ] {
        match typed_restrict_in_values(Some(ty), strings(&["2", "7"])) {
            umbral_rest::TypedScopeValues::Ints(ints) => assert_eq!(ints, vec![2, 7], "{ty:?}"),
            other => panic!("{ty:?} must bind ints, got {other:?}"),
        }
    }
}

#[test]
fn non_numeric_values_are_dropped_for_integer_columns() {
    match typed_restrict_in_values(Some(SqlType::BigInt), strings(&["2", "not-a-number"])) {
        umbral_rest::TypedScopeValues::Ints(ints) => assert_eq!(ints, vec![2]),
        other => panic!("expected ints, got {other:?}"),
    }
}

/// All values unparseable = the caller belongs to nothing this column can
/// express — that must be DENY, the same fail-closed default as an empty
/// membership list.
#[test]
fn all_unparseable_is_deny_not_unconstrained() {
    match typed_restrict_in_values(Some(SqlType::BigInt), strings(&["nope", "also-no"])) {
        umbral_rest::TypedScopeValues::Deny => {}
        other => panic!("expected Deny, got {other:?}"),
    }
}

#[test]
fn text_columns_keep_their_strings() {
    match typed_restrict_in_values(Some(SqlType::Text), strings(&["user:42", "email:x@y"])) {
        umbral_rest::TypedScopeValues::Strings(vals) => {
            assert_eq!(vals, strings(&["user:42", "email:x@y"]))
        }
        other => panic!("expected strings, got {other:?}"),
    }
}

/// An unknown column (model not in the registry, or a computed column) must
/// not change behavior: bind strings, exactly as before the fix.
#[test]
fn unknown_column_type_keeps_strings() {
    match typed_restrict_in_values(None, strings(&["3"])) {
        umbral_rest::TypedScopeValues::Strings(vals) => assert_eq!(vals, strings(&["3"])),
        other => panic!("expected strings, got {other:?}"),
    }
}
