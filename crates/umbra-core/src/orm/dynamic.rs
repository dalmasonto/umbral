//! Runtime-typed QuerySet — the ORM's answer to "I know my model at
//! request time, not compile time."
//!
//! `Manager<T>` is parameterised by a `T: Model` so the typed columns
//! (`post::TITLE`, `post::ID`) carry their `SqlType` and Rust type at
//! the type level. That's wrong for the admin: it walks the registry
//! at request time, so the model is a `ModelMeta` value, the column
//! name is a `String`, and the result row is a `HashMap<String,
//! String>` (templates can't see typed structs anyway).
//!
//! `DynQuerySet` is the parallel surface. It accepts string column
//! names against a `ModelMeta`, validates them at chain time (unknown
//! names are silently dropped so a stale `search_fields` config can't
//! crash a request), and renders to the same `sea_query` machinery
//! the typed path uses. Decoding goes through `SqlType` dispatch —
//! [`decode_to_string`] is the new pub helper that mirrors the
//! admin's private `column_to_string`.
//!
//! ## Scope of this first pass
//!
//! v0 ships the surface the admin's list / changelist / rows-fragment
//! handlers need today: `search`, `filter_eq_string`, `order_by_col`,
//! `limit`, `offset`, `count`, `fetch_as_strings`. INSERT / UPDATE /
//! DELETE plus a typed `DynValue` enum land as call sites
//! migrate. Postgres dispatch lands when the admin runs against
//! Postgres in earnest — for now the Postgres branches panic with a
//! clear message.

use std::collections::HashMap;

use sea_query::{
    Alias, Asterisk, Condition, Expr, Func, Order, Query, SqliteQueryBuilder, Value as SeaValue,
};
use sea_query_binder::SqlxBinder;
use sqlx::Row;

use crate::db::{DbPool, pool_dispatched};
use crate::migrate::{Column, ModelMeta};
use crate::orm::SqlType;
use crate::orm::write::{WriteError, json_to_sea_value, null_for};

/// Errors a runtime-typed query can produce. Thin alias — sqlx errors
/// drive every actual failure.
pub type DynError = sqlx::Error;

/// A runtime-typed, lazy SQL query against one `ModelMeta`.
///
/// Built by [`DynQuerySet::for_meta`]; chain `.search(...)` /
/// `.filter_eq_string(...)` / `.order_by_col(...)` / `.limit(...)` /
/// `.offset(...)` to refine; finish with `.count()` or
/// `.fetch_as_strings()`.
pub struct DynQuerySet<'a> {
    meta: &'a ModelMeta,
    /// Accumulated WHERE clauses, ANDed together at terminal time.
    /// Stored as `Condition` (not pushed into a `SelectStatement`
    /// directly) so `count()` and `fetch_as_strings()` can reuse the
    /// same predicate set against different SELECT projections.
    where_clauses: Vec<Condition>,
    order: Vec<(String, bool)>,
    limit: Option<u64>,
    offset: Option<u64>,
    select_cols: Vec<String>,
}

impl<'a> DynQuerySet<'a> {
    /// Start a `SELECT` against the model's table. The column list
    /// defaults to every field in declaration order; restrict it with
    /// `.select_cols(...)` before fetching when you only want a subset.
    pub fn for_meta(meta: &'a ModelMeta) -> Self {
        let select_cols = meta.fields.iter().map(|c| c.name.clone()).collect();
        Self {
            meta,
            where_clauses: Vec::new(),
            order: Vec::new(),
            limit: None,
            offset: None,
            select_cols,
        }
    }

    /// Restrict the SELECT list to the supplied column names. Names
    /// that don't exist on the model are silently dropped so a stale
    /// `list_display` config can't crash a request.
    pub fn select_cols(mut self, cols: &[String]) -> Self {
        let valid: Vec<String> = cols
            .iter()
            .filter(|n| self.meta.fields.iter().any(|c| &c.name == *n))
            .cloned()
            .collect();
        if !valid.is_empty() {
            self.select_cols = valid;
        }
        self
    }

