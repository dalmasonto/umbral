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
    /// Child PKs submitted through a form, awaiting the post-insert
    /// junction write. Drained by `take_pending_ids` in the typed
    /// create() path. Empty for hydrated / loaded rows.
    pending: Vec<sea_query::Value>,
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
            pending: Vec::new(),
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

    /// Stage child PKs to be written as junction rows after the parent
    /// insert. Called by the Form derive's validate().
    pub fn set_pending_ids(&mut self, ids: Vec<sea_query::Value>) {
        self.pending = ids;
    }

    /// Drain the staged child PKs (post-insert junction write).
    pub fn take_pending_ids(&mut self) -> Vec<sea_query::Value> {
        std::mem::take(&mut self.pending)
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
        .from_as(
            crate::db::router::schema_qualified_table(T::TABLE),
            Alias::new("c"),
        )
        .join_as(
            sea_query::JoinType::InnerJoin,
            crate::db::router::schema_qualified_table(junction),
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
    ///
    /// Fires `m2m_changed:<junction>` with `action: "add"`, the parent
    /// id, the child PK in `added`, an empty `removed`, and the actor
    /// task-local. The event fires even when the row already existed
    /// (the ON CONFLICT made it a no-op) so audit consumers see the
    /// user intent.
    pub async fn add(&self, child: &T) -> Result<(), sqlx::Error> {
        let Some((parent_id, junction)) = self.junction_handle() else {
            return Ok(());
        };
        let child_pk: T::PrimaryKey = child.primary_key();
        let child_pk_json = pk_seaval_to_json(child_pk.clone().into());
        let mut q = Query::insert();
        q.into_table(crate::db::router::schema_qualified_table(junction))
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
        execute_sql(&q).await?;
        let parent_id_json = pk_seaval_to_json(parent_id.into());
        crate::signals::emit_m2m_changed(
            junction,
            "add",
            parent_id_json,
            vec![child_pk_json],
            Vec::new(),
        )
        .await;
        Ok(())
    }

    /// Delete the junction row linking this parent to `child`. No-op
    /// when the relation doesn't exist or the M2M slot is unattached.
    ///
    /// Fires `m2m_changed:<junction>` with `action: "remove"`, the
    /// parent id, the child PK in `removed`, an empty `added`, and the
    /// actor task-local. The event fires regardless of whether the
    /// junction row existed (matching the intent-based semantics of
    /// [`Self::add`]).
    pub async fn remove(&self, child: &T) -> Result<(), sqlx::Error> {
        let Some((parent_id, junction)) = self.junction_handle() else {
            return Ok(());
        };
        let child_pk: T::PrimaryKey = child.primary_key();
        let child_pk_json = pk_seaval_to_json(child_pk.clone().into());
        let mut q = Query::delete();
        q.from_table(crate::db::router::schema_qualified_table(junction))
            .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id.clone()))
            .and_where(Expr::col(Alias::new("child_id")).eq(child_pk));
        execute_delete(&q).await?;
        let parent_id_json = pk_seaval_to_json(parent_id.into());
        crate::signals::emit_m2m_changed(
            junction,
            "remove",
            parent_id_json,
            Vec::new(),
            vec![child_pk_json],
        )
        .await;
        Ok(())
    }

    /// Delete every junction row for this parent. No-op when the
    /// M2M slot is unattached. Returns the number of rows removed.
    ///
    /// Fires `m2m_changed:<junction>` with `action: "clear"`, the
    /// parent id, the prior child PKs in `removed`, and an empty
    /// `added` — but only when at least one row was removed. Empty
    /// clears stay silent.
    pub async fn clear(&self) -> Result<u64, sqlx::Error> {
        let Some((parent_id, junction)) = self.junction_handle() else {
            return Ok(0);
        };
        let mut q = Query::delete();
        q.from_table(crate::db::router::schema_qualified_table(junction))
            .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id.clone()))
            .returning_col(Alias::new("child_id"));
        let removed_ids = execute_delete_returning_ids::<T>(&q).await?;
        let count = removed_ids.len() as u64;
        if !removed_ids.is_empty() {
            let parent_id_json = pk_seaval_to_json(parent_id.into());
            crate::signals::emit_m2m_changed(
                junction,
                "clear",
                parent_id_json,
                Vec::new(),
                removed_ids,
            )
            .await;
        }
        Ok(count)
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
        // Capture the supplied children's PKs up front for the signal
        // payload's `added` list. The DELETE-then-INSERT loop below
        // moves the typed `child_pk` value into the SQL, so we clone
        // its JSON shape first.
        let added_json: Vec<serde_json::Value> = children
            .iter()
            .map(|c| pk_seaval_to_json(c.primary_key().into()))
            .collect();
        let pool = crate::db::pool_dispatched();
        // Build the DELETE statement once — it's identical on both
        // backends. The RETURNING child_id rider captures the prior
        // set so the signal payload's `removed` list reflects what the
        // DB actually cleared.
        let mut delete = Query::delete();
        delete
            .from_table(crate::db::router::schema_qualified_table(junction))
            .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id.clone()))
            .returning_col(Alias::new("child_id"));
        let child_pk_ty = child_pk_ty::<T>();

        let removed_json: Vec<serde_json::Value> = match pool {
            crate::db::DbPool::Sqlite(p) => {
                let mut tx = p.begin().await?;
                let (sql, values) = delete.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(&mut *tx).await?;
                let removed: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|r| child_id_to_json_sqlite(r, child_pk_ty))
                    .collect::<Result<_, _>>()?;
                for child in children {
                    let child_pk: T::PrimaryKey = child.primary_key();
                    let mut insert = Query::insert();
                    insert
                        .into_table(crate::db::router::schema_qualified_table(junction))
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
                removed
            }
            crate::db::DbPool::Postgres(p) => {
                let mut tx = p.begin().await?;
                let (sql, values) = delete.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(&mut *tx).await?;
                let removed: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|r| child_id_to_json_pg(r, child_pk_ty))
                    .collect::<Result<_, _>>()?;
                for child in children {
                    let child_pk: T::PrimaryKey = child.primary_key();
                    let mut insert = Query::insert();
                    insert
                        .into_table(crate::db::router::schema_qualified_table(junction))
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
                removed
            }
        };

        let parent_id_json = pk_seaval_to_json(parent_id.into());
        crate::signals::emit_m2m_changed(junction, "set", parent_id_json, added_json, removed_json)
            .await;
        Ok(())
    }

    /// `(parent_id, junction_table)` shorthand. Returns `None` when
    /// either side is unset — the public CRUD treats that as "nothing
    /// to do" rather than an error, matching Django's behaviour on
    /// unsaved parent instances.
    fn junction_handle(&self) -> Option<(P, &'static str)> {
        Some((self.parent_id.clone()?, self.junction_table?))
    }

    // -----------------------------------------------------------------
    // Static bulk-across-parents queries.
    //
    // The instance methods (`add` / `remove` / `fetch`) are
    // single-parent: they read parent_id off `self` and act on one
    // junction row at a time. Permission gates and the like need to
    // check OR-membership across many parent ids in one query.
    // These free-standing helpers ride on the type's generic
    // parameters (`T` for the child Model, `P` for the parent PK)
    // and take the junction table name as an argument so they don't
    // require a constructed `M2M` instance.
    //
    // Callers usually know the junction name from the macro-derived
    // `<parent_table>_<field_name>` convention. For umbra-permissions
    // that's `"permissions_group_permissions"` for the
    // `Group.permissions: M2M<Permission>` field. Closes the BUG-16
    // phase 3 follow-up.
    // -----------------------------------------------------------------

    /// "Does any of `parent_ids` hold the junction relation to
    /// `child_pk`?" Returns `Ok(false)` for an empty `parent_ids`
    /// slice. Built as `SELECT 1 FROM <junction> WHERE parent_id
    /// IN (?,?,?) AND child_id = ? LIMIT 1` so the DB short-circuits
    /// on the first match; the bool comes from `fetch_optional`.
    ///
    /// Use case: permission gates. "Is the user in any group that
    /// holds this permission?" is `Group::permissions_junction()`
    /// any-holds against the user's group ids.
    pub async fn any_holds(
        junction_table: &str,
        parent_ids: &[P],
        child_pk: T::PrimaryKey,
    ) -> Result<bool, sqlx::Error> {
        if parent_ids.is_empty() {
            return Ok(false);
        }
        let mut q = Query::select();
        q.expr(Expr::value(1))
            .from(crate::db::router::schema_qualified_table(junction_table))
            .and_where(
                Expr::col(Alias::new("parent_id"))
                    .is_in(parent_ids.iter().cloned().map(|v| v.into())),
            )
            .and_where(Expr::col(Alias::new("child_id")).eq(Expr::value(child_pk)))
            .limit(1);
        let pool = crate::db::pool_dispatched();
        let exists = match pool {
            crate::db::DbPool::Sqlite(p) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_with(&sql, values)
                    .fetch_optional(p)
                    .await?
                    .is_some()
            }
            crate::db::DbPool::Postgres(p) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_with(&sql, values)
                    .fetch_optional(p)
                    .await?
                    .is_some()
            }
        };
        Ok(exists)
    }

    /// "Give me every distinct child PK any of `parent_ids` holds the
    /// junction relation to." Returns `Ok(Vec::new())` for an empty
    /// `parent_ids` slice. Built as `SELECT DISTINCT child_id FROM
    /// <junction> WHERE parent_id IN (?,?,?)`.
    ///
    /// Use case: collecting the full permission set for a user via
    /// their group memberships — one round-trip whatever the group
    /// count.
    ///
    /// Decoding `T::PrimaryKey` from `child_id` is what the extra
    /// trait bounds buy us; every built-in PK type
    /// (`i64` / `String` / `Uuid`) already satisfies them, and so
    /// does a user-defined newtype as long as it carries the matching
    /// `sqlx::Type` + `Decode` impls. See [`super::PrimaryKey`]'s
    /// extension-recipe docstring.
    pub async fn holders_of_any(
        junction_table: &str,
        parent_ids: &[P],
    ) -> Result<Vec<T::PrimaryKey>, sqlx::Error>
    where
        T::PrimaryKey: for<'r> sqlx::Decode<'r, sqlx::Sqlite>
            + for<'r> sqlx::Decode<'r, sqlx::Postgres>
            + sqlx::Type<sqlx::Sqlite>
            + sqlx::Type<sqlx::Postgres>
            + Send
            + Unpin,
    {
        if parent_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut q = Query::select();
        q.distinct()
            .column(Alias::new("child_id"))
            .from(crate::db::router::schema_qualified_table(junction_table))
            .and_where(
                Expr::col(Alias::new("parent_id"))
                    .is_in(parent_ids.iter().cloned().map(|v| v.into())),
            );
        let pool = crate::db::pool_dispatched();
        let rows: Vec<(T::PrimaryKey,)> = match pool {
            crate::db::DbPool::Sqlite(p) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, (T::PrimaryKey,), _>(&sql, values)
                    .fetch_all(p)
                    .await?
            }
            crate::db::DbPool::Postgres(p) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, (T::PrimaryKey,), _>(&sql, values)
                    .fetch_all(p)
                    .await?
            }
        };
        Ok(rows.into_iter().map(|(pk,)| pk).collect())
    }
}

