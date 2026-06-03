//! `M2M<T>` — many-to-many relations with auto-generated junction tables.
//!
//! A model declares `pub tags: M2M<Tag>` and the framework owns the
//! junction table (`post_tag` with columns `post_id`, `tag_id`). The
//! field has no column on the parent table — `Model::FIELDS` excludes it;
//! the migration engine creates the junction separately.
//!
//! The struct stores a cached parent-row PK (`parent_id`) so accessor
//! methods can apply `WHERE parent_id = ?` without threading an extra
//! argument. It also stores eagerly-loaded related rows when the owning
//! model was fetched with `.prefetch_related("tags")`.
//!
//! ## Usage
//!
//! ```rust,ignore
//! #[derive(Model)]
//! pub struct Post {
//!     pub id: i64,
//!     pub title: String,
//!     pub tags: M2M<Tag>,
//! }
//!
//! // Add a single tag.
//! post.tags.add(&tag).await?;
//!
//! // Replace the entire set.
//! post.tags.set(&[&tag1, &tag2]).await?;
//!
//! // Clear all relations.
//! post.tags.clear().await?;
//!
//! // Lazy fetch (one round-trip).
//! let tags = post.tags.fetch().await?;
//! ```
//!
//! ## Design decisions
//!
//! - `M2M` is generic over `T: Model` but the parent-side PK type is
//!   hardcoded to `i64` at v1 (same simplification as `ForeignKey<T>`).
//!   When the FK generalisation lands, `M2M<T, ParentPk = i64>` follows.
//! - `add` / `remove` / `clear` take `&mut self` so they can update the
//!   `resolved` cache in place, keeping the struct consistent.
//! - `fetch` is async and takes `&self`; `resolved` is synchronous.
//!   Callers opt into the lazy query explicitly.
//! - Junction table naming: `<parent_table>_<field_name>` (e.g.
//!   `post_tags`). The migration engine owns the name, not this struct.
//!
//! ## What is deferred
//!
//! - Reverse accessors (`tag.post_set`). Needs a runtime registry walk.
//! - Through-models with extra columns (Django's `through="...")`.
//! - Cross-database M2M (parent on DB-A, child on DB-B). Rejected at boot.
//! - `prefetch_related` (the QuerySet batch-load that populates `resolved`).
//!   The struct already has the `resolved` slot; the QuerySet plumbing is
//!   the deferred part.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use super::Model;

/// A many-to-many relation field.
///
/// `M2M<T>` stores no SQL column on the parent table. The framework
/// auto-generates a junction table at migration time and exposes these
/// accessor methods at runtime.
#[derive(Debug, Clone)]
pub struct M2M<T: Model> {
    /// Resolved related rows when the parent was loaded with
    /// `.prefetch_related("field_name")`. `None` = not loaded.
    resolved: Option<Vec<T>>,
    /// Cached parent-row PK so accessor methods know which `WHERE`
    /// clause to apply. Set by the `FromRow` path on the owning model.
    parent_id: Option<i64>,
    _phantom: PhantomData<T>,
}

/// `Default` defers to `empty()`. Required by `sqlx::FromRow` derive
/// on parent structs that mark the M2M field with `#[sqlx(skip)]` —
/// the skip path uses `Default::default()` to fill the slot, then
/// `HydrateRelated::set_m2m_parent_ids` seeds the parent_id from
/// the just-decoded row.
impl<T: Model> Default for M2M<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T: Model> M2M<T> {
    /// Create an empty `M2M` with no parent id and no resolved rows.
    pub fn empty() -> Self {
        Self {
            resolved: None,
            parent_id: None,
            _phantom: PhantomData,
        }
    }

    /// Create an `M2M` with a known parent id. Used by the `FromRow`
    /// path: the owning model's PK is captured here.
    pub fn with_parent_id(parent_id: i64) -> Self {
        Self {
            resolved: None,
            parent_id: Some(parent_id),
            _phantom: PhantomData,
        }
    }

    /// Read the cached set when `prefetch_related` populated it.
    pub fn resolved(&self) -> Option<&[T]> {
        self.resolved.as_deref()
    }

    /// Attach eagerly-loaded rows. Called internally by the
    /// `prefetch_related` machinery.
    pub fn set_resolved(&mut self, rows: Vec<T>) {
        self.resolved = Some(rows);
    }

    /// Return the cached parent id, if any.
    pub fn parent_id(&self) -> Option<i64> {
        self.parent_id
    }

    /// Set the parent id. Called by the `FromRow` path when the owning
    /// model is materialised.
    pub fn set_parent_id(&mut self, id: i64) {
        self.parent_id = Some(id);
    }

    // -----------------------------------------------------------------
    // Junction table helpers (private). The table name is derived from
    // the parent's table + field name, but this struct doesn't carry
    // the field name. For v1 these helpers take an explicit table
    // name; the derive macro emits wrapper methods that hard-code it.
    // -----------------------------------------------------------------