    /// Add `WHERE (field1 LIKE ?% OR field2 LIKE ?% OR ...)` for the
    /// supplied search columns. Empty `fields` or empty `term` is a
    /// no-op. Columns that don't exist on the model are dropped.
    pub fn search(mut self, fields: &[String], term: &str) -> Self {
        if fields.is_empty() || term.is_empty() {
            return self;
        }
        let pattern = format!("%{term}%");
        let mut cond = Condition::any();
        let mut added = 0;
        for f in fields {
            if self.meta.fields.iter().any(|c| &c.name == f) {
                cond = cond.add(Expr::col(Alias::new(f)).like(pattern.clone()));
                added += 1;
            }
        }
        if added > 0 {
            self.where_clauses.push(cond);
        }
        self
    }

    /// Add `WHERE <col> IN (?, ?, ...)` for an i64 column (PK / FK).
    /// Empty `vals` is a no-op; unknown columns are silently dropped.
    pub fn filter_in_i64(mut self, col: &str, vals: &[i64]) -> Self {
        if vals.is_empty() || !self.meta.fields.iter().any(|c| c.name == col) {
            return self;
        }
        let cond = Condition::all().add(Expr::col(Alias::new(col)).is_in(vals.iter().copied()));
        self.where_clauses.push(cond);
        self
    }

    /// Add `WHERE <col> = <value>` where the value is parsed against
    /// the column's `SqlType` so SQLite's affinity rules see the right
    /// operand type.
    pub fn filter_eq_string(mut self, col: &str, value: &str) -> Self {
        let Some(meta_col) = self.meta.fields.iter().find(|c| c.name == col) else {
            return self;
        };
        let expr = Expr::col(Alias::new(col));
        let predicate = match meta_col.ty {
            SqlType::SmallInt | SqlType::Integer => value.parse::<i32>().ok().map(|v| expr.eq(v)),
            SqlType::BigInt | SqlType::ForeignKey => value.parse::<i64>().ok().map(|v| expr.eq(v)),
            SqlType::Real | SqlType::Double => value.parse::<f64>().ok().map(|v| expr.eq(v)),
            SqlType::Boolean => {
                let v = matches!(value, "true" | "on" | "1");
                Some(expr.eq(v))
            }
            _ => Some(expr.eq(value.to_string())),
        };
        if let Some(p) = predicate {
            self.where_clauses.push(Condition::all().add(p));
        }
        self
    }

    /// Add `ORDER BY <col> ASC|DESC`. Unknown columns are silently
    /// dropped. Multiple calls append (sea-query semantics).
    pub fn order_by_col(mut self, col: &str, descending: bool) -> Self {
        if self.meta.fields.iter().any(|c| c.name == col) {
            self.order.push((col.to_string(), descending));
        }
        self
    }

    /// Set `LIMIT`.
    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    /// Set `OFFSET`.
    pub fn offset(mut self, n: u64) -> Self {
        self.offset = Some(n);
        self
    }