/// Resolve the write pool for a junction operation, routing through the
/// `DatabaseRouter` when the model registry is available.
///
/// The junction table belongs to the parent model's database (that is the
/// same DB the migration engine targets when it creates the junction). We
/// therefore route by the parent's `ModelMeta`. When `parent_model` is
/// `None`, or when the registry isn't up yet (boot / low-level tests), we
/// fall back to `pool_dispatched()` — preserving today's single-DB
/// behaviour.
fn junction_pool_for_write(parent_model: Option<&str>) -> crate::db::DbPool {
    if let Some(name) = parent_model {
        if let Some(meta) = crate::migrate::model_meta_ref(name) {
            let ctx = crate::db::route_context::current();
            let alias = crate::db::router::router().db_for_write(meta, &ctx);
            return crate::db::pool_for_dispatched(alias.as_str()).clone();
        }
    }
    crate::db::pool_dispatched().clone()
}

/// Resolve the read pool for a junction operation, routing through the
/// `DatabaseRouter` when the model registry is available. See
/// [`junction_pool_for_write`] for the routing rationale.
fn junction_pool_for_read(parent_model: Option<&str>) -> crate::db::DbPool {
    if let Some(name) = parent_model {
        if let Some(meta) = crate::migrate::model_meta_ref(name) {
            let ctx = crate::db::route_context::current();
            let alias = crate::db::router::router().db_for_read(meta, &ctx);
            return crate::db::pool_for_dispatched(alias.as_str()).clone();
        }
    }
    crate::db::pool_dispatched().clone()
}

