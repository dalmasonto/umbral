//! `ForeignKey<T>` — a typed foreign-key field for umbra models.
//!
//! A `ForeignKey<T>` field stores the primary key of a row in the
//! table owned by model `T`. The PK type is `T::PrimaryKey`, so a FK
//! to a model with `id: i64` stores an `i64`; a FK to a model with
//! `codename: String` as PK stores a `String`; a FK to a model with
//! `id: Uuid` stores a `Uuid`. Without eager loading the FK
//! serialises transparently as the PK's native JSON shape (number for
//! `i64`, string for `String`/`Uuid`) so the REST layer, backup, and
//! JSON round-trips all see the natural value. When `select_related`
//! has populated the `resolved` slot, serialisation emits the full
//! `T` object instead — this is what makes
//! `{{ post.author.first_name }}` work in templates after
//! `select_related("author")`.
//!
//! ## Behaviour summary
//!
//! | `resolved` | `serde::Serialize` output                  | `.id()`        |
//! |------------|--------------------------------------------|----------------|
//! | `None`     | the raw PK (number / string / uuid)        | the raw PK     |
//! | `Some(u)`  | `{ ...full T fields... }`                  | the raw PK     |
//!
//! The backward-compat rule for the i64-keyed common case: code that
//! doesn't call `select_related` sees the same integer serialisation
//! it did before. Callers that need the full object opt in
//! explicitly.
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
//! // Lazy: only stores the PK.
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
//! ```
//!
//! ## What is deferred
//!
//! Many-to-many relationships, reverse accessors (`User::posts`), `ON DELETE`
//! beyond RESTRICT — all deferred. See `docs/specs/relationships.md`.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use super::Model;

/// A foreign-key field that stores a `T::PrimaryKey` reference to a
/// row in the table owned by model `T`.
///
/// The PK type comes from the target model's `Model::PrimaryKey`
/// associated type, so a single `ForeignKey<T>` definition works for
/// integer-keyed, string-keyed, and UUID-keyed targets without
/// further user code.
#[derive(Debug, Clone)]
pub struct ForeignKey<T: Model> {
    /// The raw primary-key value stored in the database column.
    raw: T::PrimaryKey,
    /// Optional eagerly-loaded referenced row. Populated by `select_related`.
    /// Boxed so the FK field doesn't bloat the model struct when `resolved`
    /// is `None` (the common case).
    resolved: Option<Box<T>>,
    _phantom: PhantomData<T>,
}

impl<T: Model> PartialEq for ForeignKey<T>
where
    T::PrimaryKey: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T: Model> Eq for ForeignKey<T> where T::PrimaryKey: Eq {}

impl<T: Model> ForeignKey<T> {
    /// Create a new FK value wrapping the given raw primary-key value.
    pub fn new(raw: T::PrimaryKey) -> Self {
        Self {
            raw,
            resolved: None,
            _phantom: PhantomData,
        }
    }

    /// Return the raw primary-key value (cloned — the PK is owned).
    pub fn id(&self) -> T::PrimaryKey {
        self.raw.clone()
    }

    /// Borrow the raw primary-key value without cloning. Useful for
    /// passing into query predicates that take `&T::PrimaryKey`.
    pub fn id_ref(&self) -> &T::PrimaryKey {
        &self.raw
    }

    /// Replace the stored primary-key value.
    pub fn set(&mut self, raw: T::PrimaryKey) {
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
}

impl<T: Model> ForeignKey<T> {
    /// SQL name of the target model's primary-key column. Looks up the
    /// `primary_key = true` entry in `T::FIELDS`. Falls back to `"id"`
    /// if no field is marked PK (shouldn't happen for derive-generated
    /// models, but the fallback keeps the code defensive).
    fn pk_column_name() -> &'static str {
        T::FIELDS
            .iter()
            .find(|f| f.primary_key)
            .map(|f| f.name)
            .unwrap_or("id")
    }
}

impl<T: Model> ForeignKey<T>
where
    T::PrimaryKey: for<'q> sqlx::Encode<'q, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    /// Fetch the referenced row from the database.
    ///
    /// If `resolved` is already populated (via `select_related`), returns a
    /// clone of the cached row without a database round-trip. Otherwise runs
    /// `SELECT * FROM <T::TABLE> WHERE <pk_col> = ? LIMIT 1`.
    pub async fn resolve(&self, pool: &sqlx::SqlitePool) -> Result<T, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Clone,
    {
        if let Some(cached) = &self.resolved {
            return Ok(*cached.clone());
        }
        let columns: Vec<&str> = T::FIELDS.iter().map(|f| f.name).collect();
        let col_list = columns.join(", ");
        let pk_col = Self::pk_column_name();
        let sql = format!(
            "SELECT {} FROM {} WHERE {} = ? LIMIT 1",
            col_list,
            T::TABLE,
            pk_col
        );
        sqlx::query_as::<sqlx::Sqlite, T>(&sql)
            .bind(self.raw.clone())
            .fetch_one(pool)
            .await
    }
}

impl<T: Model> ForeignKey<T>
where
    T::PrimaryKey: for<'q> sqlx::Encode<'q, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    /// Fetch the referenced row from a Postgres pool.
    pub async fn resolve_pg(&self, pool: &sqlx::PgPool) -> Result<T, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Clone,
    {
        if let Some(cached) = &self.resolved {
            return Ok(*cached.clone());
        }
        let columns: Vec<&str> = T::FIELDS.iter().map(|f| f.name).collect();
        let col_list = columns.join(", ");
        let pk_col = Self::pk_column_name();
        let sql = format!(
            "SELECT {} FROM {} WHERE {} = $1 LIMIT 1",
            col_list,
            T::TABLE,
            pk_col
        );
        sqlx::query_as::<sqlx::Postgres, T>(&sql)
            .bind(self.raw.clone())
            .fetch_one(pool)
            .await
    }
}

