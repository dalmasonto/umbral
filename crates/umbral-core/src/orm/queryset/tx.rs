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
use super::write_helpers::{build_insert_one_for, serialize_to_map};

/// A `QuerySet` bound to an open transaction. See module docs for
/// the construction sites and the borrow-checker contract.
pub struct QuerySetTx<'tx, T> {
    pub(super) qs: QuerySet<T>,
    pub(super) tx: &'tx mut crate::db::Transaction,
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
    pub async fn delete(self) -> Result<u64, sqlx::Error> {
        let stmt = self.qs.build_delete_for(self.tx.backend_name());
        match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let result = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&mut **tx)
                    .await?;
                Ok(result.rows_affected())
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let result = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&mut **tx)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// UPDATE inside the transaction. Takes the same `column → JSON value`
    /// map as [`super::QuerySet::update_values`].
    pub async fn update_values(
        self,
        values: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, crate::orm::write::WriteError> {
        let stmt = self.qs.build_update_for(self.tx.backend_name(), &values)?;
        match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                let result = sqlx::query_with::<sqlx::Sqlite, _>(&sql, values)
                    .execute(&mut **tx)
                    .await?;
                Ok(result.rows_affected())
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let result = sqlx::query_with::<sqlx::Postgres, _>(&sql, values)
                    .execute(&mut **tx)
                    .await?;
                Ok(result.rows_affected())
            }
        }
    }

    /// INSERT one row and return the populated row, inside the transaction.
    ///
    /// This is the `Manager::create_in_tx` equivalent called through the
    /// QuerySet API: `Post::objects().on_tx(tx).create(instance).await?`.
    pub async fn create(self, instance: T) -> Result<T, crate::orm::write::WriteError>
    where
        T: serde::Serialize
            + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
    {
        let map = serialize_to_map(&instance)?;
        let stmt = build_insert_one_for::<T>(self.tx.backend_name(), &map)?;
        match self.tx.backend_name() {
            "sqlite" => {
                let tx = self.tx.as_sqlite_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(SqliteQueryBuilder);
                // Classify UNIQUE / FK / NOT NULL / CHECK violations into the
                // structured `WriteError` variants, symmetric with the non-tx
                // `QuerySet::create`. Without this a constraint violation inside
                // a transaction surfaces as an opaque `Sqlx(_)`, so callers that
                // branch on `WriteError::UniqueViolation` (e.g. the OAuth
                // username-retry loop) can't tell a collision from a real error.
                let mut row = sqlx::query_as_with::<sqlx::Sqlite, T, _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(|e| {
                        crate::orm::validation::classify_sql_error(&e, &map)
                            .unwrap_or(crate::orm::write::WriteError::Sqlx(e))
                    })?;
                row.set_m2m_parent_ids();
                Ok(row)
            }
            _ => {
                let tx = self.tx.as_pg_mut().unwrap();
                let (sql, values) = stmt.build_sqlx(PostgresQueryBuilder);
                let mut row = sqlx::query_as_with::<sqlx::Postgres, T, _>(&sql, values)
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(|e| {
                        crate::orm::validation::classify_sql_error(&e, &map)
                            .unwrap_or(crate::orm::write::WriteError::Sqlx(e))
                    })?;
                row.set_m2m_parent_ids();
                Ok(row)
            }
        }
    }
}