/// "Replace this parent's M2M junction entries with exactly
/// `child_ids`." The dynamic equivalent of [`M2M::set`] for callers
/// that only have the junction name + a list of typed
/// `sea_query::Value` PKs — the admin's form path is the motivating
/// use case, since it works with `ModelMeta` and form strings rather
/// than typed `T` instances.
///
/// Free-standing (not on `M2M<T, P>`) because the admin's form
/// handler doesn't know the typed child or parent at compile time;
/// it has only string values + a `Column` per side to derive the
/// SqlType from.
///
/// Runs `DELETE FROM <junction> WHERE parent_id = ?` followed by ONE
/// multi-row `INSERT ... VALUES (?,?),(?,?),... ON CONFLICT DO NOTHING`
/// for the whole child set, all inside a single transaction so a partial
/// replacement can't leak. Empty `child_ids` is the legitimate "clear the
/// relation" shape — the DELETE runs and no INSERT is emitted at all
/// (never an `INSERT ... VALUES ()`).
///
/// `parent_model` is the `Model::NAME` of the parent model. When
/// provided and the app registry is live, the write pool is selected
/// via the ambient `DatabaseRouter` (db-per-tenant / read-replica
/// routing). Pass `None` from low-level tests or contexts where only a
/// single pool exists; `pool_dispatched()` is used as the fallback so
/// single-DB apps are unchanged.
///
/// gaps2 #47: the insert was M one-row INSERTs in a loop; it's now a
/// single multi-row statement (M round-trips → 1) with the same
/// transaction + DELETE semantics and the same `ON CONFLICT DO NOTHING`.
///
/// Closes the BUG-16 phase 3 admin gap: the form for editing a
/// parent model can now persist M2M selections without knowing the
/// typed wrappers.
pub async fn set_junction_dynamic(
    junction_table: &str,
    parent_id: sea_query::Value,
    child_ids: Vec<sea_query::Value>,
    parent_model: Option<&str>,
) -> Result<(), sqlx::Error> {
    // Build the one multi-row INSERT shared by both backends. `None` when
    // there are no children — the empty case clears via DELETE alone and
    // must NOT emit an `INSERT ... VALUES ()`.
    let insert: Option<sea_query::InsertStatement> = if child_ids.is_empty() {
        None
    } else {
        let mut insert = Query::insert();
        insert
            .into_table(crate::db::router::schema_qualified_table(junction_table))
            .columns([Alias::new("parent_id"), Alias::new("child_id")])
            .on_conflict(
                OnConflict::columns([Alias::new("parent_id"), Alias::new("child_id")])
                    .do_nothing()
                    .to_owned(),
            );
        // One `.values_panic` call per child appends another VALUES row,
        // so the whole set lands in a single statement.
        for child_id in child_ids {
            insert.values_panic([
                Expr::value(parent_id.clone()).into(),
                Expr::value(child_id).into(),
            ]);
        }
        Some(insert)
    };

    let mut delete = Query::delete();
    delete
        .from_table(crate::db::router::schema_qualified_table(junction_table))
        .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id));

    let pool = junction_pool_for_write(parent_model);
    match pool {
        crate::db::DbPool::Sqlite(p) => {
            let mut tx = p.begin().await?;
            let (sql, values) = delete.build_sqlx(SqliteQueryBuilder);
            sqlx::query_with(&sql, values).execute(&mut *tx).await?;
            if let Some(insert) = insert {
                let (sql, values) = insert.build_sqlx(SqliteQueryBuilder);
                sqlx::query_with(&sql, values).execute(&mut *tx).await?;
            }
            tx.commit().await?;
        }
        crate::db::DbPool::Postgres(p) => {
            let mut tx = p.begin().await?;
            let (sql, values) = delete.build_sqlx(PostgresQueryBuilder);
            sqlx::query_with(&sql, values).execute(&mut *tx).await?;
            if let Some(insert) = insert {
                let (sql, values) = insert.build_sqlx(PostgresQueryBuilder);
                sqlx::query_with(&sql, values).execute(&mut *tx).await?;
            }
            tx.commit().await?;
        }
    }
    Ok(())
}

