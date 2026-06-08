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
    Alias, Asterisk, Condition, Expr, Func, Order, PostgresQueryBuilder, Query, SqliteQueryBuilder,
    Value as SeaValue,
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

    /// Add `WHERE (<predicate1> OR <predicate2> OR ...)` for a free-text
    /// term against the model's searchable columns. Per-column predicate
    /// depends on the column's [`SqlType`]:
    ///
    /// | SqlType | Predicate |
    /// |---|---|
    /// | `Text` | `UPPER(col) LIKE '%TERM%'` — case-insensitive substring |
    /// | `SmallInt` / `Integer` / `BigInt` / `ForeignKey` | `col = term` when `term.parse::<i64>().is_ok()` |
    /// | `Real` / `Double` | `col = term` when `term.parse::<f64>().is_ok()` |
    /// | `Boolean` | `col = term` when `term` parses as `true` / `false` |
    /// | everything else (Date, Time, Uuid, Json, Bytes, Array, …) | skipped |
    ///
    /// `fields` controls which columns participate:
    ///
    /// - **Non-empty:** restricted to the named columns. Names that
    ///   don't exist on the model are silently dropped.
    /// - **Empty:** every column on the model is a candidate; the
    ///   per-type table above decides which actually contribute. This
    ///   is the "no `search_fields` configured" default Django gives
    ///   you out of the box.
    ///
    /// Empty `term` (after trimming) is always a no-op. If the column
    /// selection results in zero predicates (e.g. `term = "abc"` and
    /// the only candidate columns are numeric), nothing is appended.
    pub fn search(mut self, fields: &[String], term: &str) -> Self {
        let term = term.trim();
        if term.is_empty() {
            return self;
        }

        let restricted = !fields.is_empty();
        let as_int = term.parse::<i64>().ok();
        let as_float = term.parse::<f64>().ok();
        let as_bool = match term.to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        let like_pat = format!("%{term}%").to_uppercase();

        let mut cond = Condition::any();
        let mut added = 0;
        for col in &self.meta.fields {
            if restricted && !fields.iter().any(|f| f == &col.name) {
                continue;
            }
            let predicate: Option<sea_query::SimpleExpr> = match col.ty {
                SqlType::Text => Some(
                    Expr::expr(Func::upper(Expr::col(Alias::new(&col.name))))
                        .like(like_pat.clone()),
                ),
                SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::ForeignKey => {
                    as_int.map(|n| Expr::col(Alias::new(&col.name)).eq(n))
                }
                SqlType::Real | SqlType::Double => {
                    as_float.map(|n| Expr::col(Alias::new(&col.name)).eq(n))
                }
                SqlType::Boolean => as_bool.map(|b| Expr::col(Alias::new(&col.name)).eq(b)),
                _ => None,
            };
            if let Some(p) = predicate {
                cond = cond.add(p);
                added += 1;
            }
        }
        if added > 0 {
            self.where_clauses.push(cond);
        }
        self
    }

    /// Splice an externally-built `sea_query::Condition` into the
    /// accumulated WHERE clauses. Used by callers that need lookups
    /// the typed builder methods don't cover (e.g. umbra-rest's
    /// django-filter-style parser produces a `Condition` per
    /// `field__lookup=value` triple and feeds it in here).
    pub fn filter_condition(mut self, cond: sea_query::Condition) -> Self {
        self.where_clauses.push(cond);
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

    /// Add `WHERE <col> IN (?, ?, ...)` for any column. Each value is
    /// parsed against the column's [`SqlType`] (same coercion as
    /// [`Self::filter_eq_string`]) so SQLite's affinity rules see the
    /// right operand type. Values that fail to parse are dropped from
    /// the IN list. Empty `vals` (or all-unparseable) is a no-op;
    /// unknown columns are silently dropped.
    ///
    /// Single-value calls degenerate to `<col> = ?` via sea-query's
    /// `is_in` lowering — callers can use this for both the "one
    /// selection" and "multi-selection" filter paths and get the
    /// natural SQL in each case.
    pub fn filter_in_strings(mut self, col: &str, vals: &[String]) -> Self {
        let Some(meta_col) = self.meta.fields.iter().find(|c| c.name == col) else {
            return self;
        };
        if vals.is_empty() {
            return self;
        }
        let expr = Expr::col(Alias::new(col));
        // Coerce each string value to the column's native type so the
        // bind kind matches and SQLite's STRICT mode (and Postgres's
        // type system) accepts the parameter.
        let cond = match meta_col.ty {
            SqlType::SmallInt | SqlType::Integer => {
                let parsed: Vec<i32> = vals.iter().filter_map(|s| s.parse().ok()).collect();
                if parsed.is_empty() {
                    return self;
                }
                Condition::all().add(expr.is_in(parsed))
            }
            SqlType::BigInt | SqlType::ForeignKey => {
                let parsed: Vec<i64> = vals.iter().filter_map(|s| s.parse().ok()).collect();
                if parsed.is_empty() {
                    return self;
                }
                Condition::all().add(expr.is_in(parsed))
            }
            SqlType::Real | SqlType::Double => {
                let parsed: Vec<f64> = vals.iter().filter_map(|s| s.parse().ok()).collect();
                if parsed.is_empty() {
                    return self;
                }
                Condition::all().add(expr.is_in(parsed))
            }
            SqlType::Boolean => {
                let parsed: Vec<bool> = vals
                    .iter()
                    .map(|s| matches!(s.as_str(), "true" | "on" | "1"))
                    .collect();
                Condition::all().add(expr.is_in(parsed))
            }
            _ => Condition::all().add(expr.is_in(vals.iter().map(|s| s.to_string()))),
        };
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
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let row = sqlx::query_with(&sql, values).fetch_one(pool).await?;
                Ok(row.try_get::<i64, _>(0)?)
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
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    out.push(decode_pg_to_string(&row, col_meta)?);
                }
                Ok(out)
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
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(res.rows_affected())
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
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(res.rows_affected())
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
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(res.rows_affected())
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
            DbPool::Postgres(pool) => {
                // Postgres doesn't have last_insert_rowid; we ask for
                // RETURNING the PK and read it back. Falls back to 0
                // when the model has no integer PK (e.g. UUID PKs) —
                // the caller's flow needs to skip relying on the
                // return value in that case.
                let pk_name = self
                    .meta
                    .fields
                    .iter()
                    .find(|c| c.primary_key)
                    .map(|c| c.name.clone());
                if let Some(pk) = pk_name {
                    q.returning_col(Alias::new(&pk));
                    let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                    let row = sqlx::query_with(&sql, vals).fetch_one(pool).await?;
                    Ok(row.try_get::<i64, _>(pk.as_str()).unwrap_or(0))
                } else {
                    let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                    let _ = sqlx::query_with(&sql, vals).execute(pool).await?;
                    Ok(0)
                }
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
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
                let mut out: Vec<HashMap<String, String>> = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut entry = HashMap::new();
                    for col_name in &self.select_cols {
                        if let Some(col_meta) =
                            self.meta.fields.iter().find(|c| &c.name == col_name)
                        {
                            let v = decode_pg_to_string(&row, col_meta)?;
                            entry.insert(col_name.clone(), v);
                        }
                    }
                    out.push(entry);
                }
                Ok(out)
            }
        }
    }

    /// Terminal: fetch every row, decoding each cell to a
    /// `serde_json::Value` that preserves JSON shape (numbers stay
    /// numbers, booleans stay booleans, JSON columns nest verbatim).
    /// The right shape for HTTP API responses. Returns one
    /// `serde_json::Map` per row, keyed by column name.
    pub async fn fetch_as_json(
        self,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, DynError> {
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

        let pk_name = self
            .meta
            .pk_column()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
                let mut out: Vec<serde_json::Map<String, serde_json::Value>> =
                    Vec::with_capacity(rows.len());
                for row in rows {
                    let mut entry = serde_json::Map::new();
                    for col_name in &self.select_cols {
                        if let Some(col_meta) =
                            self.meta.fields.iter().find(|c| &c.name == col_name)
                        {
                            entry.insert(col_name.clone(), decode_to_json(&row, col_meta)?);
                        }
                    }
                    // Echo M2M relations alongside the scalar
                    // columns. Per-row, per-relation `SELECT` is
                    // the v1 shape; `prefetch_related`-style batch
                    // loading is deferred.
                    if !self.meta.m2m_relations.is_empty() {
                        let pk_val = entry.get(&pk_name).cloned();
                        hydrate_m2m_into(&self.meta, pk_val.as_ref(), &mut entry).await?;
                    }
                    out.push(entry);
                }
                Ok(out)
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
                let mut out: Vec<serde_json::Map<String, serde_json::Value>> =
                    Vec::with_capacity(rows.len());
                for row in rows {
                    let mut entry = serde_json::Map::new();
                    for col_name in &self.select_cols {
                        if let Some(col_meta) =
                            self.meta.fields.iter().find(|c| &c.name == col_name)
                        {
                            entry.insert(col_name.clone(), decode_pg_to_json(&row, col_meta)?);
                        }
                    }
                    if !self.meta.m2m_relations.is_empty() {
                        let pk_val = entry.get(&pk_name).cloned();
                        hydrate_m2m_into(&self.meta, pk_val.as_ref(), &mut entry).await?;
                    }
                    out.push(entry);
                }
                Ok(out)
            }
        }
    }

    /// Terminal: fetch the first row (LIMIT 1) as a JSON object.
    /// Returns `None` when the filter matches zero rows.
    pub async fn first_as_json(
        mut self,
    ) -> Result<Option<serde_json::Map<String, serde_json::Value>>, DynError> {
        self.limit = Some(1);
        let mut rows = self.fetch_as_json().await?;
        Ok(rows.pop())
    }

    /// Terminal: INSERT one row from a JSON map. Auto-increment integer
    /// PKs are omitted when missing or null (the backend assigns).
    /// Returns the newly-inserted row as JSON (via RETURNING * on
    /// Postgres; via last_insert_rowid → SELECT * on SQLite). The
    /// per-column JSON-to-SeaValue coercion goes through the existing
    /// `json_to_sea_value` so timestamp / uuid / json paths are the
    /// same as the typed Manager::create path.
    pub async fn insert_json(
        self,
        body: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, crate::orm::write::WriteError> {
        use crate::orm::write::{WriteError, is_default_pk};

        // Phase -1 — strip `noform` columns. The user-facing
        // contract is "this column is server-managed; clients
        // never write to it" — REST callers used to filter at
        // the boundary, but the rule belongs at the dynamic-
        // write seam so every consumer (REST, admin, custom
        // handlers) gets it for free. The owned clone lets us
        // continue to take `&body` from the caller.
        //
        // Gap 109: we also need a mutable view to auto-derive
        // `#[umbra(slug_from = "...")]` columns before validation
        // runs (so a missing-required check doesn't fire on a
        // slug we're about to fill). When either rule triggers we
        // take the owned copy; otherwise the borrow passes
        // through.
        let needs_owned = self
            .meta
            .fields
            .iter()
            .any(|c| c.noform || c.slug_from.is_some());
        let mut body_owned: serde_json::Map<String, serde_json::Value>;
        let body: &serde_json::Map<String, serde_json::Value> = if needs_owned {
            body_owned = body.clone();
            for col in &self.meta.fields {
                if col.noform {
                    body_owned.remove(&col.name);
                }
            }
            crate::orm::write::apply_slug_from(&self.meta.fields, &mut body_owned, false);
            &body_owned
        } else {
            body
        };

        // Phase 0 — pre-DB validation. Required-field + FK
        // existence + choices + M2M shape checks run together
        // so the response carries every problem in one round-
        // trip. The REST plugin used to do this; centralising
        // it here means the admin plugin and any third-party
        // caller of `insert_json` gets the same structured
        // errors.
        let validation_errors = crate::orm::validation::validate_on_create(&self.meta, body).await;
        if !validation_errors.is_empty() {
            return Err(WriteError::Multiple {
                errors: validation_errors,
            });
        }

        let mut cols: Vec<&str> = Vec::new();
        let mut values: Vec<SeaValue> = Vec::new();
        for col in &self.meta.fields {
            // Auto-increment PK: omit when the body supplies no value,
            // null, or the integer sentinel 0. The backend hands out
            // the next id.
            if col.primary_key {
                let supplied = body.get(&col.name);
                let is_sentinel = match supplied {
                    None | Some(serde_json::Value::Null) => true,
                    Some(v) => is_default_pk(col.ty, v),
                };
                if matches!(
                    col.ty,
                    SqlType::Integer | SqlType::BigInt | SqlType::SmallInt
                ) && is_sentinel
                {
                    continue;
                }
            }
            let Some(json) = body.get(&col.name) else {
                // BUG-5 fix: `auto_now` and `auto_now_add` columns
                // auto-populate with `Utc::now()` on the dynamic
                // write path when the body omits them — closes
                // the gap where the REST plugin's POST handler
                // would reject a required `created_at` field even
                // though the framework was supposed to manage it.
                if col.auto_now_add || col.auto_now {
                    let now_value = crate::orm::write::now_for_column(col.ty);
                    cols.push(&col.name);
                    values.push(now_value);
                    continue;
                }
                // `validate_on_create` already caught
                // missing-required-field cases above, but a
                // column with a default that the body omitted
                // still needs to be skipped here so the backend
                // fills it.
                continue;
            };
            if json.is_null() {
                // Pre-validation lets nullable nulls through; a
                // null on a non-nullable column was caught above.
                continue;
            }
            // IMP-3: pre-validate `#[umbra(min = N)]` / `max = N`.
            // The DB-side CHECK catches violations too; surfacing
            // a structured error is friendlier.
            if let Some(n) = json.as_i64() {
                if let Some(min) = col.min {
                    if n < min {
                        return Err(WriteError::Validator {
                            field: col.name.clone(),
                            message: format!("must be >= {min} (got {n})."),
                        });
                    }
                }
                if let Some(max) = col.max {
                    if n > max {
                        return Err(WriteError::Validator {
                            field: col.name.clone(),
                            message: format!("must be <= {max} (got {n})."),
                        });
                    }
                }
            }
            // BUG-11/12/13: Slug / Email / Url wrappers.
            if let (Some(fmt), Some(s)) = (col.text_format.as_deref(), json.as_str()) {
                if let Err(e) = crate::orm::validators::validate_text_format(fmt, s) {
                    return Err(WriteError::Validator {
                        field: col.name.clone(),
                        message: e.to_string(),
                    });
                }
            }
            let sea_value =
                crate::orm::write::json_to_sea_value(col.ty, json, col.nullable, &col.name)?;
            cols.push(&col.name);
            values.push(sea_value);
        }

        // The PK name we'll read back. Used for RETURNING on Postgres
        // and for the SQLite follow-up SELECT.
        let pk_col = self
            .meta
            .fields
            .iter()
            .find(|c| c.primary_key)
            .ok_or_else(|| {
                WriteError::Sqlx(sqlx::Error::Protocol(
                    "insert_json: model has no PK".to_string(),
                ))
            })?;
        let pk_name = pk_col.name.clone();
        let pk_ty = pk_col.ty;

        let mut q = Query::insert();
        q.into_table(Alias::new(&self.meta.table));
        q.columns(cols.iter().map(|c| Alias::new(*c)).collect::<Vec<_>>());
        let exprs: Vec<sea_query::SimpleExpr> = values.into_iter().map(Into::into).collect();
        q.values_panic(exprs);

        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, vals)
                    .execute(pool)
                    .await
                    .map_err(|e| classify_or_sqlx(e, body))?;
                // Re-fetch by PK so the caller sees the row as the DB
                // stored it (defaults, autoincrement, server-side
                // coercion).
                let pk_pred = match pk_ty {
                    SqlType::Integer | SqlType::BigInt | SqlType::SmallInt => {
                        Expr::col(Alias::new(&pk_name)).eq(res.last_insert_rowid())
                    }
                    _ => {
                        // Client-supplied non-integer PK: pull it back
                        // from the body.
                        let supplied = body
                            .get(&pk_name)
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let sea_value = crate::orm::write::json_to_sea_value(
                            pk_ty, &supplied, false, &pk_name,
                        )?;
                        Expr::col(Alias::new(&pk_name)).eq(sea_value)
                    }
                };
                let mut sel = Query::select();
                sel.from(Alias::new(&self.meta.table));
                for c in &self.meta.fields {
                    sel.column(Alias::new(&c.name));
                }
                sel.cond_where(Condition::all().add(pk_pred));
                let (sel_sql, sel_vals) = sel.build_sqlx(SqliteQueryBuilder);
                let row = sqlx::query_with(&sel_sql, sel_vals).fetch_one(pool).await?;
                let mut out = serde_json::Map::new();
                for col in &self.meta.fields {
                    out.insert(col.name.clone(), decode_to_json(&row, col)?);
                }
                // Phase 2 — write junction rows for every M2M
                // relation the body carried. Validation has
                // already confirmed the array shape + element
                // existence; we just have to mirror the ids into
                // the auto-generated `<table>_<field>` table.
                let pk_value = out.get(&pk_name).cloned();
                write_m2m_junctions(&self.meta, pk_value.as_ref(), body).await?;
                // Phase 3 — hydrate M2M arrays back into the
                // response so the caller sees `tags: [1, 2]`
                // instead of an empty echo.
                hydrate_m2m_into(&self.meta, pk_value.as_ref(), &mut out).await?;
                Ok(out)
            }
            DbPool::Postgres(pool) => {
                // `RETURNING *` fetches every column of the newly-inserted
                // row in one round trip. sea-query's chained
                // `returning_col` calls don't accumulate, so we use the
                // explicit "all columns" variant.
                q.returning_all();
                let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                let row = sqlx::query_with(&sql, vals)
                    .fetch_one(pool)
                    .await
                    .map_err(|e| classify_or_sqlx(e, body))?;
                let mut out = serde_json::Map::new();
                for col in &self.meta.fields {
                    out.insert(col.name.clone(), decode_pg_to_json(&row, col)?);
                }
                let pk_value = out.get(&pk_name).cloned();
                write_m2m_junctions(&self.meta, pk_value.as_ref(), body).await?;
                hydrate_m2m_into(&self.meta, pk_value.as_ref(), &mut out).await?;
                Ok(out)
            }
        }
    }

    /// Terminal: PATCH semantics — update only the columns present
    /// in `body`. The accumulated WHERE clauses narrow the target
    /// row(s). Returns the number of rows affected.
    pub async fn update_json(
        self,
        body: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, crate::orm::write::WriteError> {
        use crate::orm::write::WriteError;

        // Phase -1 — strip `noform` columns (server-managed
        // fields the client must not overwrite).
        //
        // Gap 109: also auto-derive `slug_from` columns when the
        // source field is part of the update body (see
        // `apply_slug_from`'s update guard for why).
        let needs_owned = self
            .meta
            .fields
            .iter()
            .any(|c| c.noform || c.slug_from.is_some());
        let mut body_owned: serde_json::Map<String, serde_json::Value>;
        let body: &serde_json::Map<String, serde_json::Value> = if needs_owned {
            body_owned = body.clone();
            for col in &self.meta.fields {
                if col.noform {
                    body_owned.remove(&col.name);
                }
            }
            crate::orm::write::apply_slug_from(&self.meta.fields, &mut body_owned, true);
            &body_owned
        } else {
            body
        };

        // Phase 0 — pre-DB validation. Update-shape: required-
        // field check only complains about EXPLICIT blanks
        // (preserving the partial-update contract); FK existence
        // + choices + M2M shape apply to whatever the body
        // carries.
        let validation_errors = crate::orm::validation::validate_on_update(&self.meta, body).await;
        if !validation_errors.is_empty() {
            return Err(WriteError::Multiple {
                errors: validation_errors,
            });
        }

        let mut q = Query::update();
        q.table(Alias::new(&self.meta.table));
        let mut any = false;
        for col in &self.meta.fields {
            if col.primary_key {
                continue;
            }
            let Some(json) = body.get(&col.name) else {
                // BUG-5 fix: `auto_now` columns refresh to
                // `Utc::now()` on every update, even if the body
                // doesn't mention them. `auto_now_add` columns
                // stay frozen (they fired on create only).
                if col.auto_now {
                    let now_value = crate::orm::write::now_for_column(col.ty);
                    q.value(Alias::new(&col.name), now_value);
                    any = true;
                }
                continue;
            };
            // IMP-3: same min/max pre-validation as insert_json.
            if let Some(n) = json.as_i64() {
                if let Some(min) = col.min {
                    if n < min {
                        return Err(WriteError::Validator {
                            field: col.name.clone(),
                            message: format!("must be >= {min} (got {n})."),
                        });
                    }
                }
                if let Some(max) = col.max {
                    if n > max {
                        return Err(WriteError::Validator {
                            field: col.name.clone(),
                            message: format!("must be <= {max} (got {n})."),
                        });
                    }
                }
            }
            // BUG-11/12/13: same wrapper-type pre-validation as
            // insert_json.
            if let (Some(fmt), Some(s)) = (col.text_format.as_deref(), json.as_str()) {
                if let Err(e) = crate::orm::validators::validate_text_format(fmt, s) {
                    return Err(WriteError::Validator {
                        field: col.name.clone(),
                        message: e.to_string(),
                    });
                }
            }
            let sea_value =
                crate::orm::write::json_to_sea_value(col.ty, json, col.nullable, &col.name)?;
            q.value(Alias::new(&col.name), sea_value);
            any = true;
        }
        // Detect whether the body wants to touch any M2M
        // relations. If so, we'll write junctions *after* the
        // UPDATE — and we'll need to know the matched parent
        // PKs even when no regular columns are being changed.
        let touches_m2m = self
            .meta
            .m2m_relations
            .iter()
            .any(|r| body.contains_key(&r.field_name));
        if !any && !touches_m2m {
            return Ok(0);
        }
        for cond in &self.where_clauses {
            q.cond_where(cond.clone());
        }
        // Find every parent_id matched by the filter so we can
        // mirror the M2M arrays into each one's junction. Done
        // BEFORE the UPDATE so a no-op (any = false, touches_m2m
        // = true) still gets the M2M write.
        let parent_pks: Vec<serde_json::Value> = if touches_m2m {
            match self.meta.pk_column() {
                Some(pk_col) => collect_parent_pks(&self.meta, pk_col, &self.where_clauses).await?,
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };

        match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                if any {
                    let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                    sqlx::query_with(&sql, values)
                        .execute(pool)
                        .await
                        .map_err(|e| classify_or_sqlx(e, body))?;
                }
                for pk in &parent_pks {
                    write_m2m_junctions(&self.meta, Some(pk), body).await?;
                }
                Ok(parent_pks.len().max(if any { 1 } else { 0 }) as u64)
            }
            DbPool::Postgres(pool) => {
                if any {
                    let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                    sqlx::query_with(&sql, values)
                        .execute(pool)
                        .await
                        .map_err(|e| classify_or_sqlx(e, body))?;
                }
                for pk in &parent_pks {
                    write_m2m_junctions(&self.meta, Some(pk), body).await?;
                }
                Ok(parent_pks.len().max(if any { 1 } else { 0 }) as u64)
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
            SqlType::Bytes => row
                .try_get::<Option<Vec<u8>>, _>(name)?
                .map_or(String::new(), |b| hex_encode(&b)),
            SqlType::Decimal => panic_pg_only_unsupported(&col.name),
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
        SqlType::Bytes => hex_encode(&row.try_get::<Vec<u8>, _>(name)?),
        SqlType::Decimal => panic_pg_only_unsupported(&col.name),
    })
}

