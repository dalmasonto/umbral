//! `OneToOne<C>` — reverse OneToOne accessor on a parent model.
//!
//! Counterpart to [`super::ReverseSet`] for the case where the
//! reverse cardinality is "at most one" (the child's FK column
//! carries a `UNIQUE` constraint — the
//! `#[umbra(unique)] pub user: ForeignKey<User>` idiom that
//! `migrate.rs:3263` emits as `UNIQUE` inline). Returns
//! `Option<&C>` from `resolved()` rather than `Option<&[C]>`, so
//! callers (and templates) can write `user.profile.avatar`
//! directly without `.first()` gymnastics.
//!
//! Zero-config — no `#[umbra(one_to_one = "...")]` attribute is
//! needed. The framework discovers the back-link at runtime by
//! scanning the child's `FIELDS` for the UNIQUE FK pointing at the
//! parent's table. Exactly one match is required; 0 or 2+ matches
//! surface a loud error at prefetch time naming the ambiguous
//! candidates.
//!
//! ## Declaration
//!
//! Same `#[sqlx(skip)] #[serde(skip)]` mechanical attributes the
//! other relation slots need (no DB column on this side), but no
//! umbra-specific attribute:
//!
//! ```rust,ignore
//! #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
//! pub struct User {
//!     pub id: i64,
//!     pub username: String,
//!     /// Profile has `#[umbra(unique)] pub user: ForeignKey<User>`
//!     /// — the unique FK is what makes this OneToOne.
//!     #[sqlx(skip)]
//!     #[serde(skip)]
//!     pub profile: OneToOne<Profile>,
//! }
//! ```
//!
//! ## Loading
//!
//! ```rust,ignore
//! let user = User::objects()
//!     .prefetch_related("profile")
//!     .get(user::ID.eq(1))
//!     .await?;
//! if let Some(profile) = user.profile.resolved() {
//!     println!("{}", profile.avatar);
//! }
//! ```
//!
//! Query budget: 1 (parents) + 1 (children) regardless of parent
//! count. Same no-N+1 guarantee as `ReverseSet`.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use super::Model;

/// A reverse OneToOne accessor on a parent model. The framework
/// fills `resolved` via `.prefetch_related(field_name)` after
/// finding the unique back-pointing FK column on `C` at runtime.
/// Without that chain method `resolved()` returns `None` and the
/// slot is inert.
#[derive(Debug, Clone)]
pub struct OneToOne<C: Model> {
    /// Cached parent-row PK. Set by the macro-emitted
    /// `set_m2m_parent_ids` hook so the loader knows which
    /// `WHERE <fk_column> = parent_pk` bucket to target. (The
    /// macro hook is named `set_m2m_parent_ids` for historical
    /// reasons — it now covers M2M, ReverseSet, and OneToOne
    /// slots uniformly.)
    parent_id: Option<i64>,
    /// Resolved child row. `None` = not loaded
    /// (`.prefetch_related(...)` wasn't called for this field).
    /// `Some(None)` is collapsed to a flat `None` because the
    /// "loaded, no row" case is rare and distinguishing it from
    /// "not loaded yet" wasn't worth a `Option<Option<C>>` API.
    /// Use `is_loaded()` if you need to tell the two apart.
    resolved: Option<Box<C>>,
    /// Set to `true` by the prefetch loader after it runs, even
    /// when the child wasn't found. Lets `is_loaded()` distinguish
    /// "no prefetch yet" from "prefetched, no match".
    loaded: bool,
    _phantom: PhantomData<C>,
}

/// `Default` is what the `sqlx::FromRow` `#[sqlx(skip)]` path uses
/// to fill the slot. `HydrateRelated::set_m2m_parent_ids` then
/// seeds `parent_id` from the just-decoded parent row.
impl<C: Model> Default for OneToOne<C> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<C: Model> OneToOne<C> {
    /// Construct an empty (unloaded, no parent yet) OneToOne.
    pub fn empty() -> Self {
        Self {
            parent_id: None,
            resolved: None,
            loaded: false,
            _phantom: PhantomData,
        }
    }

    /// Construct a child-side OneToOne carrying the FK value for the
    /// target row. Mirrors [`super::ForeignKey::new`]. Used when the
    /// field is declared `pub user: OneToOne<AuthUser>` (no
    /// `#[sqlx(skip)]`) — the macro routes such fields through the
    /// unique-FK column path, and `OneToOne<T>::new(id)` is what the
    /// caller writes to construct a row before insert. The `id` lands
    /// in the same `parent_id` slot the parent-side back-link uses,
    /// because the two directions never share an instance — a given
    /// `OneToOne<T>` is either child-side (FK value) or parent-side
    /// (parent PK for prefetch bucketing).
    pub fn new(id: i64) -> Self {
        Self {
            parent_id: Some(id),
            resolved: None,
            loaded: false,
            _phantom: PhantomData,
        }
    }