/// Transaction-aware sibling of [`set_junction_dynamic`]. Same
/// DELETE-then-multi-row-INSERT replacement, but every statement
/// runs on the passed transaction instead of opening its own. Used
/// by [`crate::orm::dynamic::DynQuerySet::insert_json_in_tx`] so a
/// nested create's junction rows commit (or roll back) atomically
/// with the parent + child INSERTs.
pub async fn set_junction_dynamic_in_tx(
    junction_table: &str,
    parent_id: sea_query::Value,
    child_ids: Vec<sea_query::Value>,
    tx: &mut crate::db::Transaction,
) -> Result<(), sqlx::Error> {
    let insert: Option<sea_query::InsertStatement> = if child_ids.is_empty() {
        None
    } else {
        let mut insert = Query::insert();
        insert
            .into_table(crate::db::router::schema_qualified_table(junction_table))
            .columns([Alias::new("parent_id"), Alias::new("child_id")])
            .on_conflict(
                OnConflict::columns([Alias::new("parent_id"), Alias::new("child_id")])
                    .do_nothing()
                    .to_owned(),
            );
        for child_id in child_ids {
            insert.values_panic([
                Expr::value(parent_id.clone()).into(),
                Expr::value(child_id).into(),
            ]);
        }
        Some(insert)
    };

    let mut delete = Query::delete();
    delete
        .from_table(crate::db::router::schema_qualified_table(junction_table))
        .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id));

    match tx.backend_name() {
        "sqlite" => {
            let inner = tx
                .as_sqlite_mut()
                .expect("backend_name == sqlite implies a sqlite tx");
            let (sql, values) = delete.build_sqlx(SqliteQueryBuilder);
            sqlx::query_with(&sql, values).execute(&mut **inner).await?;
            if let Some(insert) = insert {
                let (sql, values) = insert.build_sqlx(SqliteQueryBuilder);
                sqlx::query_with(&sql, values).execute(&mut **inner).await?;
            }
        }
        _ => {
            let inner = tx
                .as_pg_mut()
                .expect("backend_name == postgres implies a pg tx");
            let (sql, values) = delete.build_sqlx(PostgresQueryBuilder);
            sqlx::query_with(&sql, values).execute(&mut **inner).await?;
            if let Some(insert) = insert {
                let (sql, values) = insert.build_sqlx(PostgresQueryBuilder);
                sqlx::query_with(&sql, values).execute(&mut **inner).await?;
            }
        }
    }
    Ok(())
}