    /// Terminal: `SELECT COUNT(*)` with the accumulated WHERE
    /// clauses. ORDER BY / LIMIT / OFFSET are dropped (irrelevant
    /// to a count).
    pub async fn count(self) -> Result<i64, DynError> {
        let mut q = Query::select();
        q.from(Alias::new(&self.meta.table));
        q.expr(Func::count(Expr::col(Asterisk)));
        for cond in &self.where_clauses {
            q.cond_where(cond.clone());
        }

        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let row = sqlx::query_with(&sql, values).fetch_one(pool).await?;
                Ok(row.try_get::<i64, _>(0)?)
            }
            DbPool::Postgres(_) => {
                unimplemented!("DynQuerySet::count: Postgres branch not yet wired")
            }
        }
    }

    /// Terminal: `SELECT DISTINCT <col>` with the accumulated WHERE.
    /// Returns each value as a string (via [`decode_to_string`]). LIMIT
    /// is honoured; ORDER BY isn't (DISTINCT ordering is whatever the
    /// underlying scan yields). Unknown column → empty result.
    pub async fn fetch_distinct_strings(self, col: &str) -> Result<Vec<String>, DynError> {
        let Some(col_meta) = self.meta.fields.iter().find(|c| c.name == col) else {
            return Ok(Vec::new());
        };
        let mut q = Query::select();
        q.distinct();
        q.from(Alias::new(&self.meta.table));
        q.column(Alias::new(col));
        for cond in &self.where_clauses {
            q.cond_where(cond.clone());
        }
        if let Some(n) = self.limit {
            q.limit(n);
        }

        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    out.push(decode_to_string(&row, col_meta)?);
                }
                Ok(out)
            }
            DbPool::Postgres(_) => {
                unimplemented!("DynQuerySet::fetch_distinct_strings: Postgres branch not yet wired")
            }
        }
    }

    /// Terminal: `DELETE FROM <table>` with the accumulated WHERE.
    /// Returns the number of rows affected.
    pub async fn delete(self) -> Result<u64, DynError> {
        let mut q = Query::delete();
        q.from_table(Alias::new(&self.meta.table));
        for cond in &self.where_clauses {
            q.cond_where(cond.clone());
        }

        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(res.rows_affected())
            }
            DbPool::Postgres(_) => {
                unimplemented!("DynQuerySet::delete: Postgres branch not yet wired")
            }
        }
    }

    /// Terminal: `UPDATE <table> SET <col> = <value>` with the
    /// accumulated WHERE. The value is parsed against the column's
    /// `SqlType` so SQLite affinity sees the right operand. Returns
    /// the number of rows affected. Unknown column → 0 rows.
    pub async fn update_one(self, col: &str, value: &str) -> Result<u64, DynError> {
        let Some(col_meta) = self.meta.fields.iter().find(|c| c.name == col) else {
            return Ok(0);
        };
        let sea_value = match form_str_to_sea_value(col_meta, value) {
            Ok(v) => v,
            Err(e) => return Err(sqlx::Error::Protocol(e.to_string())),
        };

        let mut q = Query::update();
        q.table(Alias::new(&self.meta.table));
        q.value(Alias::new(col), sea_value);
        for cond in &self.where_clauses {
            q.cond_where(cond.clone());
        }

        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(res.rows_affected())
            }
            DbPool::Postgres(_) => {
                unimplemented!("DynQuerySet::update_one: Postgres branch not yet wired")
            }
        }
    }

    /// Terminal: `UPDATE <table> SET <col1> = ?, <col2> = ?, ...` with
    /// the accumulated WHERE. Each form value is parsed against its
    /// column's `SqlType`. The primary key column is silently dropped
    /// from the form (it's the filter, not a target). `skip` lists
    /// columns the caller wants excluded (e.g. readonly fields the
    /// admin already enforced). Returns rows affected.
    pub async fn update_form(
        self,
        form: &HashMap<String, String>,
        skip: &[String],
    ) -> Result<u64, DynError> {
        let mut q = Query::update();
        q.table(Alias::new(&self.meta.table));
        let mut any = false;
        for col in &self.meta.fields {
            if col.primary_key || skip.iter().any(|s| s == &col.name) {
                continue;
            }
            let Some(raw) = form.get(&col.name) else {
                continue;
            };
            let sea_value = match form_str_to_sea_value(col, raw) {
                Ok(v) => v,
                Err(e) => return Err(sqlx::Error::Protocol(e.to_string())),
            };
            q.value(Alias::new(&col.name), sea_value);
            any = true;
        }
        if !any {
            return Ok(0);
        }
        for cond in &self.where_clauses {
            q.cond_where(cond.clone());
        }

        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(res.rows_affected())
            }
            DbPool::Postgres(_) => {
                unimplemented!("DynQuerySet::update_form: Postgres branch not yet wired")
            }
        }
    }

    /// Terminal: `INSERT INTO <table> (...) VALUES (...)` from a form
    /// map. Auto-increment integer PKs are omitted when the form value
    /// is missing or empty (SQLite hands out the next id). Form keys
    /// that don't match a column are ignored. `skip` lets the caller
    /// drop fields the admin pre-filtered. Returns `last_insert_rowid`.
    pub async fn insert_form(
        self,
        form: &HashMap<String, String>,
        skip: &[String],
    ) -> Result<i64, DynError> {
        let mut cols: Vec<&str> = Vec::new();
        let mut values: Vec<SeaValue> = Vec::new();
        for col in &self.meta.fields {
            if skip.iter().any(|s| s == &col.name) {
                continue;
            }
            // Auto-increment PK: omit when the form supplies no value
            // or an empty one; the backend hands out the next id.
            if col.primary_key
                && matches!(
                    col.ty,
                    SqlType::Integer | SqlType::BigInt | SqlType::SmallInt
                )
                && form.get(&col.name).is_none_or(|v| v.is_empty())
            {
                continue;
            }
            let raw = form.get(&col.name).map(|s| s.as_str()).unwrap_or("");
            let sea_value = match form_str_to_sea_value(col, raw) {
                Ok(v) => v,
                Err(e) => return Err(sqlx::Error::Protocol(e.to_string())),
            };
            cols.push(&col.name);
            values.push(sea_value);
        }
        if cols.is_empty() {
            return Ok(0);
        }

        let mut q = Query::insert();
        q.into_table(Alias::new(&self.meta.table));
        q.columns(cols.iter().map(|c| Alias::new(*c)).collect::<Vec<_>>());
        let exprs: Vec<sea_query::SimpleExpr> = values.into_iter().map(Into::into).collect();
        q.values_panic(exprs);

        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, vals).execute(pool).await?;
                Ok(res.last_insert_rowid())
            }
            DbPool::Postgres(_) => {
                unimplemented!("DynQuerySet::insert_form: Postgres branch not yet wired")
            }
        }
    }

    /// Terminal: fetch every row, decoding each cell to its string
    /// form via [`decode_to_string`]. Returns one `HashMap` per row,
    /// keyed by column name, holding only the columns named in
    /// `select_cols` (defaults to all).
    pub async fn fetch_as_strings(self) -> Result<Vec<HashMap<String, String>>, DynError> {
        let mut q = Query::select();
        q.from(Alias::new(&self.meta.table));
        for c in &self.select_cols {
            q.column(Alias::new(c));
        }
        for cond in &self.where_clauses {
            q.cond_where(cond.clone());
        }
        for (col, descending) in &self.order {
            q.order_by(
                Alias::new(col),
                if *descending { Order::Desc } else { Order::Asc },
            );
        }
        if let Some(n) = self.limit {
            q.limit(n);
        }
        if let Some(n) = self.offset {
            q.offset(n);
        }

        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
                let mut out: Vec<HashMap<String, String>> = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut entry = HashMap::new();
                    for col_name in &self.select_cols {
                        if let Some(col_meta) =
                            self.meta.fields.iter().find(|c| &c.name == col_name)
                        {
                            let v = decode_to_string(&row, col_meta)?;
                            entry.insert(col_name.clone(), v);
                        }
                    }
                    out.push(entry);
                }
                Ok(out)
            }
            DbPool::Postgres(_) => {
                unimplemented!("DynQuerySet::fetch_as_strings: Postgres branch not yet wired")
            }
        }
    }
}