    /// Read the FK value on a child-side `OneToOne<T>`. Mirrors
    /// [`super::ForeignKey::id`]. Panics when called on an unset
    /// slot — the v1 contract matches `ForeignKey::id` (the caller
    /// constructed the row, so they should have set the FK).
    pub fn id(&self) -> i64 {
        self.parent_id
            .expect("OneToOne::id called on an unset slot — construct with OneToOne::new(id)")
    }

    /// Borrow the resolved child. `None` means either prefetch
    /// wasn't called OR the prefetch found no matching child. Use
    /// [`Self::is_loaded`] to distinguish the two cases.
    pub fn resolved(&self) -> Option<&C> {
        self.resolved.as_deref()
    }

    /// Returns `true` if `.prefetch_related(...)` populated this
    /// slot (regardless of whether a matching child was found).
    /// `false` means the slot was never loaded and `resolved()`
    /// returning `None` does not imply "no row exists" — it could
    /// just mean "we never asked."
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Read the parent's PK so the prefetch loader knows which
    /// `WHERE <fk_column> = parent_pk` bucket to target. `None`
    /// means this slot was never wired up.
    pub fn parent_id(&self) -> Option<i64> {
        self.parent_id
    }

    /// Set the parent's PK on this slot. Called by the macro-
    /// emitted `set_m2m_parent_ids` arm.
    pub fn set_parent_id(&mut self, id: i64) {
        self.parent_id = Some(id);
    }

    /// Populate the resolved bucket from a definitely-present child
    /// row. Mirrors [`super::ForeignKey::set_resolved`] so the
    /// child-side `OneToOne<T>` sugar can share the same
    /// macro-emitted hydration arm. Setting marks the slot as
    /// loaded.
    pub fn set_resolved(&mut self, row: C) {
        self.resolved = Some(Box::new(row));
        self.loaded = true;
    }

    /// Populate (or clear) the resolved bucket. Called by the
    /// parent-side prefetch loader after running the batched IN
    /// query. Setting `None` here is legitimate ("loaded but no
    /// matching row"); `is_loaded()` flips to true either way.
    pub fn set_resolved_opt(&mut self, row: Option<C>) {
        self.resolved = row.map(Box::new);
        self.loaded = true;
    }
}

/// Serialize: emit the resolved child (or `null` if not loaded /
/// no match). Symmetric with `ForeignKey<T>` when resolved — gives
/// templates and REST consumers a clean `obj.profile.avatar`
/// shape.
impl<C: Model + Serialize> Serialize for OneToOne<C> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match &self.resolved {
            Some(row) => row.serialize(s),
            None => s.serialize_none(),
        }
    }
}

/// Deserialize: accepts a JSON object (the already-resolved
/// shape) or `null` (unloaded / no match). Round-trip support for
/// a prefetched parent.
impl<'de, C: Model + Deserialize<'de>> Deserialize<'de> for OneToOne<C> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let opt = Option::<C>::deserialize(d).unwrap_or(None);
        let loaded = opt.is_some();
        Ok(Self {
            parent_id: None,
            resolved: opt.map(Box::new),
            loaded,
            _phantom: PhantomData,
        })
    }
}

// =========================================================================
// sqlx: same "should never run" safety net as M2M<T> / ReverseSet<C>.
// OneToOne fields must be `#[sqlx(skip)]`; these impls exist to
// keep code that accidentally selects a OneToOne column from
// hard-erroring.
// =========================================================================

impl<C: Model> sqlx::Type<sqlx::Sqlite> for OneToOne<C> {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <i64 as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

impl<C: Model> sqlx::Type<sqlx::Postgres> for OneToOne<C> {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i64 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r, C: Model> sqlx::Decode<'r, sqlx::Sqlite> for OneToOne<C> {
    fn decode(
        value: sqlx::sqlite::SqliteValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Real decode for the child-side OneToOne<T> sugar — keep the
        // i64 so .id() works after a select_related-less fetch.
        // Parent-side fields are `#[sqlx(skip)]` and never hit this
        // path, so the upgrade is backwards-compatible.
        let raw = <i64 as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
        Ok(Self::new(raw))
    }
}

impl<'r, C: Model> sqlx::Decode<'r, sqlx::Postgres> for OneToOne<C> {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let raw = <i64 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::new(raw))
    }
}

// Encode — required for child-side OneToOne<T> on INSERT/UPDATE. The
// FK value is in `parent_id`; encode it as `i64` exactly like the FK
// column would. Parent-side OneToOne<C> fields are `#[sqlx(skip)]`
// and never reach the encoder.
impl<'q, C: Model> sqlx::Encode<'q, sqlx::Sqlite> for OneToOne<C> {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let id = self.parent_id.unwrap_or_default();
        <i64 as sqlx::Encode<'q, sqlx::Sqlite>>::encode_by_ref(&id, buf)
    }
}

impl<'q, C: Model> sqlx::Encode<'q, sqlx::Postgres> for OneToOne<C> {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let id = self.parent_id.unwrap_or_default();
        <i64 as sqlx::Encode<'q, sqlx::Postgres>>::encode_by_ref(&id, buf)
    }
}
