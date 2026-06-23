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

use crate::db::{DbPool, pool_for_dispatched};
use crate::migrate::{Column, ModelMeta};
use crate::orm::SqlType;
use crate::orm::write::{WriteError, json_to_sea_value, null_for};

/// Resolve the pool for a dynamic (late-bound) query on `meta`, routing
/// through the `DatabaseRouter` exactly like the typed path.
fn resolve_pool_dyn(meta: &crate::migrate::ModelMeta, op: crate::db::RouteOp) -> crate::db::DbPool {
    let ctx = crate::db::route_context();
    let r = crate::db::router::router();
    let alias = match op {
        crate::db::RouteOp::Read => r.db_for_read(meta, &ctx),
        crate::db::RouteOp::Write => r.db_for_write(meta, &ctx),
    };
    pool_for_dispatched(alias.as_str()).clone()
}

/// Errors a runtime-typed query can produce.
///
/// Carries the structured [`WriteError`] when the failure originates
/// in the umbra write-validator (form-coercion failures, required-
/// field misses, future per-field validation), and bare
/// [`sqlx::Error`] otherwise — DB-driver failures, constraint
/// violations the validator can't pre-detect, connection drops.
///
/// gaps2 #12: prior to this change `DynError` was a bare alias for
/// `sqlx::Error`, so every `WriteError` that flowed through the
/// `DynQuerySet` form path was flattened to
/// `sqlx::Error::Protocol("umbra::orm::write: <message>")` and the
/// per-field map (`field_errors()` / `non_field_errors()`) was lost
/// before the admin handler could render it. The enum preserves the
/// structure all the way to the response surface; the admin's
/// per-field rendering work (gaps2 #12 part 2) and the `Form<T>`
/// extractor (gaps2 #19) both consume it directly.
///
/// The two-arm shape composes with `?` ergonomically because both
/// `sqlx::Error` and `WriteError` lift via `From` — handlers can
/// keep their existing `?` chains and reach for `match` only at the
/// boundary where the per-field map is rendered.
#[derive(Debug)]
pub enum DynError {
    /// Structured umbra-validator failure (per-field errors,
    /// validator rules, FK / unique violations the validator
    /// pre-detected). The carried [`WriteError`] keeps its
    /// `field_errors()` / `non_field_errors()` accessors.
    Write(WriteError),
    /// Bare sqlx failure (driver-level error, connection drop,
    /// constraint violation the validator didn't catch). Surface
    /// the message via [`sqlx::Error`]'s own `Display`.
    Sqlx(sqlx::Error),
}

impl std::fmt::Display for DynError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Write(e) => write!(f, "{e}"),
            Self::Sqlx(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DynError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Write(e) => Some(e),
            Self::Sqlx(e) => Some(e),
        }
    }
}

