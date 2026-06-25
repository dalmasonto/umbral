//! Choices: closed-set enum field support.
//!
//! A user enum that implements [`ChoiceField`] can be used as a model
//! field type via `#[umbral(choices)]`. The framework stores the variant
//! as TEXT in the database; the Rust type system is the structural
//! constraint, with Postgres adding a `CHECK (col IN (...))` belt-and-
//! braces guard so a third-party process writing directly to the DB can't
//! insert a value the Rust enum can't model.
//!
//! Implementing the trait by hand is fine, but the common path is the
//! `#[derive(Choices)]` macro on a unit-variant enum:
//!
//! ```ignore
//! use umbral::prelude::*;
//!
//! #[derive(Debug, Clone, Copy, PartialEq, Eq, Choices)]
//! #[choices(rename_all = "lowercase")]
//! pub enum PostStatus {
//!     Draft,
//!     Review,
//!     Published,
//!     Archived,
//! }
//! ```
//!
//! The derive also emits the sqlx `Type` / `Encode` / `Decode` impls (for
//! Postgres and SQLite, both as `TEXT`), `Display`, and `FromStr` — so
//! the same enum value round-trips through the ORM, the admin form, and a
//! `Form` validator without any glue.

/// A field type whose values are drawn from a small, fixed set known at
/// compile time.
///
/// Implementors expose the value list (the strings stored in the
/// database) and matching human labels (used by the admin's `<select>`
/// widget). Position-for-position correspondence: `LABELS[i]` labels
/// `VALUES[i]`.
///
/// The trait is `Copy` so a `FieldSpec` referencing it stays usable in a
/// `const` slice — the same constraint we have on every other model
/// field type.
pub trait ChoiceField: Sized + Copy + 'static {
    /// The DB-stored string for each variant, in declaration order.
    const VALUES: &'static [&'static str];
    /// Human label per variant, in declaration order. Same length as
    /// [`Self::VALUES`]. The derive defaults each label to the Rust
    /// variant name (Title-Case-friendly) when `#[choices(label = "...")]`
    /// is not supplied.
    const LABELS: &'static [&'static str];

    /// The DB-stored string for this value.
    fn as_str(&self) -> &'static str;

    /// Parse the DB-stored string back into a variant. Returns `None`
    /// when the input doesn't match any of [`Self::VALUES`] — sqlx's
    /// `Decode` impl uses this and reports a typed error.
    fn from_str_ok(s: &str) -> Option<Self>;
}
