//! gaps2 #45 (accessor half) — the zero-declaration instance
//! reverse-relation accessor: `post.comment_set.all()` as a
//! generic runtime method available on EVERY model instance, with no
//! `ReverseSet` field declared on the parent.
//!
//! Why a runtime accessor (not a derive-emitted method): Rust
//! proc-macros can't enumerate a parent's children at the parent's
//! derive site — the children live in other modules / crates and the
//! macro only sees the struct it's expanding. So the child type `C` is
//! named at the CALL site, and the FK column on `C` that points back at
//! `Self` is discovered at runtime from `C::FIELDS`:
//!
//! ```ignore
//! let kids = parent.reverse::<Comment>()?.fetch().await?;
//! let recent = parent.reverse::<Comment>()?
//!     .filter(comment::CREATED.gt(cutoff))
//!     .order_by(comment::CREATED.desc())
//!     .fetch().await?;
//! // Disambiguate when C has more than one FK to this parent:
//! let via = parent.reverse_via::<Comment>("author")?.fetch().await?;
//! ```
//!
//! The return type is a real chainable [`QuerySet<C>`] — every
//! `.filter()/.order_by()/.exclude()/.fetch()/.count()/.exists()`
//! terminal works on it, exactly like `C::objects().filter(...)`. The
//! discovery + parent-PK read are synchronous and fallible, so the
//! accessor returns `Result<QuerySet<C>, ReverseError>` up front; the
//! QuerySet itself stays lazy and awaitable.
//!
//! Parent PKs are bound through the same JSON-to-SQL coercion as the
//! rest of the ORM relation machinery, so i64, String, and UUID PKs all
//! work.

use sea_query::{Alias, Expr};

use super::Predicate;
use super::model::{HydrateRelated, Model};
use super::queryset::{Manager, QuerySet};

/// Why an instance reverse accessor couldn't build its QuerySet. All
/// variants carry the names involved so the message is actionable
/// (which models, which columns, which path to take instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReverseError {
    /// `C` declares no `ForeignKey<_>` whose target table is this
    /// parent's table — there's nothing to reverse from.
    NoForeignKey {
        child: &'static str,
        parent_table: &'static str,
    },
    /// `C` declares MORE THAN ONE FK to this parent. The call is
    /// ambiguous; `reverse_via::<C>("<col>")` picks one explicitly.
    Ambiguous {
        child: &'static str,
        parent_table: &'static str,
        candidates: Vec<&'static str>,
    },
    /// `reverse_via` was given a column that doesn't exist on `C`.
    UnknownColumn { child: &'static str, column: String },
    /// `reverse_via` was given a column that exists but isn't a FK to
    /// this parent's table.
    NotAForeignKey {
        child: &'static str,
        column: String,
        parent_table: &'static str,
    },
    /// This instance's PK could not be read or bound into the child FK
    /// predicate.
    NonI64Pk { parent: &'static str },
}

impl std::fmt::Display for ReverseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReverseError::NoForeignKey {
                child,
                parent_table,
            } => write!(
                f,
                "umbral::orm::reverse: `{child}` has no foreign key to `{parent_table}` \
                 — there is no reverse relation to follow"
            ),
            ReverseError::Ambiguous {
                child,
                parent_table,
                candidates,
            } => write!(
                f,
                "umbral::orm::reverse: `{child}` has multiple foreign keys to `{parent_table}` \
                 ({}). Disambiguate with `reverse_via::<{child}>(\"<column>\")`",
                candidates.join(", ")
            ),
            ReverseError::UnknownColumn { child, column } => write!(
                f,
                "umbral::orm::reverse_via: `{child}` has no column `{column}`"
            ),
            ReverseError::NotAForeignKey {
                child,
                column,
                parent_table,
            } => write!(
                f,
                "umbral::orm::reverse_via: column `{column}` on `{child}` is not a foreign key \
                 to `{parent_table}`"
            ),
            ReverseError::NonI64Pk { parent } => write!(
                f,
                "umbral::orm::reverse: `{parent}` primary key could not be bound into the \
                 reverse relation predicate"
            ),
        }
    }
}

impl std::error::Error for ReverseError {}