impl From<sqlx::Error> for DynError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl From<WriteError> for DynError {
    fn from(e: WriteError) -> Self {
        Self::Write(e)
    }
}

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
    with_deleted: bool,
    only_deleted: bool,
    hard_delete: bool,
    /// FK column names to expand via a batched `IN (...)` lookup
    /// after the main query — same one-hop semantics as the typed
    /// `QuerySet::select_related`. Each entry must be a single-hop
    /// FK column on `meta` (validated when added). When non-empty,
    /// `fetch_as_json` / `first_as_json` swap the FK integer values
    /// in the response for the full related-row JSON object.
    /// Drives the REST plugin's `?include=fk1,fk2` query param.
    select_related: Vec<String>,
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
            with_deleted: false,
            only_deleted: false,
            hard_delete: false,
            select_related: Vec::new(),
        }
    }

    /// Include soft-deleted rows for models tagged with
    /// `#[umbra(soft_delete)]`.
    pub fn with_deleted(mut self) -> Self {
        self.with_deleted = true;
        self
    }

    /// Restrict a soft-delete model to only rows whose `deleted_at` is
    /// populated.
    pub fn only_deleted(mut self) -> Self {
        self.only_deleted = true;
        self
    }

    /// Force a real `DELETE` for a soft-delete model.
    pub fn hard_delete(mut self) -> Self {
        self.hard_delete = true;
        self
    }

    fn effective_where_clauses(&self) -> Vec<Condition> {
        let mut clauses = self.where_clauses.clone();
        if self.meta.soft_delete {
            if self.only_deleted {
                clauses
                    .push(Condition::all().add(Expr::col(Alias::new("deleted_at")).is_not_null()));
            } else if !self.with_deleted {
                clauses.push(Condition::all().add(Expr::col(Alias::new("deleted_at")).is_null()));
            }
        }
        clauses
    }

    fn live_where_clauses(&self) -> Vec<Condition> {
        let mut clauses = self.where_clauses.clone();
        if self.meta.soft_delete {
            clauses.push(Condition::all().add(Expr::col(Alias::new("deleted_at")).is_null()));
        }
        clauses
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

    /// Expand the named FK columns via a batched `IN (...)` lookup
    /// after the main query — mirrors the typed
    /// `QuerySet::select_related` shape (single-hop and `__`-chained
    /// alike). After this call, every FK field along the chain in
    /// the response JSON renders as the full related-row object
    /// instead of the raw integer id. Query budget is
    /// `1 + len(hops)` per chain regardless of how many parent rows
    /// came back (no N+1) — gap2 #18.
    ///
    /// Names may use either `.` (URL-natural) or `__` (Django
    /// muscle-memory) as the hop separator; both normalize to the
    /// same canonical chain internally. Mixed separators in one
    /// token (e.g. `author.profile__org`) are accepted too.
    ///
    /// Names that don't exist on the model OR aren't FK columns at
    /// any hop are silently dropped — the REST plugin's `?include=`
    /// handler does its own up-front validation with a 400 on
    /// unknown names, so stale dynamic includes (e.g. an internal
    /// call site that hardcoded a name that was later renamed) just
    /// skip without crashing the request.
    ///
    /// ```ignore
    /// DynQuerySet::for_meta(&meta)
    ///     .select_related_dyn(&["user".into(), "author.profile".into()])
    ///     .fetch_as_json().await
    /// ```
    pub fn select_related_dyn(mut self, fields: &[String]) -> Self {
        for name in fields {
            let canonical = normalize_sr_token(name);
            if validate_sr_chain(self.meta, &canonical).is_none() {
                continue;
            }
            if !self.select_related.iter().any(|n| n == &canonical) {
                self.select_related.push(canonical);
            }
        }
        self
    }

    /// Read-side accessor for the resolved select_related list.
    /// Used by tests + the REST handler's debug-logging path.
    #[doc(hidden)]
    pub fn select_related_fields(&self) -> &[String] {
        &self.select_related
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
        // Escape LIKE wildcards in the user's term so `%`/`_` are matched
        // literally, not as wildcards (ORM-1). Paired with `.escape('\\')`.
        let like_pat = format!("%{}%", crate::orm::escape_like_literal(term)).to_uppercase();

        let mut cond = Condition::any();
        let mut added = 0;
        for col in &self.meta.fields {
            if restricted && !fields.iter().any(|f| f == &col.name) {
                continue;
            }
            let predicate: Option<sea_query::SimpleExpr> = match col.ty {
                SqlType::Text => Some(
                    Expr::expr(Func::upper(Expr::col(Alias::new(&col.name))))
                        .like(sea_query::LikeExpr::new(like_pat.clone()).escape('\\')),
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

    /// Filter the parent set down to rows that have an M2M link to at
    /// least one of `child_ids` through the named M2M field. Emits:
    ///
    /// ```sql
    /// WHERE <pk> IN (
    ///     SELECT parent_id FROM <parent_table>_<field_name>
    ///     WHERE child_id IN (?, ?, ...)
    /// )
    /// ```
    ///
    /// The junction table name follows the framework's
    /// `{parent_table}_{field_name}` convention (same as
    /// `set_junction_dynamic` and the migration emitter use). Returns
    /// `self` unchanged when:
    ///   - `child_ids` is empty,
    ///   - no M2M relation with that `field_name` exists on the model,
    ///   - the parent model has no PK column,
    ///   - every value in `child_ids` fails to parse as `i64`
    ///     (M2M PKs are i64 at v1 across the framework).
    ///
    /// Use case: admin filter for "products with tag 1 OR tag 2 OR
    /// tag 3" — call once with all three child ids; the IN subquery
    /// is one round-trip regardless of selection count.
    pub fn filter_m2m_contains_any(mut self, field_name: &str, child_ids: &[String]) -> Self {
        if child_ids.is_empty() {
            return self;
        }
        let Some(rel) = self
            .meta
            .m2m_relations
            .iter()
            .find(|r| r.field_name == field_name)
        else {
            return self;
        };
        let Some(pk_col) = self.meta.pk_column() else {
            return self;
        };
        // PK lift Pass B: bind child ids per the M2M target's PK
        // type, not always i64. Pre-fix, `permissions_permission`
        // (whose PK is the `codename` String column) couldn't be
        // filtered via this method because every string id parsed
        // as `i64::Err` and got dropped. The junction table's
        // `child_id` column type matches the target's PK type at
        // DDL emission, so binding correctly here keeps SQLite +
        // Postgres affinity happy.
        // PK lift Pass E: cached lookup. Previously cloned the full
        // model registry per `filter_m2m_contains_any` call.
        let target_pk_ty = crate::migrate::pk_meta_for_table(&rel.target_table)
            .map(|(_, ty)| ty)
            .unwrap_or(SqlType::BigInt);
        let junction_table = format!("{}_{}", self.meta.table, rel.field_name);
        let child_id_expr = Expr::col(Alias::new("child_id"));
        let in_clause: sea_query::SimpleExpr = match target_pk_ty {
            SqlType::Text | SqlType::Uuid => {
                // String / UUID PK: bind raw strings. Empty / all-
                // whitespace tokens drop out (no realistic PK is
                // blank); everything else goes in verbatim.
                let bound: Vec<String> = child_ids
                    .iter()
                    .filter_map(|s| {
                        let s = s.trim();
                        if s.is_empty() {
                            None
                        } else {
                            Some(s.to_string())
                        }
                    })
                    .collect();
                if bound.is_empty() {
                    return self;
                }
                child_id_expr.is_in(bound)
            }
            _ => {
                // Integer-PK target (default): parse to i64. Same
                // behaviour as pre-fix; this arm matches the
                // pre-existing semantics for every shipped model.
                let parsed: Vec<i64> = child_ids.iter().filter_map(|s| s.parse().ok()).collect();
                if parsed.is_empty() {
                    return self;
                }
                child_id_expr.is_in(parsed)
            }
        };
        let subq = Query::select()
            .column(Alias::new("parent_id"))
            .from(crate::db::router::schema_qualified_table(&junction_table))
            .and_where(in_clause)
            .to_owned();
        let cond =
            Condition::all().add(Expr::col(Alias::new(pk_col.name.clone())).in_subquery(subq));
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
        // type system) accepts the parameter. `fk_effective_type` resolves
        // a ForeignKey to its target's PK type, so an FK to a String/Uuid
        // target binds the raw string (via the `_` arm) instead of being
        // parsed as i64 and dropped.
        let cond = match crate::migrate::fk_effective_type(meta_col) {
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
            // UUIDs are stored as BLOB in SQLite (sqlx Encode<Sqlite> for Uuid
            // uses .as_bytes()). Binding them as strings would miss every row.
            // Parse each submitted string into a Uuid and pass the typed vec so
            // sea-query-binder emits blob binds that match the stored values.
            SqlType::Uuid => {
                let parsed: Vec<uuid::Uuid> = vals
                    .iter()
                    .filter_map(|s| uuid::Uuid::parse_str(s).ok())
                    .collect();
                if parsed.is_empty() {
                    return self;
                }
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
        // FK-to-non-i64-target columns resolve to their target PK type, so
        // a String/Uuid FK matches the `_` arm and binds the raw string.
        let predicate = match crate::migrate::fk_effective_type(meta_col) {
            SqlType::SmallInt | SqlType::Integer => value.parse::<i32>().ok().map(|v| expr.eq(v)),
            SqlType::BigInt | SqlType::ForeignKey => value.parse::<i64>().ok().map(|v| expr.eq(v)),
            SqlType::Real | SqlType::Double => value.parse::<f64>().ok().map(|v| expr.eq(v)),
            SqlType::Boolean => {
                let v = matches!(value, "true" | "on" | "1");
                Some(expr.eq(v))
            }
            // UUIDs stored as BLOB in SQLite — parse the string into a typed
            // Uuid so sea-query-binder emits a blob bind that matches the row.
            SqlType::Uuid => uuid::Uuid::parse_str(value).ok().map(|u| expr.eq(u)),
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
        q.from(crate::db::router::schema_qualified_table(&self.meta.table));
        q.expr(Func::count(Expr::col(Asterisk)));
        let where_clauses = self.effective_where_clauses();
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }

        match resolve_pool_dyn(self.meta, crate::db::RouteOp::Read) {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let row = sqlx::query_with(&sql, values).fetch_one(&pool).await?;
                Ok(row.try_get::<i64, _>(0)?)
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let row = sqlx::query_with(&sql, values).fetch_one(&pool).await?;
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
        q.from(crate::db::router::schema_qualified_table(&self.meta.table));
        q.column(Alias::new(col));
        let where_clauses = self.effective_where_clauses();
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }
        if let Some(n) = self.limit {
            q.limit(n);
        }

        match resolve_pool_dyn(self.meta, crate::db::RouteOp::Read) {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    out.push(decode_to_string(&row, col_meta)?);
                }
                Ok(out)
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
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
    ///
    /// gaps #77: pre-collects the affected PKs (one extra SELECT per
    /// call) before the DELETE so `bulk_post_delete:<table>` can fire
    /// with the actual row ids. Subscribers that need to invalidate
    /// caches / write audit-log rows / sync a search index get the
    /// list of PKs that just left the table, not just a row count.
    pub async fn delete(self) -> Result<u64, DynError> {
        if self.meta.soft_delete && !self.hard_delete {
            return self.soft_delete_update().await;
        }
        let where_clauses = self.effective_where_clauses();
        // Pre-collect the affected PKs only when the model has a PK
        // column (every Model does in practice; the guard handles
        // the hypothetical PK-less ModelMeta).
        let parent_pks: Vec<serde_json::Value> = match self.meta.pk_column() {
            Some(pk_col) => collect_parent_pks(&self.meta, pk_col, &where_clauses)
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };

        let mut q = Query::delete();
        q.from_table(crate::db::router::schema_qualified_table(&self.meta.table));
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }

        let rows_affected = match resolve_pool_dyn(self.meta, crate::db::RouteOp::Write) {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
                res.rows_affected()
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
                res.rows_affected()
            }
        };

        // gaps #77: emit `bulk_post_delete:<table>` with the PKs we
        // captured pre-DELETE. Fires even when zero rows matched —
        // matches the typed bulk-delete convention (subscribers that
        // want to skip empty events filter in their handler).
        crate::signals::emit_bulk_post_delete_by_table(&self.meta.table, parent_pks).await;
        Ok(rows_affected)
    }

    async fn soft_delete_update(self) -> Result<u64, DynError> {
        let where_clauses = self.live_where_clauses();
        let parent_pks: Vec<serde_json::Value> = match self.meta.pk_column() {
            Some(pk_col) => collect_parent_pks(self.meta, pk_col, &where_clauses)
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };

        let mut q = Query::update();
        q.table(crate::db::router::schema_qualified_table(&self.meta.table));
        q.value(
            Alias::new("deleted_at"),
            sea_query::Value::ChronoDateTimeUtc(Some(Box::new(chrono::Utc::now()))),
        );
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }

        let rows_affected = match resolve_pool_dyn(self.meta, crate::db::RouteOp::Write) {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
                res.rows_affected()
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
                res.rows_affected()
            }
        };

        crate::signals::emit_bulk_post_delete_by_table(&self.meta.table, parent_pks).await;
        Ok(rows_affected)
    }

    /// Terminal: undo a soft-delete — `UPDATE <table> SET deleted_at =
    /// NULL` for the rows matching the accumulated WHERE that are
    /// currently soft-deleted (`deleted_at IS NOT NULL`). Returns the
    /// number of rows restored. A no-op (0 rows) on a model that isn't
    /// tagged `soft_delete`, since there is no `deleted_at` column to
    /// clear — the caller should gate on `meta.soft_delete` first.
    ///
    /// This is the inverse of [`Self::delete`] on a soft-delete model:
    /// `delete()` stamps `deleted_at = now()`, `restore()` clears it.
    /// The admin's "Restore selected" trash action drives this.
    pub async fn restore(self) -> Result<u64, DynError> {
        if !self.meta.soft_delete {
            return Ok(0);
        }
        // Restrict to the rows the caller selected AND that are
        // actually trashed — restoring a live row is a no-op but
        // narrowing here keeps the affected-count honest.
        let mut where_clauses = self.where_clauses.clone();
        where_clauses
            .push(Condition::all().add(Expr::col(Alias::new("deleted_at")).is_not_null()));

        let parent_pks: Vec<serde_json::Value> = match self.meta.pk_column() {
            Some(pk_col) => collect_parent_pks(self.meta, pk_col, &where_clauses)
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };

        let mut q = Query::update();
        q.table(crate::db::router::schema_qualified_table(&self.meta.table));
        q.value(
            Alias::new("deleted_at"),
            sea_query::Value::ChronoDateTimeUtc(None),
        );
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }

        let rows_affected = match resolve_pool_dyn(self.meta, crate::db::RouteOp::Write) {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
                res.rows_affected()
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
                res.rows_affected()
            }
        };

        // Restoring a row is a "save" from the data model's POV — the
        // row re-enters the live set — so emit the bulk-post-save
        // signal, mirroring how soft-delete emits bulk-post-delete.
        crate::signals::emit_bulk_post_save_by_table(&self.meta.table, parent_pks, false).await;
        Ok(rows_affected)
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
            // gaps2 #12: per-field validator failure (see `update_form`).
            Err(e) => {
                return Err(DynError::Write(WriteError::Validator {
                    field: col_meta.name.clone(),
                    message: e.to_string(),
                }));
            }
        };

        let mut q = Query::update();
        q.table(crate::db::router::schema_qualified_table(&self.meta.table));
        q.value(Alias::new(col), sea_value);
        let where_clauses = self.effective_where_clauses();
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }

        match resolve_pool_dyn(self.meta, crate::db::RouteOp::Write) {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
                Ok(res.rows_affected())
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
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
        let Some(q) = self.build_update_form_query(form, skip)? else {
            return Ok(0);
        };

        match resolve_pool_dyn(self.meta, crate::db::RouteOp::Write) {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
                Ok(res.rows_affected())
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let res = sqlx::query_with(&sql, values).execute(&pool).await?;
                Ok(res.rows_affected())
            }
        }
    }

    /// Build the `UPDATE` statement (SET clauses + accumulated WHERE)
    /// for [`Self::update_form`] / [`Self::update_form_in_tx`]. Returns
    /// `None` when no column would be written (the callers translate
    /// that into a `0` return). Holds all per-field validation —
    /// PK/skip exclusion, `auto_now` refresh, and the structured
    /// [`WriteError::Validator`] — so the pool and transaction paths
    /// build provably the same statement.
    fn build_update_form_query(
        &self,
        form: &HashMap<String, String>,
        skip: &[String],
    ) -> Result<Option<sea_query::UpdateStatement>, DynError> {
        let mut q = Query::update();
        q.table(crate::db::router::schema_qualified_table(&self.meta.table));
        let mut any = false;
        for col in &self.meta.fields {
            if col.primary_key || skip.iter().any(|s| s == &col.name) {
                continue;
            }
            // `auto_now` columns refresh on every update — push
            // `Utc::now()` regardless of whether the form carried
            // the column. `auto_now_add` stays frozen on update
            // (fired once at INSERT time); it falls through to the
            // standard "form omitted → skip" path below. Mirrors
            // `update_json` (line ~1047) so form + JSON write paths
            // honor the annotation identically.
            if col.auto_now {
                q.value(
                    Alias::new(&col.name),
                    crate::orm::write::now_for_column(col.ty),
                );
                any = true;
                continue;
            }
            let Some(raw) = form.get(&col.name) else {
                continue;
            };
            let sea_value = match form_str_to_sea_value(col, raw) {
                Ok(v) => v,
                // gaps2 #12: emit a structured per-field validator
                // failure so the admin / Form<T> consumer can render
                // it under the offending input. The pre-fix path
                // flattened to `sqlx::Error::Protocol(...)` and the
                // per-field hint was lost.
                Err(e) => {
                    return Err(DynError::Write(WriteError::Validator {
                        field: col.name.clone(),
                        message: e.to_string(),
                    }));
                }
            };
            q.value(Alias::new(&col.name), sea_value);
            any = true;
        }
        if !any {
            return Ok(None);
        }
        let where_clauses = self.effective_where_clauses();
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }
        Ok(Some(q))
    }

    /// Transaction-aware sibling of [`Self::update_form`]. Builds and
    /// executes the identical `UPDATE` (same per-field validation,
    /// `skip` / PK exclusion, `auto_now` refresh, [`WriteError::Validator`]
    /// shape, and accumulated WHERE) but runs it on the caller-supplied
    /// `tx`. The caller owns `commit` / `rollback`, so the update is
    /// uncommitted until they say so — used by the admin to save a
    /// parent edit and its inline child changes atomically.
    pub async fn update_form_in_tx(
        self,
        tx: &mut crate::db::Transaction,
        form: &HashMap<String, String>,
        skip: &[String],
    ) -> Result<u64, DynError> {
        let Some(q) = self.build_update_form_query(form, skip)? else {
            return Ok(0);
        };

        match tx.backend_name() {
            "sqlite" => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let inner = tx.as_sqlite_mut().expect("sqlite backend_name");
                let res = sqlx::query_with(&sql, values).execute(&mut **inner).await?;
                Ok(res.rows_affected())
            }
            _ => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let inner = tx.as_pg_mut().expect("postgres backend_name");
                let res = sqlx::query_with(&sql, values).execute(&mut **inner).await?;
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
        let Some(mut q) = self.build_insert_form_query(form, skip)? else {
            return Ok(0);
        };

        match resolve_pool_dyn(self.meta, crate::db::RouteOp::Write) {
            DbPool::Sqlite(pool) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, vals).execute(&pool).await?;
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
                    let row = sqlx::query_with(&sql, vals).fetch_one(&pool).await?;
                    Ok(row.try_get::<i64, _>(pk.as_str()).unwrap_or(0))
                } else {
                    let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                    let _ = sqlx::query_with(&sql, vals).execute(&pool).await?;
                    Ok(0)
                }
            }
        }
    }

    /// Build the `INSERT` statement for [`Self::insert_form`] /
    /// [`Self::insert_form_in_tx`]. Returns `None` when no column
    /// survives the `skip` / auto-increment-PK filtering (the callers
    /// translate that into a `0` return). All per-field validation —
    /// auto-now/auto-now-add stamping, the auto-increment PK omission,
    /// and the structured [`WriteError::Validator`] on a bad value —
    /// lives here so the pool and transaction paths build provably the
    /// same statement.
    fn build_insert_form_query(
        &self,
        form: &HashMap<String, String>,
        skip: &[String],
    ) -> Result<Option<sea_query::InsertStatement>, DynError> {
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
            // `auto_now_add` / `auto_now` columns: when the form
            // omits the field (the post-fix admin shape — these
            // columns are hidden from create + edit forms), fill
            // with `Utc::now()` here. Mirrors the same handling on
            // `insert_json` (line ~836) so the form path and the
            // JSON path stay consistent — both honor the annotation
            // without the body / form having to carry the value.
            if (col.auto_now_add || col.auto_now)
                && form.get(&col.name).is_none_or(|v| v.is_empty())
            {
                cols.push(&col.name);
                values.push(crate::orm::write::now_for_column(col.ty));
                continue;
            }
            let raw = form.get(&col.name).map(|s| s.as_str()).unwrap_or("");
            let sea_value = match form_str_to_sea_value(col, raw) {
                Ok(v) => v,
                // gaps2 #12: structured per-field validator failure
                // (see the matching site in `update_form`).
                Err(e) => {
                    return Err(DynError::Write(WriteError::Validator {
                        field: col.name.clone(),
                        message: e.to_string(),
                    }));
                }
            };
            cols.push(&col.name);
            values.push(sea_value);
        }
        if cols.is_empty() {
            return Ok(None);
        }

        let mut q = Query::insert();
        q.into_table(crate::db::router::schema_qualified_table(&self.meta.table));
        q.columns(cols.iter().map(|c| Alias::new(*c)).collect::<Vec<_>>());
        let exprs: Vec<sea_query::SimpleExpr> = values.into_iter().map(Into::into).collect();
        q.values_panic(exprs);
        Ok(Some(q))
    }

    /// Transaction-aware sibling of [`Self::insert_form`]. Builds and
    /// executes the identical `INSERT` (same `form_str_to_sea_value`
    /// per-field validation, same `skip` / auto-increment-PK / auto-now
    /// handling, same [`WriteError::Validator`] shape, same returned-PK
    /// semantics) but runs it on the caller-supplied `tx` instead of a
    /// fresh pool connection. The caller owns `commit` / `rollback`, so
    /// the insert is uncommitted until they say so — this is what lets
    /// the admin save a parent row and its inline children atomically.
    pub async fn insert_form_in_tx(
        self,
        tx: &mut crate::db::Transaction,
        form: &HashMap<String, String>,
        skip: &[String],
    ) -> Result<i64, DynError> {
        let Some(mut q) = self.build_insert_form_query(form, skip)? else {
            return Ok(0);
        };

        match tx.backend_name() {
            "sqlite" => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let inner = tx.as_sqlite_mut().expect("sqlite backend_name");
                let res = sqlx::query_with(&sql, vals).execute(&mut **inner).await?;
                Ok(res.last_insert_rowid())
            }
            _ => {
                // Postgres has no last_insert_rowid; RETURNING the PK
                // mirrors the pool path exactly, including the `0`
                // fallback for a non-integer PK.
                let pk_name = self
                    .meta
                    .fields
                    .iter()
                    .find(|c| c.primary_key)
                    .map(|c| c.name.clone());
                let inner = tx.as_pg_mut().expect("postgres backend_name");
                if let Some(pk) = pk_name {
                    q.returning_col(Alias::new(&pk));
                    let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                    let row = sqlx::query_with(&sql, vals).fetch_one(&mut **inner).await?;
                    Ok(row.try_get::<i64, _>(pk.as_str()).unwrap_or(0))
                } else {
                    let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                    let _ = sqlx::query_with(&sql, vals).execute(&mut **inner).await?;
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
        q.from(crate::db::router::schema_qualified_table(&self.meta.table));
        for c in &self.select_cols {
            q.column(Alias::new(c));
        }
        let where_clauses = self.effective_where_clauses();
        for cond in &where_clauses {
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

        match resolve_pool_dyn(self.meta, crate::db::RouteOp::Read) {
            DbPool::Sqlite(pool) => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
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
                let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
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
        q.from(crate::db::router::schema_qualified_table(&self.meta.table));
        for c in &self.select_cols {
            q.column(Alias::new(c));
        }
        let where_clauses = self.effective_where_clauses();
        for cond in &where_clauses {
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
        let selected_cols: Vec<(&String, &Column)> = self
            .select_cols
            .iter()
            .filter_map(|col_name| {
                self.meta
                    .fields
                    .iter()
                    .find(|c| &c.name == col_name)
                    .map(|col| (col_name, col))
            })
            .collect();
        let mut out: Vec<serde_json::Map<String, serde_json::Value>> =
            match resolve_pool_dyn(self.meta, crate::db::RouteOp::Read) {
                DbPool::Sqlite(pool) => {
                    let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                    let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
                    let mut out: Vec<serde_json::Map<String, serde_json::Value>> =
                        Vec::with_capacity(rows.len());
                    for row in rows {
                        let mut entry = serde_json::Map::new();
                        for (col_name, col_meta) in &selected_cols {
                            entry.insert((*col_name).clone(), decode_to_json(&row, col_meta)?);
                        }
                        out.push(entry);
                    }
                    out
                }
                DbPool::Postgres(pool) => {
                    let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                    let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
                    let mut out: Vec<serde_json::Map<String, serde_json::Value>> =
                        Vec::with_capacity(rows.len());
                    for row in rows {
                        let mut entry = serde_json::Map::new();
                        for (col_name, col_meta) in &selected_cols {
                            entry.insert((*col_name).clone(), decode_pg_to_json(&row, col_meta)?);
                        }
                        out.push(entry);
                    }
                    out
                }
            };

        // M2M echo via one batched IN per relation across every
        // parent row in `out`. Replaces the per-row, per-relation
        // SELECT that ran inside the row loop above (gap2 #16) —
        // query budget drops from `1 + N*M` to `1 + count(M2M
        // relations)` regardless of how many parent rows came back.
        // Each row picks up its `<relation>: [child_id, ...]`
        // array via PK→children grouping, with an empty array
        // for parents that have no junction rows (preserves the
        // per-row helper's "always echo the key" contract).
        if !self.meta.m2m_relations.is_empty() && !out.is_empty() {
            hydrate_m2m_batched(&self.meta, &pk_name, &mut out).await?;
        }

        // FK expansion via select_related — one batched
        // `IN (...)` per requested FK after the main query, then
        // splice the resolved row's JSON in where the integer id
        // was. No N+1: each FK costs one round-trip regardless of
        // how many parent rows came back. Reuses the same
        // `fetch_related_as_json` helper that powers the typed
        // `QuerySet::select_related` path so SQLite + Postgres
        // dispatch stays in one place.
        if !self.select_related.is_empty() && !out.is_empty() {
            hydrate_select_related_into(&self.meta, &self.select_related, &mut out).await?;
        }
        Ok(out)
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

    /// Transaction-aware single-row read: `SELECT <cols> ... LIMIT 1` for
    /// the accumulated WHERE, run on the open `tx`. Decodes every model
    /// column into a JSON map. Used by REST bulk update to read a row back
    /// on the same (uncommitted) transaction so the response reflects the
    /// in-flight write. Returns `None` when the filter matches no row.
    ///
    /// Unlike [`Self::fetch_as_json`] this does NOT hydrate M2M arrays or
    /// `select_related` — it's the column-level read the bulk write path
    /// needs, matching what the single-object PATCH read-back returns.
    pub async fn fetch_one_json_in_tx(
        self,
        tx: &mut crate::db::Transaction,
    ) -> Result<Option<serde_json::Map<String, serde_json::Value>>, DynError> {
        let mut q = Query::select();
        q.from(crate::db::router::schema_qualified_table(&self.meta.table));
        for c in &self.meta.fields {
            q.column(Alias::new(&c.name));
        }
        let where_clauses = self.effective_where_clauses();
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }
        q.limit(1);

        let out = match tx.backend_name() {
            "sqlite" => {
                let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                let inner = tx.as_sqlite_mut().expect("sqlite backend_name");
                let row = sqlx::query_with(&sql, values)
                    .fetch_optional(&mut **inner)
                    .await?;
                match row {
                    Some(row) => {
                        let mut entry = serde_json::Map::new();
                        for col in &self.meta.fields {
                            entry.insert(col.name.clone(), decode_to_json(&row, col)?);
                        }
                        Some(entry)
                    }
                    None => None,
                }
            }
            _ => {
                let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                let inner = tx.as_pg_mut().expect("postgres backend_name");
                let row = sqlx::query_with(&sql, values)
                    .fetch_optional(&mut **inner)
                    .await?;
                match row {
                    Some(row) => {
                        let mut entry = serde_json::Map::new();
                        for col in &self.meta.fields {
                            entry.insert(col.name.clone(), decode_pg_to_json(&row, col)?);
                        }
                        Some(entry)
                    }
                    None => None,
                }
            }
        };
        Ok(out)
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
        use crate::orm::write::WriteError;

        // Phase -1 — normalise the body (strip `noform`, derive
        // `slug_from`). Shared with the tx path.
        let body_owned: serde_json::Map<String, serde_json::Value>;
        let body: &serde_json::Map<String, serde_json::Value> =
            match normalise_insert_body(self.meta, body) {
                Some(owned) => {
                    body_owned = owned;
                    &body_owned
                }
                None => body,
            };

        // Phase 0 — pre-DB validation against the ambient pool.
        let validation_errors = crate::orm::validation::validate_on_create(self.meta, body).await;
        if !validation_errors.is_empty() {
            return Err(WriteError::Multiple {
                errors: validation_errors,
            });
        }

        // Phase 1 — build the INSERT + read back the PK shape.
        // Shared with the tx path.
        let InsertPlan {
            mut q,
            pk_name,
            pk_ty,
        } = build_insert_plan(self.meta, body)?;

        // gaps #77: fire `pre_save:<table>` for the dynamic-write
        // path so REST endpoints and admin form submits surface in
        // signal subscribers (audit logs, cache invalidation, search
        // index sync). Payload mirrors the typed `Manager::create`
        // shape — `{ "instance": <body JSON>, "created": true }`.
        crate::signals::emit_pre_save_by_table(
            &self.meta.table,
            serde_json::Value::Object(body.clone()),
            true,
        )
        .await;

        match resolve_pool_dyn(self.meta, crate::db::RouteOp::Write) {
            DbPool::Sqlite(pool) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let res = sqlx::query_with(&sql, vals)
                    .execute(&pool)
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
                            pk_ty, &supplied, false, &pk_name, None,
                        )?;
                        Expr::col(Alias::new(&pk_name)).eq(sea_value)
                    }
                };
                let mut sel = Query::select();
                sel.from(crate::db::router::schema_qualified_table(&self.meta.table));
                for c in &self.meta.fields {
                    sel.column(Alias::new(&c.name));
                }
                sel.cond_where(Condition::all().add(pk_pred));
                let (sel_sql, sel_vals) = sel.build_sqlx(SqliteQueryBuilder);
                let row = sqlx::query_with(&sel_sql, sel_vals)
                    .fetch_one(&pool)
                    .await?;
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
                // gaps #77: post_save with the fully-hydrated row.
                crate::signals::emit_post_save_by_table(
                    &self.meta.table,
                    serde_json::Value::Object(out.clone()),
                    true,
                )
                .await;
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
                    .fetch_one(&pool)
                    .await
                    .map_err(|e| classify_or_sqlx(e, body))?;
                let mut out = serde_json::Map::new();
                for col in &self.meta.fields {
                    out.insert(col.name.clone(), decode_pg_to_json(&row, col)?);
                }
                let pk_value = out.get(&pk_name).cloned();
                write_m2m_junctions(&self.meta, pk_value.as_ref(), body).await?;
                hydrate_m2m_into(&self.meta, pk_value.as_ref(), &mut out).await?;
                // gaps #77: post_save on the Postgres branch.
                crate::signals::emit_post_save_by_table(
                    &self.meta.table,
                    serde_json::Value::Object(out.clone()),
                    true,
                )
                .await;
                Ok(out)
            }
        }
    }

    /// Terminal: INSERT one row from a JSON map ON the passed
    /// transaction. The transactional sibling of [`Self::insert_json`]:
    /// the INSERT, the PK re-fetch, the M2M junction writes, the M2M
    /// read-back, AND the FK-existence validation all execute on `tx`
    /// rather than the ambient pool — so a caller can insert a parent
    /// and its children on one transaction and have the whole set
    /// commit (or roll back) atomically (`planning/orm_fixes.md` #2).
    ///
    /// Validation runs against the open transaction
    /// ([`crate::orm::validation::validate_on_create_in_tx`]) so a
    /// child whose FK targets a parent inserted earlier on the same
    /// (uncommitted) `tx` resolves. This is what makes a true-atomic
    /// nested create possible without the old compensating-delete
    /// dance.
    ///
    /// **Signals.** Unlike the auto-commit path, this does NOT fire
    /// `pre_save` / `post_save`. The row isn't durable until the
    /// caller commits `tx`, and a subscriber (audit log, cache
    /// invalidation, search index) firing before commit could observe
    /// — or react to — a write that then rolls back. The caller owns
    /// the commit, so the caller owns whatever post-commit signalling
    /// it wants. (The typed `Manager::create_in_tx` path makes the
    /// same choice for the same reason.)
    pub async fn insert_json_in_tx(
        self,
        body: &serde_json::Map<String, serde_json::Value>,
        tx: &mut crate::db::Transaction,
    ) -> Result<serde_json::Map<String, serde_json::Value>, crate::orm::write::WriteError> {
        use crate::orm::write::WriteError;

        // Phase -1 — normalise (shared with the pool path).
        let body_owned: serde_json::Map<String, serde_json::Value>;
        let body: &serde_json::Map<String, serde_json::Value> =
            match normalise_insert_body(self.meta, body) {
                Some(owned) => {
                    body_owned = owned;
                    &body_owned
                }
                None => body,
            };

        // Phase 0 — validation reads through the transaction so an FK
        // at an uncommitted parent resolves.
        let validation_errors =
            crate::orm::validation::validate_on_create_in_tx(self.meta, body, tx).await;
        if !validation_errors.is_empty() {
            return Err(WriteError::Multiple {
                errors: validation_errors,
            });
        }

        // Phase 1 — build the INSERT (shared with the pool path).
        let InsertPlan {
            mut q,
            pk_name,
            pk_ty,
        } = build_insert_plan(self.meta, body)?;

        match tx.backend_name() {
            "sqlite" => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let res = {
                    let inner = tx.as_sqlite_mut().expect("sqlite backend_name");
                    sqlx::query_with(&sql, vals)
                        .execute(&mut **inner)
                        .await
                        .map_err(|e| classify_or_sqlx(e, body))?
                };
                // Re-fetch by PK on the same tx so the caller sees the
                // row the DB stored (defaults, autoincrement).
                let pk_pred = match pk_ty {
                    SqlType::Integer | SqlType::BigInt | SqlType::SmallInt => {
                        Expr::col(Alias::new(&pk_name)).eq(res.last_insert_rowid())
                    }
                    _ => {
                        let supplied = body
                            .get(&pk_name)
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let sea_value = crate::orm::write::json_to_sea_value(
                            pk_ty, &supplied, false, &pk_name, None,
                        )?;
                        Expr::col(Alias::new(&pk_name)).eq(sea_value)
                    }
                };
                let mut sel = Query::select();
                sel.from(crate::db::router::schema_qualified_table(&self.meta.table));
                for c in &self.meta.fields {
                    sel.column(Alias::new(&c.name));
                }
                sel.cond_where(Condition::all().add(pk_pred));
                let (sel_sql, sel_vals) = sel.build_sqlx(SqliteQueryBuilder);
                let mut out = serde_json::Map::new();
                {
                    let inner = tx.as_sqlite_mut().expect("sqlite backend_name");
                    let row = sqlx::query_with(&sel_sql, sel_vals)
                        .fetch_one(&mut **inner)
                        .await?;
                    for col in &self.meta.fields {
                        out.insert(col.name.clone(), decode_to_json(&row, col)?);
                    }
                }
                // Phase 2/3 — junction writes + read-back on the tx.
                let pk_value = out.get(&pk_name).cloned();
                write_m2m_junctions_in_tx(self.meta, pk_value.as_ref(), body, tx).await?;
                hydrate_m2m_into_tx(self.meta, pk_value.as_ref(), &mut out, tx).await?;
                Ok(out)
            }
            _ => {
                q.returning_all();
                let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                let mut out = serde_json::Map::new();
                {
                    let inner = tx.as_pg_mut().expect("postgres backend_name");
                    let row = sqlx::query_with(&sql, vals)
                        .fetch_one(&mut **inner)
                        .await
                        .map_err(|e| classify_or_sqlx(e, body))?;
                    for col in &self.meta.fields {
                        out.insert(col.name.clone(), decode_pg_to_json(&row, col)?);
                    }
                }
                let pk_value = out.get(&pk_name).cloned();
                write_m2m_junctions_in_tx(self.meta, pk_value.as_ref(), body, tx).await?;
                hydrate_m2m_into_tx(self.meta, pk_value.as_ref(), &mut out, tx).await?;
                Ok(out)
            }
        }
    }

    /// Transaction-aware sibling of [`Self::update_json`]. PATCH semantics —
    /// update only the columns present in `body` for the rows matched by the
    /// accumulated WHERE — but every statement runs on the open `tx` so a
    /// batch of updates commits or rolls back as a unit. M2M arrays in the
    /// body are mirrored into junction tables on the same tx. Returns the
    /// number of rows touched.
    ///
    /// Used by REST bulk update (one tx for the whole array). Mirrors the
    /// pool path's validation + `noform`/`slug_from`/`auto_now` handling;
    /// the only difference is the execution target.
    pub async fn update_json_in_tx(
        self,
        body: &serde_json::Map<String, serde_json::Value>,
        tx: &mut crate::db::Transaction,
    ) -> Result<u64, crate::orm::write::WriteError> {
        use crate::orm::write::WriteError;

        // Phase -1 — strip `noform` columns + derive `slug_from` (mirrors
        // the pool path).
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

        // Phase 0 — pre-DB validation, same shape as `update_json`. FK
        // existence reads through the open tx so an FK at an uncommitted
        // sibling row in the same batch resolves.
        let validation_errors =
            crate::orm::validation::validate_on_update_in_tx(self.meta, body, tx).await;
        if !validation_errors.is_empty() {
            return Err(WriteError::Multiple {
                errors: validation_errors,
            });
        }

        let mut q = Query::update();
        q.table(crate::db::router::schema_qualified_table(&self.meta.table));
        let mut any = false;
        for col in &self.meta.fields {
            if col.primary_key {
                continue;
            }
            let Some(json) = body.get(&col.name) else {
                if col.auto_now {
                    let now_value = crate::orm::write::now_for_column(col.ty);
                    q.value(Alias::new(&col.name), now_value);
                    any = true;
                }
                continue;
            };
            validate_numeric_bounds(col, json)?;
            if let (Some(fmt), Some(s)) = (col.text_format.as_deref(), json.as_str()) {
                if let Err(e) = crate::orm::validators::validate_text_format(fmt, s) {
                    return Err(WriteError::Validator {
                        field: col.name.clone(),
                        message: e.to_string(),
                    });
                }
            }
            let sea_value = crate::orm::write::json_to_sea_value(
                col.ty,
                json,
                col.nullable,
                &col.name,
                fk_target_pk_sql_type(col),
            )?;
            q.value(Alias::new(&col.name), sea_value);
            any = true;
        }
        let touches_m2m = self
            .meta
            .m2m_relations
            .iter()
            .any(|r| body.contains_key(&r.field_name));
        if !any && !touches_m2m {
            return Ok(0);
        }
        let where_clauses = self.effective_where_clauses();
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }

        // The PKs the WHERE matches — needed for the M2M mirror below. We
        // read them on the same tx so the bulk update sees its own
        // uncommitted siblings.
        let parent_pks: Vec<serde_json::Value> = match self.meta.pk_column() {
            Some(pk_col) => {
                collect_parent_pks_in_tx(self.meta, pk_col, &where_clauses, tx).await?
            }
            None => Vec::new(),
        };

        if any {
            match tx.backend_name() {
                "sqlite" => {
                    let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                    let inner = tx.as_sqlite_mut().expect("sqlite backend_name");
                    sqlx::query_with(&sql, values)
                        .execute(&mut **inner)
                        .await
                        .map_err(|e| classify_or_sqlx(e, body))?;
                }
                _ => {
                    let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                    let inner = tx.as_pg_mut().expect("postgres backend_name");
                    sqlx::query_with(&sql, values)
                        .execute(&mut **inner)
                        .await
                        .map_err(|e| classify_or_sqlx(e, body))?;
                }
            }
        }
        for pk in &parent_pks {
            write_m2m_junctions_in_tx(self.meta, Some(pk), body, tx).await?;
        }
        Ok(parent_pks.len().max(if any { 1 } else { 0 }) as u64)
    }

    /// Transaction-aware sibling of [`Self::delete`]. Deletes (or
    /// soft-deletes, for a `soft_delete` model) the rows matched by the
    /// accumulated WHERE on the open `tx`, so a batch of deletes commits or
    /// rolls back as a unit. Returns the number of rows affected.
    ///
    /// Soft-delete models stamp `deleted_at = now()` (consistent with the
    /// pool path / gaps #35) unless [`Self::hard_delete`] was set.
    pub async fn delete_in_tx(self, tx: &mut crate::db::Transaction) -> Result<u64, DynError> {
        let soft = self.meta.soft_delete && !self.hard_delete;
        let where_clauses = if soft {
            self.live_where_clauses()
        } else {
            self.effective_where_clauses()
        };

        // Build the SQL for the active backend. Soft-delete is an UPDATE
        // stamping `deleted_at`; a hard delete is a DELETE. Each statement
        // type lowers to `(sql, values)` so the execute arm is uniform.
        let table = crate::db::router::schema_qualified_table(&self.meta.table);
        let build = |is_sqlite: bool| {
            if soft {
                let mut u = Query::update();
                u.table(table.clone());
                u.value(
                    Alias::new("deleted_at"),
                    sea_query::Value::ChronoDateTimeUtc(Some(Box::new(chrono::Utc::now()))),
                );
                for cond in &where_clauses {
                    u.cond_where(cond.clone());
                }
                if is_sqlite {
                    u.build_sqlx(SqliteQueryBuilder)
                } else {
                    u.build_sqlx(PostgresQueryBuilder)
                }
            } else {
                let mut d = Query::delete();
                d.from_table(table.clone());
                for cond in &where_clauses {
                    d.cond_where(cond.clone());
                }
                if is_sqlite {
                    d.build_sqlx(SqliteQueryBuilder)
                } else {
                    d.build_sqlx(PostgresQueryBuilder)
                }
            }
        };

        let rows_affected = match tx.backend_name() {
            "sqlite" => {
                let (sql, values) = build(true);
                let inner = tx.as_sqlite_mut().expect("sqlite backend_name");
                sqlx::query_with(&sql, values)
                    .execute(&mut **inner)
                    .await?
                    .rows_affected()
            }
            _ => {
                let (sql, values) = build(false);
                let inner = tx.as_pg_mut().expect("postgres backend_name");
                sqlx::query_with(&sql, values)
                    .execute(&mut **inner)
                    .await?
                    .rows_affected()
            }
        };
        Ok(rows_affected)
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
        q.table(crate::db::router::schema_qualified_table(&self.meta.table));
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
            validate_numeric_bounds(col, json)?;
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
            let sea_value = crate::orm::write::json_to_sea_value(
                col.ty,
                json,
                col.nullable,
                &col.name,
                fk_target_pk_sql_type(col),
            )?;
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
        let where_clauses = self.effective_where_clauses();
        for cond in &where_clauses {
            q.cond_where(cond.clone());
        }
        // Find every parent_id matched by the filter so we can
        // mirror the M2M arrays into each one's junction AND fire
        // `bulk_post_save:<table>` with the affected ids (gaps #77).
        // Done BEFORE the UPDATE so:
        //   - a no-op (`any = false`, `touches_m2m = true`) still
        //     gets the M2M write, and
        //   - the signal payload carries the exact PK set the WHERE
        //     matched, even when the UPDATE itself is a no-op
        //     (matches the typed `bulk_post_save` semantics: the
        //     subscriber learns "these rows were targeted" rather
        //     than guessing from `rows_affected`).
        let parent_pks: Vec<serde_json::Value> = match self.meta.pk_column() {
            Some(pk_col) => collect_parent_pks(&self.meta, pk_col, &self.where_clauses).await?,
            None => Vec::new(),
        };

        match resolve_pool_dyn(self.meta, crate::db::RouteOp::Write) {
            DbPool::Sqlite(pool) => {
                if any {
                    let (sql, values) = q.build_sqlx(SqliteQueryBuilder);
                    sqlx::query_with(&sql, values)
                        .execute(&pool)
                        .await
                        .map_err(|e| classify_or_sqlx(e, body))?;
                }
                for pk in &parent_pks {
                    write_m2m_junctions(&self.meta, Some(pk), body).await?;
                }
                // gaps #77: `bulk_post_save:<table>` fires after the
                // UPDATE on the dynamic path. `created = false` because
                // this is UPDATE (matches the typed bulk-save convention
                // from gap #38). `ids` is whatever the WHERE matched —
                // collect_parent_pks already ran above.
                crate::signals::emit_bulk_post_save_by_table(
                    &self.meta.table,
                    parent_pks.clone(),
                    false,
                )
                .await;
                Ok(parent_pks.len().max(if any { 1 } else { 0 }) as u64)
            }
            DbPool::Postgres(pool) => {
                if any {
                    let (sql, values) = q.build_sqlx(PostgresQueryBuilder);
                    sqlx::query_with(&sql, values)
                        .execute(&pool)
                        .await
                        .map_err(|e| classify_or_sqlx(e, body))?;
                }
                for pk in &parent_pks {
                    write_m2m_junctions(&self.meta, Some(pk), body).await?;
                }
                crate::signals::emit_bulk_post_save_by_table(
                    &self.meta.table,
                    parent_pks.clone(),
                    false,
                )
                .await;
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
            SqlType::Inet
            | SqlType::Cidr
            | SqlType::MacAddr
            | SqlType::Xml
            | SqlType::Ltree
            | SqlType::Bit
            | SqlType::FullText => panic_pg_only_unsupported(&col.name),
            // PK lift (review #3): FK columns to a String/Uuid-PK target
            // store TEXT/UUID, not BIGINT — decode by the target PK type so
            // the admin display path doesn't fail on a non-i64 FK.
            SqlType::ForeignKey => match fk_target_pk_sql_type(col) {
                Some(SqlType::Text) => row.try_get::<Option<String>, _>(name)?.unwrap_or_default(),
                Some(SqlType::Uuid) => row
                    .try_get::<Option<Uuid>, _>(name)?
                    .map_or(String::new(), |v| v.to_string()),
                _ => row
                    .try_get::<Option<i64>, _>(name)?
                    .map_or(String::new(), |v| v.to_string()),
            },
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
        SqlType::Inet
        | SqlType::Cidr
        | SqlType::MacAddr
        | SqlType::Xml
        | SqlType::Ltree
        | SqlType::Bit
        | SqlType::FullText => panic_pg_only_unsupported(&col.name),
        SqlType::ForeignKey => match fk_target_pk_sql_type(col) {
            Some(SqlType::Text) => row.try_get::<String, _>(name)?,
            Some(SqlType::Uuid) => row.try_get::<Uuid, _>(name)?.to_string(),
            _ => row.try_get::<i64, _>(name)?.to_string(),
        },
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
            | SqlType::Xml
            | SqlType::Ltree
            | SqlType::Bit
            | SqlType::FullText => row
                .try_get::<Option<String>, _>(name)
                .ok()
                .flatten()
                .unwrap_or_default(),
            // PK lift (review #3): FK to a String/Uuid-PK target is a
            // TEXT/native-uuid column on PG — decode by the target PK type.
            SqlType::ForeignKey => match fk_target_pk_sql_type(col) {
                Some(SqlType::Text) => row.try_get::<Option<String>, _>(name)?.unwrap_or_default(),
                Some(SqlType::Uuid) => row
                    .try_get::<Option<Uuid>, _>(name)?
                    .map_or(String::new(), |v| v.to_string()),
                _ => row
                    .try_get::<Option<i64>, _>(name)?
                    .map_or(String::new(), |v| v.to_string()),
            },
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
        | SqlType::Xml
        | SqlType::Ltree
        | SqlType::Bit
        | SqlType::FullText => row.try_get::<String, _>(name).unwrap_or_default(),
        SqlType::ForeignKey => match fk_target_pk_sql_type(col) {
            Some(SqlType::Text) => row.try_get::<String, _>(name)?,
            Some(SqlType::Uuid) => row.try_get::<Uuid, _>(name)?.to_string(),
            _ => row.try_get::<i64, _>(name)?.to_string(),
        },
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
/// Alias-aware sibling of [`decode_to_json`] — same decode logic but
/// pulls from a different column name (the aliased name in a JOIN
/// SELECT). Used by `QuerySet::join_related` to read child columns
/// out of a JOIN row where every child column is exposed as
/// `<field>__<col>`. Cheap clone of `Column` because the existing
/// decoder is keyed off `col.name.as_str()`.
pub fn decode_to_json_aliased(
    row: &sqlx::sqlite::SqliteRow,
    col: &Column,
    alias: &str,
) -> Result<serde_json::Value, sqlx::Error> {
    let mut aliased = col.clone();
    aliased.name = alias.to_string();
    decode_to_json(row, &aliased)
}

/// Postgres counterpart to [`decode_to_json_aliased`].
pub fn decode_pg_to_json_aliased(
    row: &sqlx::postgres::PgRow,
    col: &Column,
    alias: &str,
) -> Result<serde_json::Value, sqlx::Error> {
    let mut aliased = col.clone();
    aliased.name = alias.to_string();
    decode_pg_to_json(row, &aliased)
}

/// PK lift Pass A — when `col` is an FK column (`SqlType::ForeignKey`)
/// pointing at a model whose PK is a `String` / `Uuid` (not the
/// default `i64`), the decoder needs to bind as `String` instead of
/// `i64` or sqlx errors with "Rust type i64 not compatible with SQL
/// type TEXT".
///
/// Looks the target table up in the model registry and reads its
/// PK column's `SqlType`. Returns `None` when:
///   - `col` isn't an FK (caller falls back to the normal arm),
///   - the FK has no target (defensive — shouldn't happen in
///     practice since the macro always sets `fk_target` on FK
///     columns),
///   - the target isn't in the registry (only possible when an
///     internal call site fires before `App::build()` finishes
///     wiring plugins).
///
/// PK lift Pass E — O(1) lookup via the `pk_meta_for_table` cache
/// (was O(n) `Vec<ModelMeta>` clone + linear scan per call). The
/// cache initialises lazily on first post-`App::build` call and
/// serves from a `HashMap` for every subsequent lookup. In a hot
/// decode loop (e.g. 1000 rows × 50 columns × per-FK decode) this
/// drops the per-row registry-walk cost from a few milliseconds
/// to a single hashmap probe.
fn fk_target_pk_sql_type(col: &Column) -> Option<SqlType> {
    if !matches!(col.ty, SqlType::ForeignKey) {
        return None;
    }
    let target_table = col.fk_target.as_deref()?;
    crate::migrate::pk_meta_for_table(target_table).map(|(_, ty)| ty)
}

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
            SqlType::Inet
            | SqlType::Cidr
            | SqlType::MacAddr
            | SqlType::Xml
            | SqlType::Ltree
            | SqlType::Bit
            | SqlType::FullText => panic_pg_only_unsupported(&col.name),
            // PK lift Pass A: FK columns that target a String /
            // Uuid PK store their values as TEXT, not BIGINT. Probe
            // the target meta to pick the right Rust type for the
            // bind.
            SqlType::ForeignKey => match fk_target_pk_sql_type(col) {
                Some(SqlType::Text) => row
                    .try_get::<Option<String>, _>(name)?
                    .map_or(Value::Null, Value::from),
                Some(SqlType::Uuid) => row
                    .try_get::<Option<Uuid>, _>(name)?
                    .map_or(Value::Null, |v| Value::from(v.to_string())),
                _ => row
                    .try_get::<Option<i64>, _>(name)?
                    .map_or(Value::Null, Value::from),
            },
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
        SqlType::Inet
        | SqlType::Cidr
        | SqlType::MacAddr
        | SqlType::Xml
        | SqlType::Ltree
        | SqlType::Bit
        | SqlType::FullText => panic_pg_only_unsupported(&col.name),
        // PK lift Pass A: see the nullable arm above for the same
        // String/Uuid target dispatch.
        SqlType::ForeignKey => match fk_target_pk_sql_type(col) {
            Some(SqlType::Text) => Value::from(row.try_get::<String, _>(name)?),
            Some(SqlType::Uuid) => Value::from(row.try_get::<Uuid, _>(name)?.to_string()),
            _ => Value::from(row.try_get::<i64, _>(name)?),
        },
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
            | SqlType::Xml
            | SqlType::Ltree
            | SqlType::Bit
            | SqlType::FullText => row
                .try_get::<Option<String>, _>(name)
                .ok()
                .flatten()
                .map_or(Value::Null, Value::from),
            // PK lift Pass A: see the SQLite path for the same
            // String/Uuid target dispatch.
            SqlType::ForeignKey => match fk_target_pk_sql_type(col) {
                Some(SqlType::Text) => row
                    .try_get::<Option<String>, _>(name)?
                    .map_or(Value::Null, Value::from),
                Some(SqlType::Uuid) => row
                    .try_get::<Option<Uuid>, _>(name)?
                    .map_or(Value::Null, |v| Value::from(v.to_string())),
                _ => row
                    .try_get::<Option<i64>, _>(name)?
                    .map_or(Value::Null, Value::from),
            },
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
        | SqlType::Xml
        | SqlType::Ltree
        | SqlType::Bit
        | SqlType::FullText => row
            .try_get::<String, _>(name)
            .map(Value::from)
            .unwrap_or(Value::Null),
        // PK lift Pass A: FK columns dispatch on their target's PK
        // type (i64 / String / Uuid).
        SqlType::ForeignKey => match fk_target_pk_sql_type(col) {
            Some(SqlType::Text) => Value::from(row.try_get::<String, _>(name)?),
            Some(SqlType::Uuid) => Value::from(row.try_get::<Uuid, _>(name)?.to_string()),
            _ => Value::from(row.try_get::<i64, _>(name)?),
        },
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
    // #116: JSON / Array columns must PARSE the form string so the
    // typed input becomes a real JsonValue::Object / Array / etc.
    // Pre-fix every form value was wrapped as JsonValue::String —
    // typing `{"k": 1}` into a JSON textarea stored the literal
    // text `"\"{\\\"k\\\": 1}\""` rather than the object.
    //
    // serde_json::from_str rejects unbalanced braces / missing
    // quotes / etc.; we surface that as a WriteError::Validator so
    // the admin's inline error renders "Not valid JSON: <reason>"
    // instead of either silently storing junk OR crashing the
    // write with a raw sqlx Protocol error downstream.
    if matches!(col.ty, SqlType::Json | SqlType::Array(_)) {
        let parsed: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| WriteError::Validator {
                field: col.name.clone(),
                message: format!("Not valid JSON: {e}"),
            })?;
        return json_to_sea_value(col.ty, &parsed, col.nullable, &col.name, None);
    }
    if matches!(col.ty, SqlType::ForeignKey) {
        return match fk_target_pk_sql_type(col) {
            Some(SqlType::Text) => Ok(SeaValue::String(Some(Box::new(raw.to_string())))),
            Some(SqlType::Uuid) => uuid::Uuid::parse_str(raw)
                .map(|v| SeaValue::Uuid(Some(Box::new(v))))
                .map_err(|_| WriteError::TypeMismatch {
                    field: col.name.clone(),
                    expected: SqlType::Uuid,
                    got: raw.to_string(),
                }),
            _ => raw
                .parse::<i64>()
                .map(|v| SeaValue::BigInt(Some(v)))
                .map_err(|_| WriteError::TypeMismatch {
                    field: col.name.clone(),
                    expected: SqlType::BigInt,
                    got: raw.to_string(),
                }),
        };
    }
    let json = serde_json::Value::String(raw.to_string());
    json_to_sea_value(col.ty, &json, col.nullable, &col.name, None)
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

fn validate_numeric_bounds(
    col: &Column,
    json: &serde_json::Value,
) -> Result<(), crate::orm::write::WriteError> {
    let Some(n) = json.as_f64() else {
        return Ok(());
    };
    if let Some(min) = col.min {
        if n < min as f64 {
            return Err(crate::orm::write::WriteError::Validator {
                field: col.name.clone(),
                message: format!("must be >= {min} (got {n})."),
            });
        }
    }
    if let Some(max) = col.max {
        if n > max as f64 {
            return Err(crate::orm::write::WriteError::Validator {
                field: col.name.clone(),
                message: format!("must be <= {max} (got {n})."),
            });
        }
    }
    Ok(())
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
/// Normalize a select_related token: accept both `.` and `__` as
/// hop separators (gap2 #18), return the canonical dotted form
/// (`author.profile`). Mixed separators in one token are flattened
/// the same way (`author.profile__org` → `author.profile.org`).
///
/// Edge case: a column whose actual name contains `__` (rare; real
/// models don't do this) would alias to a dotted chain after this
/// pass and fail validation; the caller silently drops it, matching
/// the existing "unknown column" behaviour.
fn normalize_sr_token(name: &str) -> String {
    name.replace("__", ".")
}

/// Validate a dotted select_related chain (e.g. `"author.profile"`)
/// against the model graph. Each hop must be an FK on the prior
/// hop's target meta. Returns the per-hop target tables on success
/// (same length as `hops.len()`); returns `None` on any failure so
/// the caller can drop the token silently — same contract as the
/// pre-existing single-hop validation in `select_related_dyn`.
///
/// Empty chains, missing meta lookups, and non-FK columns all
/// return `None`.
fn validate_sr_chain(root_meta: &crate::migrate::ModelMeta, chain: &str) -> Option<Vec<String>> {
    let hops: Vec<&str> = chain.split('.').filter(|s| !s.is_empty()).collect();
    if hops.is_empty() {
        return None;
    }
    let registered = crate::migrate::registered_models();
    let mut targets: Vec<String> = Vec::with_capacity(hops.len());
    let mut current_table: String = root_meta.table.clone();
    let mut current_meta: Option<crate::migrate::ModelMeta> = None;
    for hop in &hops {
        let meta_ref: &crate::migrate::ModelMeta =
            if current_table == root_meta.table && current_meta.is_none() {
                root_meta
            } else {
                current_meta = registered
                    .iter()
                    .find(|m| m.table == current_table)
                    .cloned();
                current_meta.as_ref()?
            };
        let col = meta_ref.fields.iter().find(|c| &c.name == hop)?;
        let target = col.fk_target.clone()?;
        targets.push(target.clone());
        current_table = target;
    }
    Some(targets)
}

/// FK expansion for the dynamic-dispatch read path. For each name
/// in `sr_fields` (canonical dotted form — `select_related_dyn`
/// has already normalized + validated), collect the integer ids
/// across `rows`, run one batched `SELECT * FROM <target> WHERE id
/// IN (...)` per hop, and splice the resolved chain back where the
/// root FK id was. Query budget is `1 + len(hops)` per chain
/// regardless of how many parent rows came back. No N+1.
///
/// Mirrors the typed
/// `queryset::hydration::hydrate_select_related_nested` semantics:
/// per-hop fetch top-down, then bottom-up embed so the root rows
/// carry the full nested chain.
///
/// Caller has already validated that every name in `sr_fields`
/// resolves to an FK chain on `meta` (via `select_related_dyn` →
/// [`validate_sr_chain`]).
async fn hydrate_select_related_into(
    meta: &crate::migrate::ModelMeta,
    sr_fields: &[String],
    rows: &mut [serde_json::Map<String, serde_json::Value>],
) -> Result<(), sqlx::Error> {
    let pool = resolve_pool_dyn(meta, crate::db::RouteOp::Read);
    for chain in sr_fields {
        let hops: Vec<&str> = chain.split('.').filter(|s| !s.is_empty()).collect();
        if hops.is_empty() {
            continue;
        }
        let Some(targets) = validate_sr_chain(meta, chain) else {
            // select_related_dyn validates up front; if a chain
            // slipped through validation but fails here (e.g. an
            // unregistered intermediate model — only possible from
            // a direct internal caller), skip rather than crash.
            continue;
        };

        // gaps #112 / PK lift Pass A: walk the chain in PK-shape-
        // agnostic terms. Each hop's PK column name comes from the
        // target meta (could be `"id"` for integer-PK models, but
        // also `"codename"` for `permissions_permission`, etc.).
        // FK ids and PK lookups round-trip as `serde_json::Value`
        // so String / UUID / mixed-PK chains all hydrate without
        // the pre-fix `.as_i64()` silently dropping non-integer
        // links.
        let registered = crate::migrate::registered_models();
        let hop_target_pk: Vec<(String, SqlType)> = targets
            .iter()
            .filter_map(|t| {
                registered
                    .iter()
                    .find(|m| &m.table == t)
                    .and_then(|m| m.pk_column().map(|c| (c.name.clone(), c.ty)))
            })
            .collect();
        if hop_target_pk.len() != hops.len() {
            // A meta lookup failed mid-chain (only possible from
            // an unregistered intermediate model — unreachable in
            // practice). Skip the chain rather than crash.
            continue;
        }
        let hop_target_soft_delete: Vec<bool> = targets
            .iter()
            .map(|t| {
                registered
                    .iter()
                    .find(|m| &m.table == t)
                    .is_some_and(|m| m.soft_delete)
            })
            .collect();

        // Phase 1: per-hop fetch, top-down. levels[i] holds the
        // related-row JSON objects at depth i, BEFORE any nesting
        // is embedded.
        let first_field = hops[0];
        let mut ids: Vec<serde_json::Value> = Vec::with_capacity(rows.len());
        for row in rows.iter() {
            let Some(v) = row.get(first_field) else {
                continue;
            };
            if v.is_null() {
                continue;
            }
            ids.push(v.clone());
        }
        if ids.is_empty() {
            continue;
        }
        dedup_by_pk_key(&mut ids);
        let mut levels: Vec<Vec<serde_json::Value>> = Vec::with_capacity(hops.len());
        levels.push(
            crate::orm::queryset::hydration::fetch_related_as_json_by_pk(
                &targets[0],
                &hop_target_pk[0].0,
                hop_target_pk[0].1,
                hop_target_soft_delete[0],
                &ids,
                &pool,
            )
            .await?,
        );

        for hop_idx in 1..hops.len() {
            let hop_field = hops[hop_idx];
            let hop_target = &targets[hop_idx];
            let prev_lvl = &levels[hop_idx - 1];
            let mut next_ids: Vec<serde_json::Value> = prev_lvl
                .iter()
                .filter_map(|r| {
                    let v = r.as_object()?.get(hop_field)?;
                    if v.is_null() { None } else { Some(v.clone()) }
                })
                .collect();
            if next_ids.is_empty() {
                // Chain bottoms out (every prior-level row has
                // NULL for this hop). Subsequent hops would also
                // be empty; stop here. Earlier levels still embed
                // below.
                break;
            }
            dedup_by_pk_key(&mut next_ids);
            levels.push(
                crate::orm::queryset::hydration::fetch_related_as_json_by_pk(
                    hop_target,
                    &hop_target_pk[hop_idx].0,
                    hop_target_pk[hop_idx].1,
                    hop_target_soft_delete[hop_idx],
                    &next_ids,
                    &pool,
                )
                .await?,
            );
        }

        // Phase 2: bottom-up embed. For each level from second-
        // to-last down to first, splice the next level's matching
        // row into the corresponding hop slot. By the time we hit
        // level 0 its rows carry the full nested chain.
        if levels.len() > 1 {
            for i in (0..levels.len() - 1).rev() {
                let next_pk_col = &hop_target_pk[i + 1].0;
                let next_by_pk: HashMap<String, serde_json::Value> = levels[i + 1]
                    .iter()
                    .filter_map(|obj| {
                        let map = obj.as_object()?;
                        let pk_val = map.get(next_pk_col.as_str())?;
                        Some((pk_json_key(pk_val), obj.clone()))
                    })
                    .collect();
                let hop_field = hops[i + 1];
                for row in levels[i].iter_mut() {
                    let Some(map) = row.as_object_mut() else {
                        continue;
                    };
                    let Some(fk_val) = map.get(hop_field) else {
                        continue;
                    };
                    if fk_val.is_null() {
                        continue;
                    }
                    let key = pk_json_key(fk_val);
                    if let Some(next_json) = next_by_pk.get(&key) {
                        map.insert(hop_field.to_string(), next_json.clone());
                    }
                }
            }
        }

        // Phase 3: splice level-0 rows (now fully nested) into
        // the root rows. Rows pointing at an id that didn't
        // resolve (target row deleted between the parent fetch
        // and the IN-lookup — a race window) keep the raw FK
        // value; the alternative would be silently nulling the
        // field which hides a real referential-integrity issue.
        let first_pk_col = &hop_target_pk[0].0;
        let first_by_pk: HashMap<String, serde_json::Value> = levels
            .into_iter()
            .next()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|obj| {
                let map = obj.as_object()?;
                let pk_val = map.get(first_pk_col.as_str())?;
                Some((pk_json_key(pk_val), obj.clone()))
            })
            .collect();
        for row in rows.iter_mut() {
            let Some(fk_val) = row.get(first_field) else {
                continue;
            };
            if fk_val.is_null() {
                continue;
            }
            let key = pk_json_key(fk_val);
            if let Some(resolved) = first_by_pk.get(&key) {
                row.insert(first_field.to_string(), resolved.clone());
            }
        }
    }
    Ok(())
}

/// Dedup a `Vec<serde_json::Value>` of PK values by stable string
/// key. `serde_json::Value` isn't `Hash`, so the standard
/// sort+dedup doesn't apply; the `pk_json_key` namespacing makes
/// every Number / String / other land in its own bucket.
fn dedup_by_pk_key(ids: &mut Vec<serde_json::Value>) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    ids.retain(|v| seen.insert(pk_json_key(v)));
}

/// Batched M2M echo across every row returned by `fetch_as_json`.
/// One `SELECT parent_id, child_id FROM <junction> WHERE parent_id
/// IN (...)` per registered M2M relation — query budget is
/// `count(meta.m2m_relations)` regardless of how many parent rows
/// came back. Replaces the per-row [`hydrate_m2m_into`] call site
/// in the read loop (gap2 #16) which was a 1+N*M issuer.
///
/// Each row's `<relation>` key is inserted as an array of `child_id`
/// values (integers or strings, matching the junction column's
/// declared shape). Parents with no junction rows still get the key
/// — initialised to an empty array — so the response shape is
/// consistent regardless of link presence (same contract the
/// per-row helper already maintained).
async fn hydrate_m2m_batched(
    meta: &crate::migrate::ModelMeta,
    pk_name: &str,
    rows: &mut [serde_json::Map<String, serde_json::Value>],
) -> Result<(), sqlx::Error> {
    if meta.m2m_relations.is_empty() || rows.is_empty() {
        return Ok(());
    }

    // Initialise every row's relation arrays up front so parents
    // with zero junction rows still surface the field. Matches the
    // per-row helper's behaviour where the `SELECT` returning zero
    // rows produced `<rel>: []` rather than omitting the key.
    for row in rows.iter_mut() {
        for rel in &meta.m2m_relations {
            row.insert(rel.field_name.clone(), serde_json::Value::Array(Vec::new()));
        }
    }

    // Collect parent PKs once across all rows, deduped. Skip rows
    // missing the PK column or whose PK value isn't a shape the
    // junction can bind (numbers + strings; see `json_pk_to_sea`).
    let mut parent_sea_vals: Vec<sea_query::Value> = Vec::with_capacity(rows.len());
    let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in rows.iter() {
        let Some(pk_json) = row.get(pk_name) else {
            continue;
        };
        let Some(sea_val) = json_pk_to_sea(pk_json) else {
            continue;
        };
        let key = pk_json_key(pk_json);
        if seen_keys.insert(key) {
            parent_sea_vals.push(sea_val);
        }
    }
    if parent_sea_vals.is_empty() {
        return Ok(());
    }

    for rel in &meta.m2m_relations {
        let junction_table = format!("{}_{}", meta.table, rel.field_name);
        let mut sel = Query::select();
        sel.from(crate::db::router::schema_qualified_table(&junction_table));
        sel.column(Alias::new("parent_id"));
        sel.column(Alias::new("child_id"));
        sel.and_where(Expr::col(Alias::new("parent_id")).is_in(parent_sea_vals.clone()));

        let mut children_by_parent: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        match resolve_pool_dyn(meta, crate::db::RouteOp::Read) {
            DbPool::Sqlite(pool) => {
                let (sql, values) = sel.build_sqlx(SqliteQueryBuilder);
                let db_rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
                for r in &db_rows {
                    let parent = read_junction_id_sqlite(r, "parent_id")?;
                    let child = read_junction_id_sqlite(r, "child_id")?;
                    children_by_parent
                        .entry(pk_json_key(&parent))
                        .or_default()
                        .push(child);
                }
            }
            DbPool::Postgres(pool) => {
                let (sql, values) = sel.build_sqlx(PostgresQueryBuilder);
                let db_rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
                for r in &db_rows {
                    let parent = read_junction_id_pg(r, "parent_id")?;
                    let child = read_junction_id_pg(r, "child_id")?;
                    children_by_parent
                        .entry(pk_json_key(&parent))
                        .or_default()
                        .push(child);
                }
            }
        }

        for row in rows.iter_mut() {
            let Some(pk_json) = row.get(pk_name) else {
                continue;
            };
            let key = pk_json_key(pk_json);
            if let Some(children) = children_by_parent.remove(&key) {
                row.insert(rel.field_name.clone(), serde_json::Value::Array(children));
            }
        }
    }
    Ok(())
}

/// Stable string key for a parent PK JSON value, used to group
/// junction rows under their owning parent in
/// [`hydrate_m2m_batched`]. Integers and strings get their own
/// disjoint namespaces (`n:42` vs `s:42`) so a numeric PK and a
/// string PK that stringify identically never collide.
fn pk_json_key(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Number(n) => format!("n:{n}"),
        serde_json::Value::String(s) => format!("s:{s}"),
        other => format!("o:{other}"),
    }
}