/// "Which child PKs does `parent_id` hold the M2M junction relation
/// to, as plain strings?" The dynamic equivalent of
/// [`M2M::fetch`] for callers that only have the junction name +
/// per-side PK [`SqlType`]s — the admin form's "pre-check current
/// selection" path is the motivating use case.
///
/// `child_pk_ty` selects the right `try_get<T>` codec from the
/// returned row; everything is stringified before return so the
/// template layer can string-compare against candidate PKs without
/// learning typed shapes.
///
/// `parent_model` is the `Model::NAME` of the parent model. When
/// provided and the app registry is live, the read pool is selected
/// via the ambient `DatabaseRouter`. Pass `None` from low-level
/// tests; `pool_dispatched()` is used as the fallback.
///
/// Free-standing for the same reason as `set_junction_dynamic`:
/// admin code works with `ModelMeta` / `SqlType`, not typed `T`.
pub async fn load_junction_selection(
    junction_table: &str,
    parent_id: sea_query::Value,
    child_pk_ty: super::SqlType,
    parent_model: Option<&str>,
) -> Result<Vec<String>, sqlx::Error> {
    let mut q = Query::select();
    q.distinct()
        .column(Alias::new("child_id"))
        .from(crate::db::router::schema_qualified_table(junction_table))
        .and_where(Expr::col(Alias::new("parent_id")).eq(parent_id));
    let pool = junction_pool_for_read(parent_model);
    let mut out: Vec<String> = Vec::new();
    match &pool {
        crate::db::DbPool::Sqlite(p) => {
            let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(p).await?;
            for row in rows {
                let s = stringify_sqlite_child_id(&row, child_pk_ty)?;
                out.push(s);
            }
        }
        crate::db::DbPool::Postgres(p) => {
            let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(p).await?;
            for row in rows {
                let s = stringify_postgres_child_id(&row, child_pk_ty)?;
                out.push(s);
            }
        }
    }
    Ok(out)
}