/// Decode one SQLite cell to its template-friendly string form.
///
/// Public so admin-like crates can decode rows they fetched outside
/// `DynQuerySet` (typed row paths, ad-hoc joins). The dispatch mirrors
/// `bind_form_value`'s parse step in reverse.
pub fn decode_to_string(
    row: &sqlx::sqlite::SqliteRow,
    col: &Column,
) -> Result<String, sqlx::Error> {
    use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
    use serde_json::Value;
    use uuid::Uuid;

    let name = col.name.as_str();
    if col.nullable {
        return Ok(match col.ty {
            SqlType::SmallInt | SqlType::Integer => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::BigInt => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Real => row
                .try_get::<Option<f32>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Double => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Boolean => row
                .try_get::<Option<bool>, _>(name)?
                .map_or(String::new(), |v| {
                    if v { "true" } else { "false" }.to_string()
                }),
            SqlType::Text => row.try_get::<Option<String>, _>(name)?.unwrap_or_default(),
            SqlType::Date => row
                .try_get::<Option<NaiveDate>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Time => row
                .try_get::<Option<NaiveTime>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Timestamptz => row
                .try_get::<Option<DateTime<Utc>>, _>(name)?
                .map_or(String::new(), |v| v.to_rfc3339()),
            SqlType::Uuid => row
                .try_get::<Option<Uuid>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Json => row
                .try_get::<Option<Value>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Array(_) => panic_array_unsupported(&col.name),
            SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
                panic_pg_only_unsupported(&col.name)
            }
            SqlType::ForeignKey => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => row.try_get::<i32, _>(name)?.to_string(),
        SqlType::BigInt => row.try_get::<i64, _>(name)?.to_string(),
        SqlType::Real => row.try_get::<f32, _>(name)?.to_string(),
        SqlType::Double => row.try_get::<f64, _>(name)?.to_string(),
        SqlType::Boolean => if row.try_get::<bool, _>(name)? {
            "true"
        } else {
            "false"
        }
        .to_string(),
        SqlType::Text => row.try_get::<String, _>(name)?,
        SqlType::Date => row.try_get::<NaiveDate, _>(name)?.to_string(),
        SqlType::Time => row.try_get::<NaiveTime, _>(name)?.to_string(),
        SqlType::Timestamptz => row.try_get::<DateTime<Utc>, _>(name)?.to_rfc3339(),
        SqlType::Uuid => row.try_get::<Uuid, _>(name)?.to_string(),
        SqlType::Json => row.try_get::<Value, _>(name)?.to_string(),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        SqlType::ForeignKey => row.try_get::<i64, _>(name)?.to_string(),
    })
}