// =========================================================================
// serde: PK serialisation by default; full T when resolved.
//
// The two behaviours are:
//
// - `resolved = None`: serialise as `T::PrimaryKey`. For an i64-keyed
//   target that's a bare integer (matches the pre-generalisation
//   shape); for a string-keyed target like `Permission` it's a
//   string.
// - `resolved = Some(row)`: serialise as the full `T` object so
//   template `{{ post.author.username }}` and
//   `ctx["author"]["username"]` both work after `select_related`.
//
// Deserialisation reads `T::PrimaryKey` directly. There is no round-
// trip symmetry when `resolved` is `Some` — the serialised form is
// the full object, but loading it back reads only the PK. This is
// intentional: the resolved slot is a runtime annotation produced
// by `select_related`, not a persisted field.
// =========================================================================

impl<T: Model + Serialize> Serialize for ForeignKey<T>
where
    T::PrimaryKey: Serialize,
{
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if let Some(resolved) = &self.resolved {
            resolved.serialize(s)
        } else {
            self.raw.serialize(s)
        }
    }
}

impl<'de, T: Model + serde::de::DeserializeOwned> Deserialize<'de> for ForeignKey<T>
where
    T::PrimaryKey: serde::de::DeserializeOwned,
{
    /// Accepts BOTH shapes:
    /// - a scalar (number / string) → the FK's raw PK value, with
    ///   `resolved: None`. The pre-#42 shape; preserves backward
    ///   compatibility with every `serde_json::from_str` site that
    ///   reads an unresolved FK from JSON.
    /// - a JSON object → parse as `T`, extract the PK out of the
    ///   object, store both. This is what makes the nested
    ///   `select_related("author__manager")` traversal work: the
    ///   hydration path builds a nested object with the manager
    ///   already embedded inside author, calls
    ///   `parent.hydrate_fk("author", nested_json)`, and recursive
    ///   Deserialize through this impl populates the chain end-to-end.
    ///   Side benefit: a select_related'd model now round-trips
    ///   through `serde_json::to_value(&t)` / `from_value` without
    ///   losing the resolved relation.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        match v {
            serde_json::Value::Object(_) => {
                // PK field name comes from the Model's FIELDS table.
                // Falls back to "id" if no primary_key flag is set
                // (every umbra model has one by derive contract; the
                // fallback is just defensive).
                let pk_name = T::FIELDS
                    .iter()
                    .find(|f| f.primary_key)
                    .map(|f| f.name)
                    .unwrap_or("id");
                let pk_v = v.get(pk_name).cloned().ok_or_else(|| {
                    D::Error::custom(format!(
                        "ForeignKey<{}>: nested object missing pk field `{pk_name}`",
                        T::NAME
                    ))
                })?;
                let raw: T::PrimaryKey = serde_json::from_value(pk_v).map_err(D::Error::custom)?;
                let resolved: T = serde_json::from_value(v).map_err(D::Error::custom)?;
                Ok(Self {
                    raw,
                    resolved: Some(Box::new(resolved)),
                    _phantom: PhantomData,
                })
            }
            other => {
                let raw: T::PrimaryKey = serde_json::from_value(other).map_err(D::Error::custom)?;
                Ok(Self::new(raw))
            }
        }
    }
}

// =========================================================================
// sqlx: encode / decode delegate to T::PrimaryKey.
//
// The `FromRow` derive on user structs calls `decode` on each column.
// By delegating `Type`, `Encode`, `Decode` to the PK type, a
// `ForeignKey<T>` column round-trips through the database with no
// special-case logic in the QuerySet or the write path. The
// `resolved` slot is not involved — the DB only ever sees the raw PK.
// =========================================================================

impl<T: Model> sqlx::Type<sqlx::Sqlite> for ForeignKey<T>
where
    T::PrimaryKey: sqlx::Type<sqlx::Sqlite>,
{
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <T::PrimaryKey as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <T::PrimaryKey as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

impl<T: Model> sqlx::Type<sqlx::Postgres> for ForeignKey<T>
where
    T::PrimaryKey: sqlx::Type<sqlx::Postgres>,
{
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <T::PrimaryKey as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <T::PrimaryKey as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r, T: Model> sqlx::Decode<'r, sqlx::Sqlite> for ForeignKey<T>
where
    T::PrimaryKey: sqlx::Decode<'r, sqlx::Sqlite>,
{
    fn decode(
        value: sqlx::sqlite::SqliteValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let raw = <T::PrimaryKey as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
        Ok(Self::new(raw))
    }
}

impl<'r, T: Model> sqlx::Decode<'r, sqlx::Postgres> for ForeignKey<T>
where
    T::PrimaryKey: sqlx::Decode<'r, sqlx::Postgres>,
{
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let raw = <T::PrimaryKey as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::new(raw))
    }
}

impl<'q, T: Model> sqlx::Encode<'q, sqlx::Sqlite> for ForeignKey<T>
where
    T::PrimaryKey: sqlx::Encode<'q, sqlx::Sqlite> + Clone,
{
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <T::PrimaryKey as sqlx::Encode<'q, sqlx::Sqlite>>::encode_by_ref(&self.raw, buf)
    }
}

impl<'q, T: Model> sqlx::Encode<'q, sqlx::Postgres> for ForeignKey<T>
where
    T::PrimaryKey: sqlx::Encode<'q, sqlx::Postgres> + Clone,
{
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <T::PrimaryKey as sqlx::Encode<'q, sqlx::Postgres>>::encode_by_ref(&self.raw, buf)
    }
}