/// Decode one Postgres cell to its template-friendly string form.
///
/// Sibling of [`decode_to_string`] for the Postgres backend. Same
/// dispatch table on `SqlType`; the only difference is the executor
/// type (`PgRow` instead of `SqliteRow`) and a handful of types that
/// Postgres binds differently — `i32` for SmallInt instead of SQLite's
/// affinity-coerced `i32`, native bool, native chrono / uuid /
/// serde_json::Value. Array / Inet / Cidr / MacAddr / FullText all
/// live on Postgres natively but are decoded as their JSON string
/// shape here (the admin templates only need a printable form).
pub fn decode_pg_to_string(
    row: &sqlx::postgres::PgRow,
    col: &Column,
) -> Result<String, sqlx::Error> {
    use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
    use serde_json::Value;
    use uuid::Uuid;

    let name = col.name.as_str();
    if col.nullable {
        return Ok(match col.ty {
            SqlType::SmallInt => row
                .try_get::<Option<i16>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Integer => row
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
            // Array / network / FullText decode as their printable forms.
            // Pg drivers hand back typed Vec / IpNetwork / etc.; we lift
            // through a best-effort string decode for now since the admin
            // only needs a glance. Decode failures fall through to empty
            // string (the admin still renders something useful).
            SqlType::Array(_)
            | SqlType::Inet
            | SqlType::Cidr
            | SqlType::MacAddr
            | SqlType::FullText => row
                .try_get::<Option<String>, _>(name)
                .ok()
                .flatten()
                .unwrap_or_default(),
            SqlType::ForeignKey => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Bytes => row
                .try_get::<Option<Vec<u8>>, _>(name)?
                .map_or(String::new(), |b| hex_encode(&b)),
            SqlType::Decimal => panic_pg_only_unsupported(&col.name),
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt => row.try_get::<i16, _>(name)?.to_string(),
        SqlType::Integer => row.try_get::<i32, _>(name)?.to_string(),
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
        // Same as the nullable branch: lift through best-effort string.
        SqlType::Array(_)
        | SqlType::Inet
        | SqlType::Cidr
        | SqlType::MacAddr
        | SqlType::FullText => row.try_get::<String, _>(name).unwrap_or_default(),
        SqlType::ForeignKey => row.try_get::<i64, _>(name)?.to_string(),
        SqlType::Bytes => hex_encode(&row.try_get::<Vec<u8>, _>(name)?),
        SqlType::Decimal => panic_pg_only_unsupported(&col.name),
    })
}

/// Decode one SQLite cell to a `serde_json::Value` that preserves the
/// column's JSON shape (numbers stay numbers, booleans stay booleans,
/// dates render as ISO strings, JSON columns nest verbatim, NULLs
/// become `Value::Null`). This is the row → JSON converter the REST
/// plugin's auto-CRUD list / detail handlers feed straight into their
/// HTTP body.
pub fn decode_to_json(
    row: &sqlx::sqlite::SqliteRow,
    col: &Column,
) -> Result<serde_json::Value, sqlx::Error> {
    use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
    use serde_json::Value;
    use uuid::Uuid;

    let name = col.name.as_str();
    if col.nullable {
        return Ok(match col.ty {
            SqlType::SmallInt | SqlType::Integer => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::BigInt => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Real => row
                .try_get::<Option<f32>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v as f64)),
            SqlType::Double => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Boolean => row
                .try_get::<Option<bool>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Text => row
                .try_get::<Option<String>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Date => row
                .try_get::<Option<NaiveDate>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Time => row
                .try_get::<Option<NaiveTime>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Timestamptz => row
                .try_get::<Option<DateTime<Utc>>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_rfc3339())),
            SqlType::Uuid => row
                .try_get::<Option<Uuid>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Json => row
                .try_get::<Option<Value>, _>(name)?
                .unwrap_or(Value::Null),
            SqlType::Array(_) => panic_array_unsupported(&col.name),
            SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
                panic_pg_only_unsupported(&col.name)
            }
            SqlType::ForeignKey => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Bytes => row
                .try_get::<Option<Vec<u8>>, _>(name)?
                .map_or(Value::Null, |b| bytes_to_json(&b)),
            SqlType::Decimal => panic_pg_only_unsupported(&col.name),
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => Value::from(row.try_get::<i32, _>(name)?),
        SqlType::BigInt => Value::from(row.try_get::<i64, _>(name)?),
        SqlType::Real => Value::from(row.try_get::<f32, _>(name)? as f64),
        SqlType::Double => Value::from(row.try_get::<f64, _>(name)?),
        SqlType::Boolean => Value::from(row.try_get::<bool, _>(name)?),
        SqlType::Text => Value::from(row.try_get::<String, _>(name)?),
        SqlType::Date => Value::from(row.try_get::<NaiveDate, _>(name)?.to_string()),
        SqlType::Time => Value::from(row.try_get::<NaiveTime, _>(name)?.to_string()),
        SqlType::Timestamptz => Value::from(row.try_get::<DateTime<Utc>, _>(name)?.to_rfc3339()),
        SqlType::Uuid => Value::from(row.try_get::<Uuid, _>(name)?.to_string()),
        SqlType::Json => row.try_get::<Value, _>(name)?,
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        SqlType::ForeignKey => Value::from(row.try_get::<i64, _>(name)?),
        SqlType::Bytes => bytes_to_json(&row.try_get::<Vec<u8>, _>(name)?),
        SqlType::Decimal => panic_pg_only_unsupported(&col.name),
    })
}