/// Convert one form-submitted string into a `SeaValue` ready for
/// binding. Handles the "empty + nullable" case explicitly so a blank
/// form field produces SQL NULL instead of an empty-string mismatch
/// for numeric columns. The rest of the conversion delegates to
/// [`json_to_sea_value`] by wrapping the value as `JsonValue::String`,
/// which already understands "true"/"false" booleans and RFC3339
/// timestamps the HTML form layer hands in.
fn form_str_to_sea_value(col: &Column, raw: &str) -> Result<SeaValue, WriteError> {
    if raw.is_empty() {
        if col.ty == SqlType::Boolean {
            // Unchecked checkbox = false, not NULL.
            return Ok(SeaValue::Bool(Some(false)));
        }
        if col.nullable {
            return Ok(null_for(col.ty));
        }
        return Err(WriteError::RequiredFieldMissing {
            field: col.name.clone(),
        });
    }
    let json = serde_json::Value::String(raw.to_string());
    json_to_sea_value(col.ty, &json, col.nullable, &col.name)
}

fn panic_array_unsupported(column: &str) -> ! {
    panic!(
        "DynQuerySet: column `{column}` is a Postgres-only Array; the \
         field/backend system check should have failed boot."
    )
}

fn panic_pg_only_unsupported(column: &str) -> ! {
    panic!(
        "DynQuerySet: column `{column}` is a Postgres-only network type \
         (Inet/Cidr/MacAddr); the field/backend system check should \
         have failed boot."
    )
}