/// Decode the `child_id` column of one junction row into a string,
/// using the SqlType to pick the right typed `try_get`. Mirrors the
/// `decode_to_string` dispatch in `orm::dynamic` but specialised
/// to the junction's only column.
fn stringify_sqlite_child_id(
    row: &sqlx::sqlite::SqliteRow,
    ty: super::SqlType,
) -> Result<String, sqlx::Error> {
    use sqlx::Row;
    Ok(match ty {
        super::SqlType::SmallInt | super::SqlType::Integer => {
            row.try_get::<i32, _>("child_id")?.to_string()
        }
        super::SqlType::BigInt | super::SqlType::ForeignKey => {
            row.try_get::<i64, _>("child_id")?.to_string()
        }
        super::SqlType::Uuid => row.try_get::<uuid::Uuid, _>("child_id")?.to_string(),
        // TEXT and anything else come back as a String.
        _ => row.try_get::<String, _>("child_id")?,
    })
}

fn stringify_postgres_child_id(
    row: &sqlx::postgres::PgRow,
    ty: super::SqlType,
) -> Result<String, sqlx::Error> {
    use sqlx::Row;
    Ok(match ty {
        super::SqlType::SmallInt => row.try_get::<i16, _>("child_id")?.to_string(),
        super::SqlType::Integer => row.try_get::<i32, _>("child_id")?.to_string(),
        super::SqlType::BigInt | super::SqlType::ForeignKey => {
            row.try_get::<i64, _>("child_id")?.to_string()
        }
        super::SqlType::Uuid => row.try_get::<uuid::Uuid, _>("child_id")?.to_string(),
        _ => row.try_get::<String, _>("child_id")?,
    })
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

/// Convert a `sea_query::Value` into a `serde_json::Value`, preserving
/// numeric/string types where possible and falling back to the
/// Display-style stringification for unusual variants. Used by the M2M
/// signal-emission paths to land typed PKs in `serde_json` payloads
/// without forcing every PK type to implement Serialize.
fn pk_seaval_to_json(v: sea_query::Value) -> serde_json::Value {
    use sea_query::Value as SV;
    use serde_json::json;
    match v {
        SV::TinyInt(Some(n)) => json!(n),
        SV::SmallInt(Some(n)) => json!(n),
        SV::Int(Some(n)) => json!(n),
        SV::BigInt(Some(n)) => json!(n),
        SV::TinyUnsigned(Some(n)) => json!(n),
        SV::SmallUnsigned(Some(n)) => json!(n),
        SV::Unsigned(Some(n)) => json!(n),
        SV::BigUnsigned(Some(n)) => json!(n),
        SV::Float(Some(f)) => json!(f),
        SV::Double(Some(f)) => json!(f),
        SV::String(Some(s)) => json!(*s),
        // Uuid + any other sea_query::Value variant falls through to a
        // best-effort Display rendering. Uuid's Display gives the
        // canonical hyphenated form which round-trips cleanly.
        other => json!(format!("{:?}", other)),
    }
}

/// Look up the child model's PK SqlType so the `RETURNING child_id`
/// decoder can pick the right typed `try_get`.
fn child_pk_ty<T: Model>() -> super::SqlType {
    T::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.ty)
        .unwrap_or(super::SqlType::BigInt)
}

