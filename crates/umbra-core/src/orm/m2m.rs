//! `M2M<T, P = i64>` — many-to-many relations with auto-generated
//! junction tables and any-PK support.
//!
//! A model declares `pub tags: M2M<Tag>` and the framework owns the
//! junction table (`<parent_table>_<field_name>`, e.g. `post_tags`)
//! with `parent_id` and `child_id` columns. The field has no column on
//! the parent table — `Model::FIELDS` excludes it; the migration engine
//! creates the junction separately via [`crate::migrate::Operation::CreateM2MTable`].
//!
//! ## Type parameters
//!
//! - `T` — the child model the relation points at.
//! - `P` — the parent model's primary-key type. Defaults to `i64`.
//!   Override when the parent's PK isn't `i64`:
//!   `pub tags: M2M<Tag, String>` on a model whose PK is a `String`
//!   slug. The bound is [`super::PrimaryKey`], which carries
//!   `Into<sea_query::Value>` so the junction CRUD can bind it on
//!   any backend.
//!
//! The child's PK type comes from `T::PrimaryKey` and is bound the
//! same way; both backends store the right column widths.
//!
//! ## Public CRUD
//!
//! ```rust,ignore
//! // Lazy fetch (one round-trip through the junction).
//! let tags = post.tags.fetch().await?;
//!
//! // Add a single tag.
//! post.tags.add(&tag).await?;
//!
//! // Remove a single tag.
//! post.tags.remove(&tag).await?;
//!
//! // Replace the entire set.
//! post.tags.set(&[&tag1, &tag2]).await?;
//!
//! // Clear every relation for this parent.
//! post.tags.clear().await?;
//! ```
//!
//! Every CRUD method routes through [`crate::db::pool_dispatched`]
//! and uses sea-query — no per-backend SQL strings, no hardcoded
//! placeholder syntax. The methods are no-ops when `parent_id` is
//! unset (the field was constructed via `M2M::empty()` outside the
//! `FromRow` path, e.g. before the parent row was saved).
//!
//! ## How junction metadata reaches the struct
//!
//! The struct doesn't know its junction table name at construction
//! time — sqlx's `FromRow` decoder doesn't know which field on the
//! parent it's filling. The `Model` derive emits a
//! `HydrateRelated::set_m2m_parent_ids` body that calls
//! [`Self::set_junction_table`] alongside [`Self::set_parent_id`] for
//! every M2M field, and the QuerySet terminals invoke it on every
//! materialised row. Plain-`Default` instances get the table name
//! filled in this same way.
//!
//! ## What is deferred
//!
//! - **Reverse accessors** (`tag.post_set`). Needs a runtime registry walk.
//! - **`through=` models** (Django's M2M with extra fields on the
//!   junction). The current shape only covers the implicit join table.
//! - **Cross-database M2M** (parent on DB-A, child on DB-B). Rejected at boot.
//! - **`prefetch_related` (batch-load)** — the QuerySet plumbing that
//!   populates `resolved` is the deferred part. The slot is already there.

use std::marker::PhantomData;

use sea_query::{Alias, Expr, OnConflict, PostgresQueryBuilder, Query, SqliteQueryBuilder};
use sea_query_binder::SqlxBinder;
use serde::{Deserialize, Serialize};

use super::{Model, PrimaryKey};

/// A many-to-many relation field.
///
/// `M2M<T, P>` stores no SQL column on the parent table. The framework
/// auto-generates a junction table at migration time and exposes the
/// accessor methods at runtime.
#[derive(Debug, Clone)]
pub struct M2M<T: Model, P: PrimaryKey = i64> {
    /// Resolved related rows when the parent was loaded with
    /// `.prefetch_related("field_name")`. `None` = not loaded.
    resolved: Option<Vec<T>>,
    /// Cached parent-row PK so accessor methods know which `WHERE`
    /// clause to apply. Set by the `set_m2m_parent_ids` hook on the
    /// owning model.
    parent_id: Option<P>,
    /// Junction table name. Set by the macro at hydrate time alongside
    /// `parent_id`. Without this the CRUD methods can't build any SQL
    /// and return `Ok(())` / `Ok(Vec::new())` — same shape as a row
    /// with no parent id.
    junction_table: Option<&'static str>,
    _phantom: PhantomData<T>,
}

