//! `ForeignKey<T>` — a typed foreign-key field for umbra models.
//!
//! A `ForeignKey<T>` field stores the `i64` primary key of a row in the
//! table owned by model `T`. It serialises and deserialises transparently
//! as `i64` so the REST layer, backup, and JSON round-trips all see a plain
//! integer. The extra `.id()` / `.set()` accessors and the async `.resolve()`
//! helper give callers a typed surface on top.
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
//! ```
//!
//! ## What the migration engine emits
//!
//! For an `i64` FK field the `CREATE TABLE` DDL includes a `REFERENCES` clause:
//!
//! ```sql
//! -- SQLite
//! CREATE TABLE "post" (
//!   "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
//!   "title" text NOT NULL,
//!   "author" bigint NOT NULL REFERENCES "user"("id")
//! )
//!
//! -- Postgres
//! CREATE TABLE "post" (
//!   "id" bigserial PRIMARY KEY,
//!   "title" text NOT NULL,
//!   "author" bigint NOT NULL REFERENCES "user"("id")
//! )
//! ```
//!
//! ## What's deferred
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
/// Transparently serialises / deserialises as `i64` (same as the raw integer)
/// so the REST and backup layers don't need special handling. The `sqlx`
/// `FromRow` derive picks it up via the `ForeignKey::from(i64)` impl (the
/// derive calls `i64::decode` then wraps through `From`).
///
/// The type parameter `T: Model` is phantom — it carries the referenced model
/// type at the Rust level so `.resolve()` knows which table to query and the
/// column constant in the sibling module has the right `ForeignKeyCol<T>`
/// type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey<T: Model> {
    raw: i64,
    _phantom: PhantomData<T>,
}

impl<T: Model> ForeignKey<T> {
    /// Create a new FK value wrapping the given raw primary-key integer.
    pub fn new(raw: i64) -> Self {
        Self {
            raw,
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

    /// Fetch the referenced row from the database.
    ///
    /// Runs `SELECT * FROM <T::TABLE> WHERE id = $1 LIMIT 1`.
    /// Returns `Ok(row)` when exactly one matching row exists. Maps
    /// directly to a sqlx error when the query fails; no `GetError`
    /// wrapping — this is a point-lookup, so `RowNotFound` is the only
    /// distinguished case callers usually branch on.
    pub async fn resolve(&self, pool: &sqlx::SqlitePool) -> Result<T, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>,
    {
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
    /// Postgres counterpart of [`Self::resolve`]. Uses `$1` placeholder
    /// syntax via sqlx's Postgres query builder.
    pub async fn resolve_pg(&self, pool: &sqlx::PgPool) -> Result<T, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
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
// serde: transparent i64 serialisation.
//
// `ForeignKey<T>` serialises as a plain JSON integer, matching how every
// other ORM with a FK column behaves — the REST layer sees numbers, not
// objects. The `Serialize` / `Deserialize` impls delegate to `i64`
// directly so the round-trip is lossless and unambiguous.
// =========================================================================

impl<T: Model> Serialize for ForeignKey<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.raw.serialize(s)
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