/// Decode a junction row's `child_id` column into a JSON value
/// suitable for the m2m_changed signal payload. Mirrors
/// `stringify_sqlite_child_id` but returns `serde_json::Value` so
/// integers stay numeric.
fn child_id_to_json_sqlite(
    row: &sqlx::sqlite::SqliteRow,
    ty: super::SqlType,
) -> Result<serde_json::Value, sqlx::Error> {
    use serde_json::json;
    use sqlx::Row;
    Ok(match ty {
        super::SqlType::SmallInt | super::SqlType::Integer => {
            json!(row.try_get::<i32, _>("child_id")?)
        }
        super::SqlType::BigInt | super::SqlType::ForeignKey => {
            json!(row.try_get::<i64, _>("child_id")?)
        }
        super::SqlType::Uuid => json!(row.try_get::<uuid::Uuid, _>("child_id")?.to_string()),
        _ => json!(row.try_get::<String, _>("child_id")?),
    })
}

fn child_id_to_json_pg(
    row: &sqlx::postgres::PgRow,
    ty: super::SqlType,
) -> Result<serde_json::Value, sqlx::Error> {
    use serde_json::json;
    use sqlx::Row;
    Ok(match ty {
        super::SqlType::SmallInt => json!(row.try_get::<i16, _>("child_id")?),
        super::SqlType::Integer => json!(row.try_get::<i32, _>("child_id")?),
        super::SqlType::BigInt | super::SqlType::ForeignKey => {
            json!(row.try_get::<i64, _>("child_id")?)
        }
        super::SqlType::Uuid => json!(row.try_get::<uuid::Uuid, _>("child_id")?.to_string()),
        _ => json!(row.try_get::<String, _>("child_id")?),
    })
}

/// DELETE that also captures the affected `child_id`s via RETURNING.
/// Used by [`M2M::clear`] (and the standalone `clear` paths) to feed
/// the m2m_changed signal's `removed` list without a separate SELECT.
async fn execute_delete_returning_ids<T: Model>(
    q: &sea_query::DeleteStatement,
) -> Result<Vec<serde_json::Value>, sqlx::Error> {
    let ty = child_pk_ty::<T>();
    let pool = crate::db::pool_dispatched();
    Ok(match pool {
        crate::db::DbPool::Sqlite(p) => {
            let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(p).await?;
            rows.iter()
                .map(|r| child_id_to_json_sqlite(r, ty))
                .collect::<Result<_, _>>()?
        }
        crate::db::DbPool::Postgres(p) => {
            let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(p).await?;
            rows.iter()
                .map(|r| child_id_to_json_pg(r, ty))
                .collect::<Result<_, _>>()?
        }
    })
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