/// `Default` defers to `empty()`. Required by `sqlx::FromRow` derive
/// on parent structs that mark the M2M field with `#[sqlx(skip)]` —
/// the skip path uses `Default::default()` to fill the slot, then
/// `HydrateRelated::set_m2m_parent_ids` seeds parent_id +
/// junction_table from the just-decoded row.
impl<T: Model, P: PrimaryKey> Default for M2M<T, P> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T: Model, P: PrimaryKey> M2M<T, P> {
    /// Create an empty `M2M` with no parent id, no junction metadata,
    /// and no resolved rows.
    pub fn empty() -> Self {
        Self {
            resolved: None,
            parent_id: None,
            junction_table: None,
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

    /// Return a reference to the cached parent id, if any.
    pub fn parent_id(&self) -> Option<&P> {
        self.parent_id.as_ref()
    }

    /// Set the parent id. Called by the `set_m2m_parent_ids` macro
    /// body when the owning model is materialised.
    pub fn set_parent_id(&mut self, id: P) {
        self.parent_id = Some(id);
    }

    /// Set the junction table name. Called by the macro alongside
    /// [`Self::set_parent_id`] using the deterministic
    /// `<parent_table>_<field_name>` convention.
    pub fn set_junction_table(&mut self, name: &'static str) {
        self.junction_table = Some(name);
    }

    /// Return the junction table name once set, or `None` for an
    /// unattached `M2M::empty()`.
    pub fn junction_table(&self) -> Option<&'static str> {
        self.junction_table
    }

    /// `SELECT child.* FROM <child_table> child INNER JOIN
    /// <junction> j ON child.<pk> = j.child_id WHERE j.parent_id = ?`.
    ///
    /// Returns `Ok(Vec::new())` when the M2M slot is unattached
    /// (parent never persisted, or junction metadata never seeded —
    /// same shape as "no rows match"). For large fan-outs prefer
    /// `prefetch_related` on the parent QuerySet.
    pub async fn fetch(&self) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let Some((parent_id, junction)) = self.junction_handle() else {
            return Ok(Vec::new());
        };
        let child_pk = child_pk_col::<T>();
        let mut q = Query::select();
        q.columns(
            T::FIELDS
                .iter()
                .map(|f| (Alias::new("c"), Alias::new(f.name))),
        )
        .from_as(Alias::new(T::TABLE), Alias::new("c"))
        .join_as(
            sea_query::JoinType::InnerJoin,
            Alias::new(junction),
            Alias::new("j"),
            Expr::col((Alias::new("j"), Alias::new("child_id")))
                .equals((Alias::new("c"), Alias::new(child_pk))),
        )
        .and_where(Expr::col((Alias::new("j"), Alias::new("parent_id"))).eq(parent_id));

        let pool = crate::db::pool_dispatched();
        match pool {
            crate::db::DbPool::Sqlite(p) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_all(p)
                    .await
            }
            crate::db::DbPool::Postgres(p) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_all(p)
                    .await
            }
        }
    }

    /// Insert one junction row linking this parent to `child`. Idempotent
    /// — duplicate `(parent_id, child_id)` inserts succeed silently
    /// (`ON CONFLICT DO NOTHING` on both backends). Returns `Ok(())`
    /// when the M2M slot is unattached.
    pub async fn add(&self, child: &T) -> Result<(), sqlx::Error> {
        let Some((parent_id, junction)) = self.junction_handle() else {
            return Ok(());
        };
        let child_pk: T::PrimaryKey = child.primary_key();
        let mut q = Query::insert();
        q.into_table(Alias::new(junction))
            .columns([Alias::new("parent_id"), Alias::new("child_id")])
            .values_panic([Expr::value(parent_id).into(), Expr::value(child_pk).into()])
            .on_conflict(
                OnConflict::columns([Alias::new("parent_id"), Alias::new("child_id")])
                    .do_nothing()
                    .to_owned(),
            );
        execute_sql(&q).await
    }

    /// Delete the junction row linking this parent to `child`. No-op
    /// when the relation doesn't exist or the M2M slot is unattached.
    pub async fn remove(&self, child: &T) -> Result<(), sqlx::Error> {
        let Some((parent_id, junction)) = self.junction_handle() else {
            return Ok(());
        };
        let child_pk: T::PrimaryKey = child.primary_key();
        let mut q = Query::delete();
        q.from_table(Alias::new(junction))
            .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id))
            .and_where(Expr::col(Alias::new("child_id")).eq(child_pk));
        execute_delete(&q).await.map(|_| ())
    }

    /// Delete every junction row for this parent. No-op when the
    /// M2M slot is unattached. Returns the number of rows removed.
    pub async fn clear(&self) -> Result<u64, sqlx::Error> {
        let Some((parent_id, junction)) = self.junction_handle() else {
            return Ok(0);
        };
        let mut q = Query::delete();
        q.from_table(Alias::new(junction))
            .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id));
        execute_delete(&q).await
    }

    /// Replace the entire set of relations for this parent with
    /// exactly the supplied children. Equivalent to `clear()` followed
    /// by `add()` for each entry; both run in the same transaction so
    /// a partial replacement can't leak. No-op when the M2M slot is
    /// unattached. The order children are added is not significant —
    /// the junction is a set.
    pub async fn set(&self, children: &[&T]) -> Result<(), sqlx::Error> {
        let Some((parent_id, junction)) = self.junction_handle() else {
            return Ok(());
        };
        let pool = crate::db::pool_dispatched();
        match pool {
            crate::db::DbPool::Sqlite(p) => {
                let mut tx = p.begin().await?;
                let mut delete = Query::delete();
                delete
                    .from_table(Alias::new(junction))
                    .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id.clone()));
                let (sql, values) = delete.build_sqlx(SqliteQueryBuilder);
                sqlx::query_with(&sql, values).execute(&mut *tx).await?;
                for child in children {
                    let child_pk: T::PrimaryKey = child.primary_key();
                    let mut insert = Query::insert();
                    insert
                        .into_table(Alias::new(junction))
                        .columns([Alias::new("parent_id"), Alias::new("child_id")])
                        .values_panic([
                            Expr::value(parent_id.clone()).into(),
                            Expr::value(child_pk).into(),
                        ])
                        .on_conflict(
                            OnConflict::columns([Alias::new("parent_id"), Alias::new("child_id")])
                                .do_nothing()
                                .to_owned(),
                        );
                    let (sql, values) = insert.build_sqlx(SqliteQueryBuilder);
                    sqlx::query_with(&sql, values).execute(&mut *tx).await?;
                }
                tx.commit().await?;
            }
            crate::db::DbPool::Postgres(p) => {
                let mut tx = p.begin().await?;
                let mut delete = Query::delete();
                delete
                    .from_table(Alias::new(junction))
                    .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id.clone()));
                let (sql, values) = delete.build_sqlx(PostgresQueryBuilder);
                sqlx::query_with(&sql, values).execute(&mut *tx).await?;
                for child in children {
                    let child_pk: T::PrimaryKey = child.primary_key();
                    let mut insert = Query::insert();
                    insert
                        .into_table(Alias::new(junction))
                        .columns([Alias::new("parent_id"), Alias::new("child_id")])
                        .values_panic([
                            Expr::value(parent_id.clone()).into(),
                            Expr::value(child_pk).into(),
                        ])
                        .on_conflict(
                            OnConflict::columns([Alias::new("parent_id"), Alias::new("child_id")])
                                .do_nothing()
                                .to_owned(),
                        );
                    let (sql, values) = insert.build_sqlx(PostgresQueryBuilder);
                    sqlx::query_with(&sql, values).execute(&mut *tx).await?;
                }
                tx.commit().await?;
            }
        }
        Ok(())
    }

    /// `(parent_id, junction_table)` shorthand. Returns `None` when
    /// either side is unset — the public CRUD treats that as "nothing
    /// to do" rather than an error, matching Django's behaviour on
    /// unsaved parent instances.
    fn junction_handle(&self) -> Option<(P, &'static str)> {
        Some((self.parent_id.clone()?, self.junction_table?))
    }
}