/// Postgres sibling of [`decode_to_json`]. Same dispatch table; the
/// only difference is the executor type (`PgRow`) and the i16 path
/// for SmallInt (PG binds i16, SQLite affinity-coerces to i32).
pub fn decode_pg_to_json(
    row: &sqlx::postgres::PgRow,
    col: &Column,
) -> Result<serde_json::Value, sqlx::Error> {
    use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
    use serde_json::Value;
    use uuid::Uuid;

    let name = col.name.as_str();
    if col.nullable {
        return Ok(match col.ty {
            SqlType::SmallInt => row
                .try_get::<Option<i16>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Integer => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::BigInt => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Real => row
                .try_get::<Option<f32>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v as f64)),
            SqlType::Double => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Boolean => row
                .try_get::<Option<bool>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Text => row
                .try_get::<Option<String>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Date => row
                .try_get::<Option<NaiveDate>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Time => row
                .try_get::<Option<NaiveTime>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Timestamptz => row
                .try_get::<Option<DateTime<Utc>>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_rfc3339())),
            SqlType::Uuid => row
                .try_get::<Option<Uuid>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Json => row
                .try_get::<Option<Value>, _>(name)?
                .unwrap_or(Value::Null),
            SqlType::Array(_)
            | SqlType::Inet
            | SqlType::Cidr
            | SqlType::MacAddr
            | SqlType::FullText => row
                .try_get::<Option<String>, _>(name)
                .ok()
                .flatten()
                .map_or(Value::Null, Value::from),
            SqlType::ForeignKey => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Bytes => row
                .try_get::<Option<Vec<u8>>, _>(name)?
                .map_or(Value::Null, |b| bytes_to_json(&b)),
            SqlType::Decimal => panic_pg_only_unsupported(&col.name),
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt => Value::from(row.try_get::<i16, _>(name)?),
        SqlType::Integer => Value::from(row.try_get::<i32, _>(name)?),
        SqlType::BigInt => Value::from(row.try_get::<i64, _>(name)?),
        SqlType::Real => Value::from(row.try_get::<f32, _>(name)? as f64),
        SqlType::Double => Value::from(row.try_get::<f64, _>(name)?),
        SqlType::Boolean => Value::from(row.try_get::<bool, _>(name)?),
        SqlType::Text => Value::from(row.try_get::<String, _>(name)?),
        SqlType::Date => Value::from(row.try_get::<NaiveDate, _>(name)?.to_string()),
        SqlType::Time => Value::from(row.try_get::<NaiveTime, _>(name)?.to_string()),
        SqlType::Timestamptz => Value::from(row.try_get::<DateTime<Utc>, _>(name)?.to_rfc3339()),
        SqlType::Uuid => Value::from(row.try_get::<Uuid, _>(name)?.to_string()),
        SqlType::Json => row.try_get::<Value, _>(name)?,
        SqlType::Array(_)
        | SqlType::Inet
        | SqlType::Cidr
        | SqlType::MacAddr
        | SqlType::FullText => row
            .try_get::<String, _>(name)
            .map(Value::from)
            .unwrap_or(Value::Null),
        SqlType::ForeignKey => Value::from(row.try_get::<i64, _>(name)?),
        SqlType::Bytes => bytes_to_json(&row.try_get::<Vec<u8>, _>(name)?),
        SqlType::Decimal => panic_pg_only_unsupported(&col.name),
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

