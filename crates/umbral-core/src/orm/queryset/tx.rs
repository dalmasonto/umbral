//! `QuerySetTx` — a QuerySet bound to an open transaction.
//!
//! Construction happens in [`super::QuerySet::on_tx`] /
//! [`super::Manager::on_tx`] using struct-literal syntax against the
//! `pub(super)` fields. All terminals here mirror their plain-QuerySet
//! siblings but route their SQL through the borrowed
//! [`crate::db::Transaction`] so the operations commit or roll back
//! as a unit with every other operation in the same
//! `umbral::db::transaction(...)` closure.
//!
//! The struct borrows `&mut Transaction` so the borrow checker
//! enforces that only one `QuerySetTx` uses the transaction at a
//! time, and that the transaction stays alive for the duration of
//! each terminal call.

use sea_query::{Expr, Func, PostgresQueryBuilder, SqliteQueryBuilder};
use sea_query_binder::SqlxBinder;

use crate::orm::{HydrateRelated, Model};

use super::QuerySet;
use super::errors::GetError;
use super::write_helpers::{build_insert_one_for, pk_field, serialize_to_map};

/// A `QuerySet` bound to an open transaction. See module docs for
/// the construction sites and the borrow-checker contract.
pub struct QuerySetTx<'tx, T> {
    pub(super) qs: QuerySet<T>,
    pub(super) tx: &'tx mut crate::db::Transaction,
}

/// Pull the PK column out of each already-decoded full row (the `has_sub`
/// RETURNING-* path in `delete()`), mirroring the non-tx `QuerySet::delete`.
fn pk_ids_from_full_rows(
    full: &[serde_json::Value],
    pk: Option<&'static crate::orm::FieldSpec>,
) -> Vec<serde_json::Value> {
    match pk {
        Some(field) => full
            .iter()
            .map(|r| {
                r.get(field.name)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
            })
            .collect(),
        None => Vec::new(),
    }
}

impl<'tx, T: Model> QuerySetTx<'tx, T> {
    // -----------------------------------------------------------------------
    // Read terminals
    // -----------------------------------------------------------------------