/// Resolve the child model's PK column name from `T::FIELDS`.
/// Defaults to `"id"` if the model somehow has no PK column (the
/// macro guarantees one in practice).
fn child_pk_col<T: Model>() -> &'static str {
    T::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.name)
        .unwrap_or("id")
}

/// Build and execute an INSERT against the ambient dispatched pool.
async fn execute_sql(q: &sea_query::InsertStatement) -> Result<(), sqlx::Error> {
    let pool = crate::db::pool_dispatched();
    match pool {
        crate::db::DbPool::Sqlite(p) => {
            let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
            sqlx::query_with(&sql, values).execute(p).await?;
        }
        crate::db::DbPool::Postgres(p) => {
            let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
            sqlx::query_with(&sql, values).execute(p).await?;
        }
    }
    Ok(())
}

/// Build and execute a DELETE against the ambient dispatched pool,
/// returning the affected row count.
async fn execute_delete(q: &sea_query::DeleteStatement) -> Result<u64, sqlx::Error> {
    let pool = crate::db::pool_dispatched();
    let n = match pool {
        crate::db::DbPool::Sqlite(p) => {
            let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
            sqlx::query_with(&sql, values)
                .execute(p)
                .await?
                .rows_affected()
        }
        crate::db::DbPool::Postgres(p) => {
            let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
            sqlx::query_with(&sql, values)
                .execute(p)
                .await?
                .rows_affected()
        }
    };
    Ok(n)
}