/// Generic instance reverse-relation accessors. Blanket-implemented for
/// every model (anything that is both [`Model`] and [`HydrateRelated`],
/// which is every `#[derive(Model)]` type), so a fetched instance of
/// any model gets `.reverse::<C>()` / `.reverse_via::<C>(col)` for free.
pub trait ReverseRelations: Model + HydrateRelated {
    /// Build a chainable [`QuerySet<C>`] of the children of type `C`
    /// whose foreign key points at THIS instance. The FK column is
    /// discovered from `C::FIELDS` (the field whose `fk_target ==
    /// Self::TABLE`):
    ///
    /// - exactly one → that column is used,
    /// - zero → [`ReverseError::NoForeignKey`],
    /// - two or more → [`ReverseError::Ambiguous`] (use [`reverse_via`]).
    ///
    /// [`reverse_via`]: ReverseRelations::reverse_via
    fn reverse<C: Model + HydrateRelated>(&self) -> Result<QuerySet<C>, ReverseError> {
        let fk_col = discover_single_fk::<Self, C>()?;
        self.reverse_on::<C>(fk_col)
    }

    /// Like [`reverse`], but with the FK column on `C` named explicitly
    /// — the escape hatch for when `C` has more than one FK to this
    /// parent. The column is validated to exist AND to be an FK to
    /// `Self::TABLE`.
    ///
    /// [`reverse`]: ReverseRelations::reverse
    fn reverse_via<C: Model + HydrateRelated>(
        &self,
        fk_col: &str,
    ) -> Result<QuerySet<C>, ReverseError> {
        let spec = C::FIELDS.iter().find(|f| f.name == fk_col).ok_or_else(|| {
            ReverseError::UnknownColumn {
                child: C::NAME,
                column: fk_col.to_string(),
            }
        })?;
        if spec.fk_target != Some(Self::TABLE) {
            return Err(ReverseError::NotAForeignKey {
                child: C::NAME,
                column: fk_col.to_string(),
                parent_table: Self::TABLE,
            });
        }
        self.reverse_on::<C>(spec.name)
    }

    /// Shared tail: read this instance's PK and build
    /// `C::objects().filter(<fk_col> = pk)`.
    #[doc(hidden)]
    fn reverse_on<C: Model + HydrateRelated>(
        &self,
        fk_col: &'static str,
    ) -> Result<QuerySet<C>, ReverseError> {
        let pk = self
            .pk_as_json()
            .ok_or(ReverseError::NonI64Pk { parent: Self::NAME })?;
        let spec = C::FIELDS.iter().find(|f| f.name == fk_col).ok_or_else(|| {
            ReverseError::UnknownColumn {
                child: C::NAME,
                column: fk_col.to_string(),
            }
        })?;
        let parent_pk_ty = Self::FIELDS.iter().find(|f| f.primary_key).map(|f| f.ty);
        let pk_value =
            crate::orm::write::json_to_sea_value(spec.ty, &pk, false, fk_col, parent_pk_ty)
                .map_err(|_| ReverseError::NonI64Pk { parent: Self::NAME })?;
        // Build the predicate from a runtime column name + the parent
        // PK. `Predicate::new` is crate-internal, which is exactly why
        // this accessor lives in umbral-core rather than in a plugin:
        // turning a runtime column name into a typed `Predicate<C>`
        // needs the crate-private constructor.
        let predicate: Predicate<C> = Predicate::new(Expr::col(Alias::new(fk_col)).eq(pk_value));
        Ok(Manager::<C>::new().filter(predicate))
    }
}

impl<T: Model + HydrateRelated> ReverseRelations for T {}

/// Scan `C::FIELDS` for the single FK whose target table is `P::TABLE`.
/// Zero → `NoForeignKey`; two+ → `Ambiguous` (names every candidate).
fn discover_single_fk<P: Model, C: Model>() -> Result<&'static str, ReverseError> {
    let candidates: Vec<&'static str> = C::FIELDS
        .iter()
        .filter(|f| f.fk_target == Some(P::TABLE))
        .map(|f| f.name)
        .collect();
    match candidates.len() {
        1 => Ok(candidates[0]),
        0 => Err(ReverseError::NoForeignKey {
            child: C::NAME,
            parent_table: P::TABLE,
        }),
        _ => Err(ReverseError::Ambiguous {
            child: C::NAME,
            parent_table: P::TABLE,
            candidates,
        }),
    }
}