    /// SELECT all matching rows inside the transaction.
    pub async fn fetch(self) -> Result<Vec<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        let q = self.qs.build_query_for(self.tx.backend_name());
        let mut rows = match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await?
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await?
            }
        };
        // BUG-16 step 2: wire each row's PK into its M2M slots so
        // junction-table accessors used inside the transaction see
        // the right parent.
        for r in &mut rows {
            r.set_m2m_parent_ids();
        }
        Ok(rows)
    }

    /// SELECT LIMIT 1 and return the first row, if any.
    pub async fn first(mut self) -> Result<Option<T>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        self.qs.query.limit(1);
        self.qs.user_limit = Some(1);
        let q = self.qs.build_query_for(self.tx.backend_name());
        let mut row = match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_optional(&mut **tx)
                    .await?
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_optional(&mut **tx)
                    .await?
            }
        };
        if let Some(r) = row.as_mut() {
            r.set_m2m_parent_ids();
        }
        Ok(row)
    }

    /// SELECT COUNT(*) inside the transaction.
    pub async fn count(self) -> Result<i64, sqlx::Error> {
        let backend = self.tx.backend_name();
        let mut rebuilt = self.qs.build_query_for(backend);
        rebuilt.clear_selects();
        // `sea_query::Asterisk` renders the bare SQL `*` token; `Alias::new("*")`
        // would render `COUNT("*")` — a quoted identifier Postgres reads as a
        // column named `*`. Matches the non-transactional count path.
        rebuilt.expr(Func::count(Expr::col(sea_query::Asterisk)));
        rebuilt.reset_limit();
        rebuilt.reset_offset();
        match backend {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = rebuilt.build_sqlx(SqliteQueryBuilder);
                let (n,): (i64,) = sqlx::query_as_with::<sqlx::Sqlite, (i64,), _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await?;
                Ok(n)
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = rebuilt.build_sqlx(PostgresQueryBuilder);
                let (n,): (i64,) = sqlx::query_as_with::<sqlx::Postgres, (i64,), _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await?;
                Ok(n)
            }
        }
    }

    /// Return whether any row matches, inside the transaction.
    pub async fn exists(mut self) -> Result<bool, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.qs.query.limit(1);
        self.qs.user_limit = Some(1);
        let backend = self.tx.backend_name();
        let q = self.qs.build_query_for(backend);
        let row_opt: Option<T> = match backend {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_optional(&mut **tx)
                    .await?
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_optional(&mut **tx)
                    .await?
            }
        };
        Ok(row_opt.is_some())
    }

    /// Exactly-one terminal inside the transaction. See [`super::QuerySet::get`].
    pub async fn get(mut self) -> Result<T, GetError>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        self.qs.query.limit(2);
        self.qs.user_limit = Some(2);
        let q = self.qs.build_query_for(self.tx.backend_name());
        let mut rows: Vec<T> = match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(GetError::Sqlx)?
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(GetError::Sqlx)?
            }
        };
        match rows.len() {
            0 => Err(GetError::NotFound),
            1 => Ok(rows.pop().unwrap()),
            _ => Err(GetError::MultipleObjectsReturned),
        }
    }

    // -----------------------------------------------------------------------
    // Write terminals
    // -----------------------------------------------------------------------

    /// DELETE inside the transaction. Returns the number of rows deleted.
    ///
    /// On a `#[umbral(soft_delete)]` model this rewrites to
    /// `UPDATE ... SET deleted_at = NOW()` (plus the on_delete=cascade
    /// soft-cascade), exactly like the non-transactional `QuerySet::delete`
    /// — otherwise a `.delete()` that happened to run inside `on_tx()` would
    /// permanently destroy rows the caller expected to be recoverable.
    /// `.hard_delete()` opts back into a real DELETE.
    ///
    /// gaps6 #14/#15: mirrors the non-tx `QuerySet::delete()` signal
    /// contract exactly (per-row `post_delete` — full redacted row when
    /// subscribed, pk-only otherwise — plus one `bulk_post_delete`), except
    /// every emit is BUFFERED on the transaction and only runs after the
    /// caller's `db::transaction*` commits. A rollback drops the buffer
    /// with the transaction, so nothing fires for a delete that never
    /// actually happened.
    pub async fn delete(self) -> Result<u64, sqlx::Error> {
        if self.qs.soft_delete_active && !self.qs.hard_delete {
            return self.soft_delete_in_tx().await;
        }
        let mut stmt = self.qs.build_delete_for(self.tx.backend_name());
        let pk = pk_field::<T>();
        let has_sub =
            pk.is_some() && crate::signals::has_subscribers(&format!("post_delete:{}", T::TABLE));
        if has_sub {
            stmt.returning_all();
        } else if let Some(field) = pk {
            stmt.returning_col(sea_query::Alias::new(field.name));
        }
        let (ids, full_rows): (Vec<serde_json::Value>, Vec<serde_json::Value>) =
            match self.tx.backend_name() {
                "sqlite" => {
                    let tx = self.tx.as_sqlite_mut().unwrap();
                    let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                    let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                        .fetch_all(&mut **tx)
                        .await?;
                    if has_sub {
                        let full: Vec<serde_json::Value> = rows
                            .iter()
                            .map(super::backend_sqlite::row_to_json)
                            .collect();
                        let ids = pk_ids_from_full_rows(&full, pk);
                        (ids, full)
                    } else {
                        let ids = match pk {
                            Some(field) => rows
                                .iter()
                                .map(|r| super::backend_sqlite::pk_to_json(r, field.name, field.ty))
                                .collect::<Result<_, _>>()?,
                            None => Vec::new(),
                        };
                        (ids, Vec::new())
                    }
                }
                _ => {
                    let tx = self.tx.as_pg_mut().unwrap();
                    let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                    let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                        .fetch_all(&mut **tx)
                        .await?;
                    if has_sub {
                        let full: Vec<serde_json::Value> =
                            rows.iter().map(super::backend_pg::row_to_json).collect();
                        let ids = pk_ids_from_full_rows(&full, pk);
                        (ids, full)
                    } else {
                        let ids = match pk {
                            Some(field) => rows
                                .iter()
                                .map(|r| super::backend_pg::pk_to_json(r, field.name, field.ty))
                                .collect::<Result<_, _>>()?,
                            None => Vec::new(),
                        };
                        (ids, Vec::new())
                    }
                }
            };
        let count = ids.len() as u64;
        if !ids.is_empty() {
            if pk.is_some() {
                if has_sub {
                    for row in full_rows {
                        let mut row = row;
                        if let serde_json::Value::Object(map) = &mut row {
                            for f in T::SIGNAL_SKIP_FIELDS {
                                map.remove(*f);
                            }
                        }
                        let table = T::TABLE;
                        self.tx.push_pending_signal(Box::new(move || {
                            Box::pin(async move {
                                crate::signals::emit_post_delete_by_table(table, row).await;
                            })
                        }));
                    }
                } else if let Some(pkf) = pk {
                    for id in ids.iter() {
                        let payload = serde_json::json!({ pkf.name: id });
                        let table = T::TABLE;
                        self.tx.push_pending_signal(Box::new(move || {
                            Box::pin(async move {
                                crate::signals::emit_post_delete_by_table(table, payload).await;
                            })
                        }));
                    }
                }
            }
            let table = T::TABLE;
            self.tx.push_pending_signal(Box::new(move || {
                Box::pin(async move {
                    crate::signals::emit_bulk_post_delete_by_table(table, ids).await;
                })
            }));
        }
        Ok(count)
    }

    /// The soft-delete rewrite of [`Self::delete`], run inside the caller's
    /// transaction: cascade to any `on_delete = "cascade"` children, then
    /// stamp `deleted_at = NOW()` on the matched live rows (idempotent —
    /// never re-stamps an already soft-deleted row). Mirrors
    /// `QuerySet::soft_delete_update`, minus the private tx it opens.
    async fn soft_delete_in_tx(self) -> Result<u64, sqlx::Error> {
        use sea_query::{Alias, Query, Value};
        let backend = self.tx.backend_name();
        let now = chrono::Utc::now();
        let table = crate::db::router::schema_qualified_table(T::TABLE);

        // Cascade first — locate children through the parent's still-live
        // predicate before the parent is stamped, so no orphaned live child
        // is left behind.
        if let Some(pkf) = pk_field::<T>() {
            let mut sel = Query::select();
            sel.column(Alias::new(pkf.name)).from(table.clone());
            for p in &self.qs.predicates {
                sel.and_where(p.cond_for(backend));
            }
            sel.and_where(Expr::col(Alias::new("deleted_at")).is_null());
            let meta = crate::migrate::ModelMeta::for_::<T>();
            let mut conn = crate::orm::soft_delete_cascade::CascadeConn::from_tx(self.tx);
            crate::orm::soft_delete_cascade::cascade_soft_delete(&mut conn, &meta, sel, now)
                .await?;
        }

        let mut stmt = Query::update();
        stmt.table(table);
        stmt.value(
            Alias::new("deleted_at"),
            Value::ChronoDateTimeUtc(Some(Box::new(now))),
        );
        for p in &self.qs.predicates {
            stmt.and_where(p.cond_for(backend));
        }
        stmt.and_where(Expr::col(Alias::new("deleted_at")).is_null());
        let pk = pk_field::<T>();
        if let Some(pkf) = pk {
            stmt.returning_col(Alias::new(pkf.name));
        }

        // Mirrors `QuerySet::soft_delete_update`: only the bulk signal fires
        // for a soft delete, buffered here to run after commit (gaps6 #15).
        let ids: Vec<serde_json::Value> = match backend {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await?;
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| super::backend_sqlite::pk_to_json(r, field.name, field.ty))
                        .collect::<Result<_, _>>()?,
                    None => Vec::new(),
                }
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await?;
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| super::backend_pg::pk_to_json(r, field.name, field.ty))
                        .collect::<Result<_, _>>()?,
                    None => Vec::new(),
                }
            }
        };
        let count = ids.len() as u64;
        if !ids.is_empty() {
            let table = T::TABLE;
            self.tx.push_pending_signal(Box::new(move || {
                Box::pin(async move {
                    crate::signals::emit_bulk_post_delete_by_table(table, ids).await;
                })
            }));
        }
        Ok(count)
    }

    /// UPDATE inside the transaction. Takes the same `column → JSON value`
    /// map as [`super::QuerySet::update_values`]. Buffers `bulk_post_save`
    /// (mirroring the non-tx path's signal) to fire after commit (gaps6 #15).
    pub async fn update_values(
        self,
        values: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, crate::orm::write::WriteError> {
        let mut stmt = self.qs.build_update_for(self.tx.backend_name(), &values)?;
        let pk = pk_field::<T>();
        if let Some(field) = pk {
            stmt.returning_col(sea_query::Alias::new(field.name));
        }
        let ids: Vec<serde_json::Value> = match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await?;
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| super::backend_sqlite::pk_to_json(r, field.name, field.ty))
                        .collect::<Result<_, _>>()?,
                    None => Vec::new(),
                }
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .fetch_all(&mut **tx)
                    .await?;
                match pk {
                    Some(field) => rows
                        .iter()
                        .map(|r| super::backend_pg::pk_to_json(r, field.name, field.ty))
                        .collect::<Result<_, _>>()?,
                    None => Vec::new(),
                }
            }
        };
        let count = ids.len() as u64;
        if !ids.is_empty() {
            let table = T::TABLE;
            self.tx.push_pending_signal(Box::new(move || {
                Box::pin(async move {
                    crate::signals::emit_bulk_post_save_by_table(table, ids, false).await;
                })
            }));
        }
        Ok(count)
    }

    /// INSERT one row and return the populated row, inside the transaction.
    ///
    /// This is the `Manager::create_in_tx` equivalent called through the
    /// QuerySet API: `Post::objects().on_tx(tx).create(instance).await?`.
    ///
    /// gaps6 #15: buffers the same `post_save` (created=true) the non-tx
    /// `Manager::create` emits, redacted through `serialize_for_signal` —
    /// same choke point, same `SIGNAL_SKIP_FIELDS` contract — but the emit
    /// itself only runs after the caller's transaction commits.
    pub async fn create(self, instance: impl Into<T>) -> Result<T, crate::orm::write::WriteError>
    where
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        // gaps4 #88: accept a partial insert shape (`<Model>New`) as well as a
        // full `T` — `T: Into<T>` is identity, so existing calls are unchanged.
        let instance: T = instance.into();
        let map = serialize_to_map(&instance)?;
        let stmt = build_insert_one_for::<T>(self.tx.backend_name(), &map)?;
        let mut row: T = match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                // Classify UNIQUE / FK / NOT NULL / CHECK violations into the
                // structured `WriteError` variants, symmetric with the non-tx
                // `QuerySet::create`. Without this a constraint violation inside
                // a transaction surfaces as an opaque `Sqlx(_)`, so callers that
                // branch on `WriteError::UniqueViolation` (e.g. the OAuth
                // username-retry loop) can't tell a collision from a real error.
                sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(|e| {
                        crate::orm::validation::classify_sql_error(&e, &map)
                            .unwrap_or(crate::orm::write::WriteError::Sqlx(e))
                    })?
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(|e| {
                        crate::orm::validation::classify_sql_error(&e, &map)
                            .unwrap_or(crate::orm::write::WriteError::Sqlx(e))
                    })?
            }
        };
        row.set_m2m_parent_ids();
        if let Some(payload) = crate::signals::serialize_for_signal(&row, "post_save") {
            let table = T::TABLE;
            self.tx.push_pending_signal(Box::new(move || {
                Box::pin(async move {
                    crate::signals::emit_post_save_by_table(table, payload, true).await;
                })
            }));
        }
        Ok(row)
    }
}