/// Hex-encode a byte slice, lowercase, no `0x` prefix. The
/// human-readable rendering for `SqlType::Bytes` columns when the
/// admin / debug tooling asks for a string form.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Render bytes as a JSON array of u8 numbers. Symmetric with the
/// `json_to_sea_value` path that accepts the same shape on input.
fn bytes_to_json(bytes: &[u8]) -> serde_json::Value {
    serde_json::Value::Array(bytes.iter().map(|b| serde_json::Value::from(*b)).collect())
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

/// Classify a sqlx error from an `insert_json` / `update_json`
/// SQL execution into a structured `WriteError`. Constraint
/// failures are body-aware (the original JSON value is threaded
/// into the message); unknown errors fall through to
/// `WriteError::Sqlx` and the REST layer renders them as a 500.
fn classify_or_sqlx(
    e: sqlx::Error,
    body: &serde_json::Map<String, serde_json::Value>,
) -> crate::orm::write::WriteError {
    if let Some(classified) = crate::orm::validation::classify_sql_error(&e, body) {
        return classified;
    }
    crate::orm::write::WriteError::Sqlx(e)
}

/// Convert a JSON PK-shaped value (number or string) into a
/// `sea_query::Value` usable as a junction-table binding. Returns
/// `None` for shapes we don't know how to bind (arrays, objects,
/// booleans) — those won't reach here because
/// `validate_m2m_relations` rejects them upstream.
fn json_pk_to_sea(v: &serde_json::Value) -> Option<sea_query::Value> {
    match v {
        serde_json::Value::Number(n) => n.as_i64().map(|i| sea_query::Value::BigInt(Some(i))),
        serde_json::Value::String(s) => Some(sea_query::Value::String(Some(Box::new(s.clone())))),
        _ => None,
    }
}

/// Read every M2M relation off its junction table and attach
/// the resulting `child_id` arrays to `out` under each relation's
/// field name. Called from `insert_json` / `update_json`'s read-
/// back path so the response JSON includes the relations the
/// caller just wrote (otherwise the `tags: [1, 2]` they POSTed
/// would never appear in the response, since `M2M<T>` is
/// `#[serde(skip)]` on the parent struct).
async fn hydrate_m2m_into(
    meta: &crate::migrate::ModelMeta,
    parent_pk_json: Option<&serde_json::Value>,
    out: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), sqlx::Error> {
    if meta.m2m_relations.is_empty() {
        return Ok(());
    }
    let Some(parent_pk_value) = parent_pk_json.and_then(json_pk_to_sea) else {
        return Ok(());
    };
    for rel in &meta.m2m_relations {
        let junction_table = format!("{}_{}", meta.table, rel.field_name);
        let mut sel = Query::select();
        sel.from(Alias::new(&junction_table));
        sel.column(Alias::new("child_id"));
        sel.and_where(Expr::col(Alias::new("parent_id")).eq(parent_pk_value.clone()));
        let children: Vec<serde_json::Value> = match pool_dispatched() {
            DbPool::Sqlite(pool) => {
                let (sql, values) = sel.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
                rows.iter()
                    .map(|r| {
                        r.try_get::<i64, _>("child_id")
                            .map(|i| serde_json::Value::Number(i.into()))
                            .or_else(|_| {
                                r.try_get::<String, _>("child_id")
                                    .map(serde_json::Value::String)
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = sel.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
                rows.iter()
                    .map(|r| {
                        r.try_get::<i64, _>("child_id")
                            .map(|i| serde_json::Value::Number(i.into()))
                            .or_else(|_| {
                                r.try_get::<String, _>("child_id")
                                    .map(serde_json::Value::String)
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        out.insert(rel.field_name.clone(), serde_json::Value::Array(children));
    }
    Ok(())
}

/// Run `SELECT <pk> FROM <table> WHERE <conds>` to find every
/// row the dynamic UPDATE would touch. Returns each matched PK
/// as the raw JSON value the parent table holds — number for
/// integer PKs, string for UUID / String PKs. Used by
/// `update_json` so we know which junction-table parent_ids
/// to write to even when the body has no regular column changes.
async fn collect_parent_pks(
    meta: &crate::migrate::ModelMeta,
    pk_col: &crate::migrate::Column,
    where_clauses: &[Condition],
) -> Result<Vec<serde_json::Value>, crate::orm::write::WriteError> {
    let mut sel = Query::select();
    sel.from(Alias::new(&meta.table));
    sel.column(Alias::new(&pk_col.name));
    for cond in where_clauses {
        sel.cond_where(cond.clone());
    }
    match pool_dispatched() {
        DbPool::Sqlite(pool) => {
            let (sql, values) = sel.build_sqlx(SqliteQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
            rows.iter()
                .map(|row| decode_to_json(row, pk_col))
                .collect::<Result<Vec<_>, _>>()
                .map_err(crate::orm::write::WriteError::Sqlx)
        }
        DbPool::Postgres(pool) => {
            let (sql, values) = sel.build_sqlx(PostgresQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(pool).await?;
            rows.iter()
                .map(|row| decode_pg_to_json(row, pk_col))
                .collect::<Result<Vec<_>, _>>()
                .map_err(crate::orm::write::WriteError::Sqlx)
        }
    }
}

/// Mirror each M2M field in `body` into its junction table for
/// the given parent PK. Validation has already confirmed array
/// shape + child existence, so this is a straight write —
/// `set_junction_dynamic` wipes any existing rows for the
/// parent and re-inserts the supplied ids inside a transaction.
///
/// `parent_pk_json` is the JSON value the parent row holds at
/// its PK column (read straight off the post-INSERT row). When
/// it's `None` or unparseable we silently skip — there's nothing
/// to anchor the junction to.
async fn write_m2m_junctions(
    meta: &crate::migrate::ModelMeta,
    parent_pk_json: Option<&serde_json::Value>,
    body: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), crate::orm::write::WriteError> {
    if meta.m2m_relations.is_empty() {
        return Ok(());
    }
    let Some(parent_pk_value) = parent_pk_json.and_then(json_pk_to_sea) else {
        return Ok(());
    };
    for rel in &meta.m2m_relations {
        let Some(value) = body.get(&rel.field_name) else {
            continue;
        };
        let Some(items) = value.as_array() else {
            continue; // shape was validated upstream
        };
        let mut child_ids: Vec<sea_query::Value> = Vec::with_capacity(items.len());
        for item in items {
            if item.is_null() {
                continue;
            }
            if let Some(v) = json_pk_to_sea(item) {
                child_ids.push(v);
            }
        }
        let junction_table = format!("{}_{}", meta.table, rel.field_name);
        crate::orm::m2m::set_junction_dynamic(&junction_table, parent_pk_value.clone(), child_ids)
            .await
            .map_err(crate::orm::write::WriteError::Sqlx)?;
    }
    Ok(())
}