// =========================================================================
// serde: serialise as an array of resolved rows when prefetch_related
// fired; empty array otherwise. Mirrors Django's M2M serialisation.
// =========================================================================

impl<T: Model + Serialize, P: PrimaryKey> Serialize for M2M<T, P> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if let Some(resolved) = &self.resolved {
            resolved.serialize(s)
        } else {
            let empty: Vec<T> = Vec::new();
            empty.serialize(s)
        }
    }
}

impl<'de, T: Model, P: PrimaryKey> Deserialize<'de> for M2M<T, P> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // M2M fields are not persisted on the parent table, so
        // deserialisation from a row is a no-op. The resolved slot
        // stays empty until `prefetch_related` populates it.
        let _ = serde_json::Value::deserialize(d)?;
        Ok(Self::empty())
    }
}

// =========================================================================
// sqlx: encode / decode — M2M fields have no column on the parent table,
// so these impls are unreachable from the FromRow path when the field
// is correctly marked `#[sqlx(skip)]`. They exist only as a safety net
// for hand-written queries that accidentally select an M2M column.
// =========================================================================

impl<T: Model, P: PrimaryKey> sqlx::Type<sqlx::Sqlite> for M2M<T, P> {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <i64 as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

impl<T: Model, P: PrimaryKey> sqlx::Type<sqlx::Postgres> for M2M<T, P> {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i64 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r, T: Model, P: PrimaryKey> sqlx::Decode<'r, sqlx::Sqlite> for M2M<T, P> {
    fn decode(
        value: sqlx::sqlite::SqliteValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let _ = <i64 as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
        Ok(Self::empty())
    }
}

impl<'r, T: Model, P: PrimaryKey> sqlx::Decode<'r, sqlx::Postgres> for M2M<T, P> {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let _ = <i64 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::empty())
    }
}

impl<'q, T: Model, P: PrimaryKey> sqlx::Encode<'q, sqlx::Sqlite> for M2M<T, P> {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i64 as sqlx::Encode<'q, sqlx::Sqlite>>::encode_by_ref(&0i64, buf)
    }
}

impl<'q, T: Model, P: PrimaryKey> sqlx::Encode<'q, sqlx::Postgres> for M2M<T, P> {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i64 as sqlx::Encode<'q, sqlx::Postgres>>::encode_by_ref(&0i64, buf)
    }
}
