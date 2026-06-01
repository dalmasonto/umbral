//! `ForeignKey<T>` — a typed foreign-key field for umbra models.
//!
//! A `ForeignKey<T>` field stores the `i64` primary key of a row in the
//! table owned by model `T`. Without eager loading it serialises transparently
//! as `i64` so the REST layer, backup, and JSON round-trips all see a plain
//! integer. When `select_related` has populated the `resolved` slot,
//! serialisation emits the full `T` object instead — this is what makes
//! `{{ post.author.first_name }}` work in templates after
//! `select_related("author")`.
//!
//! ## Behaviour summary
//!
//! | `resolved` | `serde::Serialize` output | `.id()` | `.resolved()` |
//! |------------|--------------------------|---------|---------------|
//! | `None`     | `42` (bare integer)       | `42`    | `None`        |
//! | `Some(u)`  | `{"id":42,"name":"Alice"}` | `42`   | `Some(&u)`    |
//!
//! The backward-compat rule: code that doesn't call `select_related` sees
//! the same integer serialisation as before gap 14. Callers that need the
//! full object opt in explicitly.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use umbra::orm::{ForeignKey, Model};
//!
//! #[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
//! pub struct User {
//!     pub id: i64,
//!     pub name: String,
//! }
//!
//! #[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
//! pub struct Post {
//!     pub id: i64,
//!     pub title: String,
//!     pub author: ForeignKey<User>,
//! }
//!
//! // Lazy: only stores the integer.
//! let post = Post::objects().filter(post::ID.eq(1)).get().await?;
//! assert_eq!(post.author.id(), 7);
//! assert!(post.author.resolved().is_none());
//!
//! // Eager: resolved slot is populated by the JOIN.
//! let post = Post::objects()
//!     .filter(post::ID.eq(1))
//!     .select_related("author")
//!     .get()
//!     .await?;
//! assert_eq!(post.author.resolved().unwrap().name, "Alice");
//!
//! // Template context: ctx["author"]["name"] == "Alice"
//! let ctx = serde_json::to_value(&post)?;
//! ```
//!
//! ## What is deferred
//!
//! Many-to-many relationships, reverse accessors (`User::posts`), `ON DELETE`
//! beyond RESTRICT, and FK columns with non-`i64` targets are all deferred.
//! See `docs/specs/relationships.md`.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use super::Model;

/// A foreign-key field that stores an `i64` reference to the primary key of
/// model `T`.
///
/// When `resolved` is `None` (the common case without `select_related`),
/// this type serialises and deserialises transparently as `i64` — the REST
/// layer, backup, and JSON round-trips all see a plain integer.
///
/// When `select_related` populates `resolved`, `Serialize` emits the full `T`
/// value instead, enabling `{{ post.author.first_name }}` in templates and
/// `ctx["author"]["name"]` in Rust code that uses `serde_json::to_value`.
///
/// The `sqlx::Encode` / `sqlx::Decode` impls remain bound to `i64` regardless
/// of the `resolved` slot: the database stores only the integer PK, never the
/// nested object.
#[derive(Debug, Clone)]
pub struct ForeignKey<T: Model> {
    /// The raw i64 primary-key value stored in the database column.
    raw: i64,
    /// Optional eagerly-loaded referenced row. Populated by `select_related`.
    /// Boxed so the FK field doesn't bloat the model struct when `resolved`
    /// is `None` (the common case).
    resolved: Option<Box<T>>,
    _phantom: PhantomData<T>,
}

impl<T: Model + PartialEq> PartialEq for ForeignKey<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T: Model + PartialEq + Eq> Eq for ForeignKey<T> {}

impl<T: Model> ForeignKey<T> {
    /// Create a new FK value wrapping the given raw primary-key integer.
    pub fn new(raw: i64) -> Self {
        Self {
            raw,
            resolved: None,
            _phantom: PhantomData,
        }
    }

    /// Return the raw primary-key value.
    pub fn id(&self) -> i64 {
        self.raw
    }

    /// Replace the stored primary-key value.
    pub fn set(&mut self, raw: i64) {
        self.raw = raw;
    }

    /// Return a reference to the eagerly-loaded model row, if any.
    ///
    /// `None` means `select_related` was not called (or was called but this
    /// FK field was not named). `Some(&T)` means the JOIN was executed and
    /// the full row is available without a round-trip.
    pub fn resolved(&self) -> Option<&T> {
        self.resolved.as_deref()
    }

    /// Attach an already-fetched model row to this FK.
    ///
    /// Called internally by the `select_related` machinery in `QuerySet`
    /// after the JOIN rows are split and hydrated. Not intended for direct
    /// user call sites, but `pub` so the ORM layer (different module) can
    /// reach it.
    pub fn set_resolved(&mut self, row: T) {
        self.resolved = Some(Box::new(row));
    }