/// Read a junction-table id column as JSON (number or string).
/// Junction columns are i64 for integer PKs and TEXT for string /
/// uuid PKs; we don't know at compile time which one a relation
/// uses, so try i64 first and fall back to String.
fn read_junction_id_sqlite(
    row: &sqlx::sqlite::SqliteRow,
    col: &str,
) -> Result<serde_json::Value, sqlx::Error> {
    if let Ok(i) = row.try_get::<i64, _>(col) {
        return Ok(serde_json::Value::Number(i.into()));
    }
    let s = row.try_get::<String, _>(col)?;
    Ok(serde_json::Value::String(s))
}

fn read_junction_id_pg(
    row: &sqlx::postgres::PgRow,
    col: &str,
) -> Result<serde_json::Value, sqlx::Error> {
    if let Ok(i) = row.try_get::<i64, _>(col) {
        return Ok(serde_json::Value::Number(i.into()));
    }
    let s = row.try_get::<String, _>(col)?;
    Ok(serde_json::Value::String(s))
}

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
        sel.from(crate::db::router::schema_qualified_table(&junction_table));
        sel.column(Alias::new("child_id"));
        sel.and_where(Expr::col(Alias::new("parent_id")).eq(parent_pk_value.clone()));
        let children: Vec<serde_json::Value> =
            match resolve_pool_dyn(meta, crate::db::RouteOp::Read) {
                DbPool::Sqlite(pool) => {
                    let (sql, values) = sel.build_sqlx(SqliteQueryBuilder);
                    let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
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
                    let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
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
    sel.from(crate::db::router::schema_qualified_table(&meta.table));
    sel.column(Alias::new(&pk_col.name));
    for cond in where_clauses {
        sel.cond_where(cond.clone());
    }
    match resolve_pool_dyn(meta, crate::db::RouteOp::Read) {
        DbPool::Sqlite(pool) => {
            let (sql, values) = sel.build_sqlx(SqliteQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
            rows.iter()
                .map(|row| decode_to_json(row, pk_col))
                .collect::<Result<Vec<_>, _>>()
                .map_err(crate::orm::write::WriteError::Sqlx)
        }
        DbPool::Postgres(pool) => {
            let (sql, values) = sel.build_sqlx(PostgresQueryBuilder);
            let rows = sqlx::query_with(&sql, values).fetch_all(&pool).await?;
            rows.iter()
                .map(|row| decode_pg_to_json(row, pk_col))
                .collect::<Result<Vec<_>, _>>()
                .map_err(crate::orm::write::WriteError::Sqlx)
        }
    }
}

/// Transaction-aware sibling of [`collect_parent_pks`]: reads the matched
/// PKs on the open `tx` so a bulk update mid-transaction sees the rows the
/// same tx has touched. Used by `update_json_in_tx`.
async fn collect_parent_pks_in_tx(
    meta: &crate::migrate::ModelMeta,
    pk_col: &crate::migrate::Column,
    where_clauses: &[Condition],
    tx: &mut crate::db::Transaction,
) -> Result<Vec<serde_json::Value>, crate::orm::write::WriteError> {
    let mut sel = Query::select();
    sel.from(crate::db::router::schema_qualified_table(&meta.table));
    sel.column(Alias::new(&pk_col.name));
    for cond in where_clauses {
        sel.cond_where(cond.clone());
    }
    match tx.backend_name() {
        "sqlite" => {
            let (sql, values) = sel.build_sqlx(SqliteQueryBuilder);
            let inner = tx.as_sqlite_mut().expect("sqlite backend_name");
            let rows = sqlx::query_with(&sql, values)
                .fetch_all(&mut **inner)
                .await?;
            rows.iter()
                .map(|row| decode_to_json(row, pk_col))
                .collect::<Result<Vec<_>, _>>()
                .map_err(crate::orm::write::WriteError::Sqlx)
        }
        _ => {
            let (sql, values) = sel.build_sqlx(PostgresQueryBuilder);
            let inner = tx.as_pg_mut().expect("postgres backend_name");
            let rows = sqlx::query_with(&sql, values)
                .fetch_all(&mut **inner)
                .await?;
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
/// Phase -1 of the dynamic insert: strip `noform` columns and derive
/// any `#[umbra(slug_from = "...")]` columns. Returns `Some(owned)`
/// when either rule fired (the caller binds the owned copy) or `None`
/// when the body passes through untouched. Shared by `insert_json`
/// and `insert_json_in_tx` so the two paths can't drift on what they
/// strip / derive before validation runs.
fn normalise_insert_body(
    meta: &crate::migrate::ModelMeta,
    body: &serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let needs_owned = meta
        .fields
        .iter()
        .any(|c| c.noform || c.slug_from.is_some());
    if !needs_owned {
        return None;
    }
    let mut owned = body.clone();
    for col in &meta.fields {
        if col.noform {
            owned.remove(&col.name);
        }
    }
    crate::orm::write::apply_slug_from(&meta.fields, &mut owned, false);
    Some(owned)
}

/// The prepared INSERT plus the PK shape the caller re-fetches by.
struct InsertPlan {
    q: sea_query::InsertStatement,
    pk_name: String,
    pk_ty: SqlType,
}

/// Phase 1 of the dynamic insert: validate min/max + text-format
/// wrappers per column, coerce each JSON value to its `SeaValue`, and
/// assemble the `Query::insert()`. Auto-increment integer PKs and
/// absent-with-default columns are omitted so the backend fills them;
/// `auto_now` / `auto_now_add` columns the body omitted are filled
/// with `Utc::now()`. Shared by `insert_json` and `insert_json_in_tx`
/// so column handling is identical on both paths; the methods differ
/// only in which executor runs the statement.
fn build_insert_plan(
    meta: &crate::migrate::ModelMeta,
    body: &serde_json::Map<String, serde_json::Value>,
) -> Result<InsertPlan, crate::orm::write::WriteError> {
    use crate::orm::write::{WriteError, is_default_pk};

    let mut cols: Vec<&str> = Vec::new();
    let mut values: Vec<SeaValue> = Vec::new();
    for col in &meta.fields {
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
            if col.auto_now_add || col.auto_now {
                let now_value = crate::orm::write::now_for_column(col.ty);
                cols.push(&col.name);
                values.push(now_value);
                continue;
            }
            continue;
        };
        if json.is_null() {
            continue;
        }
        validate_numeric_bounds(col, json)?;
        if let (Some(fmt), Some(s)) = (col.text_format.as_deref(), json.as_str()) {
            if let Err(e) = crate::orm::validators::validate_text_format(fmt, s) {
                return Err(WriteError::Validator {
                    field: col.name.clone(),
                    message: e.to_string(),
                });
            }
        }
        let sea_value = crate::orm::write::json_to_sea_value(
            col.ty,
            json,
            col.nullable,
            &col.name,
            fk_target_pk_sql_type(col),
        )?;
        cols.push(&col.name);
        values.push(sea_value);
    }

    let pk_col = meta.fields.iter().find(|c| c.primary_key).ok_or_else(|| {
        WriteError::Sqlx(sqlx::Error::Protocol(
            "insert_json: model has no PK".to_string(),
        ))
    })?;
    let pk_name = pk_col.name.clone();
    let pk_ty = pk_col.ty;

    let mut q = Query::insert();
    q.into_table(crate::db::router::schema_qualified_table(&meta.table));
    q.columns(cols.iter().map(|c| Alias::new(*c)).collect::<Vec<_>>());
    let exprs: Vec<sea_query::SimpleExpr> = values.into_iter().map(Into::into).collect();
    q.values_panic(exprs);

    Ok(InsertPlan { q, pk_name, pk_ty })
}

/// Transaction-aware sibling of [`write_m2m_junctions`]: mirrors each
/// M2M field in `body` into its junction table on the passed `tx`, so
/// the junction rows commit / roll back with the parent INSERT.
async fn write_m2m_junctions_in_tx(
    meta: &crate::migrate::ModelMeta,
    parent_pk_json: Option<&serde_json::Value>,
    body: &serde_json::Map<String, serde_json::Value>,
    tx: &mut crate::db::Transaction,
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
            continue;
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
        crate::orm::m2m::set_junction_dynamic_in_tx(
            &junction_table,
            parent_pk_value.clone(),
            child_ids,
            tx,
        )
        .await
        .map_err(crate::orm::write::WriteError::Sqlx)?;
    }
    Ok(())
}

/// Transaction-aware sibling of [`hydrate_m2m_into`]: read the just-
/// written junction rows back off the SAME `tx` so the response echoes
/// the M2M arrays the caller will see post-commit. Reading on the pool
/// here would miss the uncommitted junction writes.
async fn hydrate_m2m_into_tx(
    meta: &crate::migrate::ModelMeta,
    parent_pk_json: Option<&serde_json::Value>,
    out: &mut serde_json::Map<String, serde_json::Value>,
    tx: &mut crate::db::Transaction,
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
        sel.from(crate::db::router::schema_qualified_table(&junction_table));
        sel.column(Alias::new("child_id"));
        sel.and_where(Expr::col(Alias::new("parent_id")).eq(parent_pk_value.clone()));
        let children: Vec<serde_json::Value> = match tx.backend_name() {
            "sqlite" => {
                let inner = tx.as_sqlite_mut().expect("sqlite backend_name");
                let (sql, values) = sel.build_sqlx(SqliteQueryBuilder);
                let rows = sqlx::query_with(&sql, values)
                    .fetch_all(&mut **inner)
                    .await?;
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
            _ => {
                let inner = tx.as_pg_mut().expect("postgres backend_name");
                let (sql, values) = sel.build_sqlx(PostgresQueryBuilder);
                let rows = sqlx::query_with(&sql, values)
                    .fetch_all(&mut **inner)
                    .await?;
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
        crate::orm::m2m::set_junction_dynamic(
            &junction_table,
            parent_pk_value.clone(),
            child_ids,
            Some(&meta.name),
        )
        .await
        .map_err(crate::orm::write::WriteError::Sqlx)?;
    }
    Ok(())
}

// =========================================================================
// CSV / tabular import (#61). Coerce string cells to the column's type and
// route each row through `insert_json`, so validators / auto_now /
// slug_from / FK-existence checks all apply. The CSV *parsing* lives in the
// CLI (the `csv` crate); this is the coerce-and-insert half, kept in core
// because the type coercion needs `ModelMeta` + `SqlType` + the dynamic
// write path.
// =========================================================================

/// Coerce one raw CSV cell to the `serde_json::Value` shape its column
/// expects, so downstream validation (`min`/`max`, choices) sees a typed
/// value rather than a string. An empty cell on a nullable column becomes
/// `null`. A value that doesn't parse for a numeric/bool column falls back
/// to the raw string, letting `insert_json` surface a clear per-row error
/// instead of silently dropping data. Text / Date / Time / Uuid / etc.
/// pass through as strings — `json_to_sea_value` parses each from there.
fn coerce_csv_cell(ty: SqlType, nullable: bool, raw: &str) -> serde_json::Value {
    use serde_json::Value;
    if raw.is_empty() && nullable {
        return Value::Null;
    }
    match ty {
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::ForeignKey => raw
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(raw.to_string())),
        SqlType::Real | SqlType::Double => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(raw.to_string())),
        SqlType::Boolean => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "t" | "yes" | "y" => Value::Bool(true),
            "false" | "0" | "f" | "no" | "n" => Value::Bool(false),
            _ => Value::String(raw.to_string()),
        },
        SqlType::Json => {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
        }
        _ => Value::String(raw.to_string()),
    }
}

/// Outcome of [`import_table_rows`]: how many rows inserted, plus the
/// `(line, message)` of every row that failed. Best-effort — a bad row is
/// reported and skipped, never fatal — because messy real-world CSVs want
/// "tell me which rows are wrong," not an all-or-nothing abort. `line` is
/// 1-based over the file (the header is line 1, so the first data row is
/// line 2), matching what a spreadsheet shows.
#[derive(Debug, Default)]
pub struct CsvImportReport {
    pub inserted: usize,
    pub errors: Vec<(usize, String)>,
}

/// Insert tabular string rows into `meta`'s table. Each cell is coerced to
/// its column's type ([`coerce_csv_cell`]) and the row routes through the
/// dynamic write path ([`DynQuerySet::insert_json`]) so every per-row
/// framework behaviour (validators, `auto_now`, `slug_from`, FK existence,
/// soft-delete) applies exactly as it would for a REST POST.
///
/// `headers` names the column each cell maps to; a header that matches no
/// model field is ignored, so an extra CSV column (or a re-ordered export)
/// imports cleanly. Rows commit independently — there is no surrounding
/// transaction (the dynamic write path has none; see `orm_fixes.md` #2).
pub async fn import_table_rows(
    meta: &ModelMeta,
    headers: &[String],
    rows: &[Vec<String>],
) -> CsvImportReport {
    let col_for: HashMap<&str, &Column> =
        meta.fields.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut report = CsvImportReport::default();
    for (i, row) in rows.iter().enumerate() {
        let mut obj = serde_json::Map::new();
        for (header, cell) in headers.iter().zip(row.iter()) {
            if let Some(col) = col_for.get(header.as_str()) {
                obj.insert(header.clone(), coerce_csv_cell(col.ty, col.nullable, cell));
            }
        }
        match DynQuerySet::for_meta(meta).insert_json(&obj).await {
            Ok(_) => report.inserted += 1,
            Err(e) => report.errors.push((i + 2, e.to_string())),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::form_str_to_sea_value;
    use crate::migrate::Column;
    use crate::orm::{FkAction, SqlType};
    use sea_query::Value as SeaValue;

    fn col(name: &str, ty: SqlType, nullable: bool) -> Column {
        Column {
            name: name.to_string(),
            ty,
            primary_key: false,
            nullable,
            fk_target: None,
            noform: false,
            db_constraint: true,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: Vec::new(),
            choice_labels: Vec::new(),
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: FkAction::NoAction,
            on_update: FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: None,
            slug_from: None,
        }
    }

    #[test]
    fn form_fk_numeric_string_binds_as_bigint() {
        let mut plugin = col("plugin", SqlType::ForeignKey, false);
        plugin.fk_target = Some("plugin".to_string());

        let value = form_str_to_sea_value(&plugin, "1").expect("coerce FK id");

        assert_eq!(
            value,
            SeaValue::BigInt(Some(1)),
            "integer-backed FK form values must bind as bigint, not text"
        );
    }

    #[test]
    fn nullable_form_fk_blank_binds_as_null_bigint() {
        let mut parent = col("parent", SqlType::ForeignKey, true);
        parent.fk_target = Some("plugin_comment".to_string());

        let value = form_str_to_sea_value(&parent, "").expect("blank nullable FK");

        assert_eq!(
            value,
            SeaValue::BigInt(None),
            "blank nullable integer-backed FK should bind SQL NULL"
        );
    }
}
