//! ORM heavy-relations epic — TaskFlow #447.
//!
//! `back_fk_column::<Parent, Child>()` (crates/umbral-core/src/orm/relation.rs)
//! is the type-only reverse-FK resolver: given only the two model types, it
//! finds the column on `Child` that anchors the back-link to `Parent`. It
//! used to pick the FIRST matching `ForeignKey<Parent>` field silently. If
//! `Child` declares TWO such fields, that's a genuine ambiguity — this suite
//! proves it now panics loudly, naming both candidates, instead of guessing.
//!
//! `back_fk_column` reads only `Model::FIELDS`/`Model::TABLE`/`Model::NAME`
//! (all derive-emitted statics), so no app registration or database is
//! needed to exercise it.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use umbral::orm::ForeignKey;
use umbral_core::orm::relation::back_fk_column;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "bfk_parent")]
pub struct Parent {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
}

/// The happy path: exactly one `ForeignKey<Parent>` — unambiguous.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "bfk_only_child")]
pub struct OnlyChild {
    #[umbral(primary_key)]
    pub id: i64,
    pub parent: ForeignKey<Parent>,
}

/// The ambiguous case: TWO `ForeignKey<Parent>` fields on the same child
/// (e.g. a "reporting line" shape — a primary manager and a secondary one,
/// both pointing at the same parent table).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "bfk_dual_child")]
pub struct DualChild {
    #[umbral(primary_key)]
    pub id: i64,
    pub primary_parent: ForeignKey<Parent>,
    pub secondary_parent: ForeignKey<Parent>,
}

#[test]
fn single_fk_resolves_the_right_column() {
    assert_eq!(back_fk_column::<Parent, OnlyChild>(), "parent");
}

#[test]
#[should_panic(expected = "ambiguous reverse relation from `Parent` to `DualChild`")]
fn two_fks_to_same_parent_panics_naming_both_candidates() {
    back_fk_column::<Parent, DualChild>();
}

#[test]
fn two_fks_panic_message_names_every_candidate_column() {
    let result = std::panic::catch_unwind(back_fk_column::<Parent, DualChild>);
    let err = result.expect_err("back_fk_column must panic on 2+ candidate FKs");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .expect("panic payload is a string message");

    assert!(
        msg.contains("primary_parent"),
        "panic message should name the first candidate column: {msg}"
    );
    assert!(
        msg.contains("secondary_parent"),
        "panic message should name the second candidate column: {msg}"
    );
    assert!(
        msg.contains("DualChild"),
        "panic message should name the Child model: {msg}"
    );
    assert!(
        msg.contains("Parent"),
        "panic message should name the Parent model: {msg}"
    );
    assert!(
        msg.contains("_via_") || msg.contains("via_"),
        "panic message should point at the disambiguated `<child>_via_<field>_set` \
         accessor: {msg}"
    );
}