    /// Fetch the referenced row from the database.
    ///
    /// If `resolved` is already populated (via `select_related`), returns a
    /// clone of the cached row without a database round-trip. Otherwise runs
    /// `SELECT * FROM <T::TABLE> WHERE id = ? LIMIT 1`.
    pub async fn resolve(&self, pool: &sqlx::SqlitePool) -> Result<T, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Clone,
    {
        if let Some(cached) = &self.resolved {
            return Ok(*cached.clone());
        }
        let columns: Vec<&str> = T::FIELDS.iter().map(|f| f.name).collect();
        let col_list = columns.join(", ");
        let sql = format!("SELECT {} FROM {} WHERE id = ? LIMIT 1", col_list, T::TABLE);
        sqlx::query_as::<sqlx::Sqlite, T>(&sql)
            .bind(self.raw)
            .fetch_one(pool)
            .await
    }

    /// Fetch the referenced row from a Postgres pool.
    ///
    /// Postgres counterpart of [`Self::resolve`]. Returns a clone of the
    /// cached `resolved` row when available.
    pub async fn resolve_pg(&self, pool: &sqlx::PgPool) -> Result<T, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Clone,
    {
        if let Some(cached) = &self.resolved {
            return Ok(*cached.clone());
        }
        let columns: Vec<&str> = T::FIELDS.iter().map(|f| f.name).collect();
        let col_list = columns.join(", ");
        let sql = format!(
            "SELECT {} FROM {} WHERE id = $1 LIMIT 1",
            col_list,
            T::TABLE
        );
        sqlx::query_as::<sqlx::Postgres, T>(&sql)
            .bind(self.raw)
            .fetch_one(pool)
            .await
    }
}

impl<T: Model> From<i64> for ForeignKey<T> {
    fn from(raw: i64) -> Self {
        Self::new(raw)
    }
}

impl<T: Model> From<ForeignKey<T>> for i64 {
    fn from(fk: ForeignKey<T>) -> i64 {
        fk.raw
    }
}

// =========================================================================
// serde: transparent i64 serialisation by default; full T when resolved.
//
// The two behaviours are:
//
// - `resolved = None`: serialise as `i64` exactly as before gap 14.
//   Backward-compatible; the REST layer, backup, and template contexts all
//   continue to see a plain integer.
//
// - `resolved = Some(row)`: serialise as the full `T` object so template
//   `{{ post.author.username }}` and `ctx["author"]["username"]` both work
//   after `select_related`.
//
// Deserialisation always reads from an `i64`. There is no round-trip
// symmetry when `resolved` is `Some` — the serialised form is the full
// object, but loading it back reads only the integer PK. This is
// intentional: the resolved slot is a runtime annotation produced by
// `select_related`, not a persisted field.
// =========================================================================

impl<T: Model + Serialize> Serialize for ForeignKey<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if let Some(resolved) = &self.resolved {
            resolved.serialize(s)
        } else {
            self.raw.serialize(s)
        }
    }
}

impl<'de, T: Model> Deserialize<'de> for ForeignKey<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = i64::deserialize(d)?;
        Ok(Self::new(raw))
    }
}

// =========================================================================
// sqlx: encode / decode as i64.
//
// The `FromRow` derive on user structs calls `decode` on each column.
// By implementing `sqlx::Type`, `Encode`, and `Decode` as thin wrappers
// around `i64`, a `ForeignKey<T>` column round-trips through the database
// with no special-case logic in the QuerySet or the write path.
// The `resolved` slot is not involved — the DB only ever sees the integer.
// =========================================================================

impl<T: Model> sqlx::Type<sqlx::Sqlite> for ForeignKey<T> {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <i64 as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

impl<T: Model> sqlx::Type<sqlx::Postgres> for ForeignKey<T> {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i64 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r, T: Model> sqlx::Decode<'r, sqlx::Sqlite> for ForeignKey<T> {
    fn decode(
        value: sqlx::sqlite::SqliteValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let raw = <i64 as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
        Ok(Self::new(raw))
    }
}

impl<'r, T: Model> sqlx::Decode<'r, sqlx::Postgres> for ForeignKey<T> {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let raw = <i64 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::new(raw))
    }
}

impl<'q, T: Model> sqlx::Encode<'q, sqlx::Sqlite> for ForeignKey<T> {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i64 as sqlx::Encode<'q, sqlx::Sqlite>>::encode_by_ref(&self.raw, buf)
    }
}

impl<'q, T: Model> sqlx::Encode<'q, sqlx::Postgres> for ForeignKey<T> {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i64 as sqlx::Encode<'q, sqlx::Postgres>>::encode_by_ref(&self.raw, buf)
    }
}
