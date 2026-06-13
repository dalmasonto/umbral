//! `ReverseSet<C>` — reverse-FK collection field on a parent model.
//!
//! Gap #44 / feature #19's remaining open item. Stores no SQL column
//! on the parent table; the related rows live in `C`'s own table with
//! a FK column pointing back at the parent. After
//! `.prefetch_related("comment_set")`, the slot is populated with
//! every child whose FK matches the parent's PK.
//!
//! ## Declaration
//!
//! On the parent struct, mark the field `#[sqlx(skip)]` +
//! `#[serde(skip)]` (no DB column to decode, no JSON shape to emit
//! by default) and tag it with `#[umbra(reverse_fk = "<fk_col>")]`
//! naming the FK column on the child that points back:
//!
//! ```rust,ignore
//! #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
//! pub struct Post {
//!     pub id: i64,
//!     pub title: String,
//!     /// Comment has `pub post: ForeignKey<Post>` — that's the
//!     /// "post" the attribute names.
//!     #[sqlx(skip)]
//!     #[serde(skip)]
//!     #[umbra(reverse_fk = "post")]
//!     pub comment_set: ReverseSet<Comment>,
//! }
//! ```
//!
//! ## Loading
//!
//! ```rust,ignore
//! let posts = Post::objects()
//!     .prefetch_related("comment_set")
//!     .fetch()
//!     .await?;
//! for post in &posts {
//!     for comment in post.comment_set.resolved().unwrap() {
//!         println!("{}: {}", post.title, comment.body);
//!     }
//! }
//! ```
//!
//! Query budget: 1 (parents) + 1 (children) regardless of parent
//! count. No N+1.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use super::Model;

/// A reverse-FK collection field on a parent model. The framework
/// fills `resolved` via `.prefetch_related(field_name)`; without that
/// chain method `resolved()` returns `None` and `set_parent_id` /
/// `set_fk_column` stay unset (the field is inert).
#[derive(Debug, Clone)]
pub struct ReverseSet<C: Model> {
    /// Cached parent-row PK as a `serde_json::Value` (PK lift — was
    /// `Option<i64>`). Set by the macro-emitted `set_m2m_parent_ids` hook
    /// (which post-#44 covers both M2M and reverse-FK slots) after each
    /// parent row is decoded. Holding the PK shape-agnostically lets a
    /// `String`/slug- or `Uuid`-PK parent carry a `ReverseSet` field; the
    /// prefetch loader groups children by the parent's `pk_as_json()`
    /// regardless of this cache.
    parent_id: Option<serde_json::Value>,
    /// Name of the FK column on `C` that points back at the parent.
    /// Set by the same macro hook from the `#[umbra(reverse_fk =
    /// "...")]` attribute. The prefetch loader emits
    /// `WHERE <fk_column> IN (parent_pks)` against `C::TABLE`.
    fk_column: Option<&'static str>,
    /// Resolved children. `None` = not loaded
    /// (`.prefetch_related(...)` wasn't called for this field).
    /// `Some(vec![])` = loaded but no matching children, distinct
    /// from "not loaded yet" so callers can branch.
    resolved: Option<Vec<C>>,
    _phantom: PhantomData<C>,
}

/// `Default` is what the `sqlx::FromRow` `#[sqlx(skip)]` path uses
/// to fill the slot. `HydrateRelated::set_m2m_parent_ids` then seeds
/// `parent_id` + `fk_column` from the just-decoded parent row.
impl<C: Model> Default for ReverseSet<C> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<C: Model> ReverseSet<C> {
    /// Construct an empty (unloaded, no parent yet) ReverseSet.
    pub fn empty() -> Self {
        Self {
            parent_id: None,
            fk_column: None,
            resolved: None,
            _phantom: PhantomData,
        }
    }

    /// Borrow the resolved children as a slice. `None` means
    /// prefetch wasn't called for this field; the framework never
    /// silently loads children on first access (no lazy loading by
    /// design — Rust has no property accessors to intercept and
    /// hidden round-trips are surprising).
    pub fn resolved(&self) -> Option<&[C]> {
        self.resolved.as_deref()
    }

    /// Set the parent's PK on this slot so the prefetch loader knows
    /// which `WHERE <fk_column> = parent_pk` bucket to target.
    /// Called by the macro-emitted `set_m2m_parent_ids` arm with the
    /// parent's PK as a `serde_json::Value` (PK lift — was `i64`).
    pub fn set_parent_id(&mut self, id: serde_json::Value) {
        self.parent_id = Some(id);
    }

    /// Set the FK column name on the child. Called by the same macro
    /// hook from the `#[umbra(reverse_fk = "...")]` attribute.
    pub fn set_fk_column(&mut self, col: &'static str) {
        self.fk_column = Some(col);
    }

    /// Read the parent PK + FK column the prefetch loader needs.
    /// `None` for either means this slot was never wired up (the
    /// macro didn't see a `#[umbra(reverse_fk = ...)]` attribute, or
    /// `set_m2m_parent_ids` wasn't called yet). Returns
    /// `Option<(parent_id, fk_column)>` so the caller can early-exit
    /// without a separate isset check.
    pub fn parent_link(&self) -> Option<(&serde_json::Value, &'static str)> {
        match (&self.parent_id, self.fk_column) {
            (Some(id), Some(col)) => Some((id, col)),
            _ => None,
        }
    }

    /// Populate the resolved bucket. Called once by the prefetch
    /// loader after grouping the batched child rows by `fk_column`
    /// value.
    pub fn set_resolved(&mut self, rows: Vec<C>) {
        self.resolved = Some(rows);
    }
}

/// Serialize: emit the resolved children (or `[]` if not yet
/// loaded). Symmetric with `M2M`'s shape so templates / REST
/// serialisation doesn't surprise users with `null` for unloaded
/// slots.
impl<C: Model + Serialize> Serialize for ReverseSet<C> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match &self.resolved {
            Some(rows) => rows.serialize(s),
            None => Vec::<C>::new().serialize(s),
        }
    }
}

/// Deserialize: accepts a JSON array of `C` (the
/// already-resolved shape). Round-trip support for a prefetched
/// parent. Tolerates `null` / missing by returning an unloaded
/// default — the same shape `#[serde(skip)]` would produce.
impl<'de, C: Model + Deserialize<'de>> Deserialize<'de> for ReverseSet<C> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let opt = Option::<Vec<C>>::deserialize(d).unwrap_or(None);
        Ok(Self {
            parent_id: None,
            fk_column: None,
            resolved: opt,
            _phantom: PhantomData,
        })
    }
}

// =========================================================================
// sqlx: same "should never run" safety net as M2M<T>. ReverseSet fields
// must be marked `#[sqlx(skip)]` on the parent struct so FromRow uses
// the Default impl rather than trying to decode a column that doesn't
// exist. These impls exist only to keep code that accidentally selects
// a ReverseSet column from hard-erroring.
// =========================================================================

impl<C: Model> sqlx::Type<sqlx::Sqlite> for ReverseSet<C> {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <i64 as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

impl<C: Model> sqlx::Type<sqlx::Postgres> for ReverseSet<C> {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i64 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r, C: Model> sqlx::Decode<'r, sqlx::Sqlite> for ReverseSet<C> {
    fn decode(
        value: sqlx::sqlite::SqliteValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let _ = <i64 as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
        Ok(Self::empty())
    }
}

impl<'r, C: Model> sqlx::Decode<'r, sqlx::Postgres> for ReverseSet<C> {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let _ = <i64 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::empty())
    }
}