    /// Build a `DELETE FROM <junction> WHERE parent_col = ?` statement
    /// and execute it. Returns the number of rows deleted.
    async fn _clear_junction(
        &self,
        pool: &sqlx::SqlitePool,
        junction_table: &str,
        parent_col: &str,
    ) -> Result<u64, sqlx::Error> {
        let sql = format!(
            "DELETE FROM \"{}\" WHERE \"{}\" = ?",
            junction_table, parent_col
        );
        let result = sqlx::query(&sql)
            .bind(self.parent_id.unwrap_or(0))
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// Build an `INSERT OR IGNORE INTO <junction> (parent_col, child_col)
    /// VALUES (?, ?)` and execute it.
    async fn _add_to_junction(
        &self,
        pool: &sqlx::SqlitePool,
        junction_table: &str,
        parent_col: &str,
        child_col: &str,
        child_pk: i64,
    ) -> Result<(), sqlx::Error> {
        let sql = format!(
            "INSERT OR IGNORE INTO \"{}\" (\"{}\", \"{}\") VALUES (?, ?)",
            junction_table, parent_col, child_col
        );
        sqlx::query(&sql)
            .bind(self.parent_id.unwrap_or(0))
            .bind(child_pk)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Build a `DELETE FROM <junction> WHERE parent_col = ? AND child_col = ?`
    /// and execute it.
    async fn _remove_from_junction(
        &self,
        pool: &sqlx::SqlitePool,
        junction_table: &str,
        parent_col: &str,
        child_col: &str,
        child_pk: i64,
    ) -> Result<(), sqlx::Error> {
        let sql = format!(
            "DELETE FROM \"{}\" WHERE \"{}\" = ? AND \"{}\" = ?",
            junction_table, parent_col, child_col
        );
        sqlx::query(&sql)
            .bind(self.parent_id.unwrap_or(0))
            .bind(child_pk)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Lazy fetch: SELECT child.* FROM child INNER JOIN junction ON ...
    /// WHERE junction.parent_col = ?
    ///
    /// This is a convenience method for small N. For large sets use
    /// `prefetch_related` on the QuerySet.
    pub async fn fetch(
        &self,
        junction_table: &str,
        parent_col: &str,
        child_table: &str,
    ) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + Clone,
    {
        let parent_id = match self.parent_id {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };
        let child_pk_col = T::FIELDS
            .iter()
            .find(|f| f.primary_key)
            .map(|f| f.name)
            .unwrap_or("id");
        let columns: Vec<&str> = T::FIELDS.iter().map(|f| f.name).collect();
        let col_list = columns.join(", ");
        let sql = format!(
            "SELECT {} FROM \"{}\" \"c\" \
             INNER JOIN \"{}\" \"j\" ON \"c\".\"{}\" = \"j\".\"child\" \
             WHERE \"j\".\"{}\" = ?",
            col_list, child_table, junction_table, child_pk_col, parent_col
        );
        let pool = crate::db::pool_dispatched();
        match pool {
            crate::db::DbPool::Sqlite(p) => {
                sqlx::query_as::<sqlx::Sqlite, T>(&sql)
                    .bind(parent_id)
                    .fetch_all(p)
                    .await
            }
            crate::db::DbPool::Postgres(p) => {
                let pg_sql = sql.replace("?", "$1");
                sqlx::query_as::<sqlx::Postgres, T>(&pg_sql)
                    .bind(parent_id)
                    .fetch_all(p)
                    .await
            }
        }
    }
}

// =========================================================================
// serde: serialise as an array of child PKs when not resolved, array of
// full objects when resolved.
// =========================================================================

impl<T: Model + Serialize> Serialize for M2M<T>
where
    T::PrimaryKey: Serialize,
{
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if let Some(resolved) = &self.resolved {
            resolved.serialize(s)
        } else {
            // Without resolved data, emit an empty array. This matches
            // Django's M2M serialisation when prefetch_related wasn't
            // called: the field is present but empty.
            let empty: Vec<T> = Vec::new();
            empty.serialize(s)
        }
    }
}

impl<'de, T: Model> Deserialize<'de> for M2M<T>
where
    T::PrimaryKey: Deserialize<'de>,
{
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // M2M fields are not persisted on the parent table, so
        // deserialisation from a row is a no-op. The resolved slot
        // stays empty until `prefetch_related` populates it.
        let _ = Vec::<T::PrimaryKey>::deserialize(d)?;
        Ok(Self::empty())
    }
}

// =========================================================================
// sqlx: encode / decode — M2M fields have no column on the parent table,
// so these impls are technically unreachable from the FromRow path. They
// exist only to satisfy the derive-macro's blanket expectations when it
// emits generic bounds.
// =========================================================================

impl<T: Model> sqlx::Type<sqlx::Sqlite> for M2M<T> {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <i64 as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

impl<T: Model> sqlx::Type<sqlx::Postgres> for M2M<T> {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i64 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r, T: Model> sqlx::Decode<'r, sqlx::Sqlite> for M2M<T> {
    fn decode(
        value: sqlx::sqlite::SqliteValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // M2M fields don't appear in SELECT column lists, so this
        // path is only reached if the user hand-writes a query that
        // includes an M2M column. Decode as a no-op.
        let _ = <i64 as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
        Ok(Self::empty())
    }
}

impl<'r, T: Model> sqlx::Decode<'r, sqlx::Postgres> for M2M<T> {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let _ = <i64 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::empty())
    }
}

impl<'q, T: Model> sqlx::Encode<'q, sqlx::Sqlite> for M2M<T> {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i64 as sqlx::Encode<'q, sqlx::Sqlite>>::encode_by_ref(&0i64, buf)
    }
}

impl<'q, T: Model> sqlx::Encode<'q, sqlx::Postgres> for M2M<T> {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i64 as sqlx::Encode<'q, sqlx::Postgres>>::encode_by_ref(&0i64, buf)
    }
}
