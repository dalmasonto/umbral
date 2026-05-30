//! umbra-rest — auto-generated JSON REST API over umbra models.
//!
//! Register [`RestPlugin`] on `App::builder()` and every registered
//! model gets a standard REST surface at `/api/<table>/`:
//!
//! - `GET /api/<table>/`         — list (returns `{"results": [...], "count": N}`)
//! - `POST /api/<table>/`        — create, returns 201 + the new row
//! - `GET /api/<table>/<id>`     — retrieve, 404 on miss
//! - `PUT /api/<table>/<id>`     — update (full replacement), returns 200 + row
//! - `PATCH /api/<table>/<id>`   — partial update, returns 200 + row
//! - `DELETE /api/<table>/<id>`  — destroy, returns 204
//!
//! Same data, plain JSON. Per-column dispatch on the M3 `SqlType`
//! catalogue: integers / floats / bool / text / date / time /
//! timestamptz / uuid, plus nullable forms.
//!
//! ## Exposure
//!
//! By default the plugin auto-exposes every registered model except
//! the three known-internal tables: `auth_user`, `session`, and
//! `umbra_migrations`. Letting `/api/auth_user/` exist would leak
//! password hashes; the default block-list is the safe shape.
//!
//! Tighten with `RestPlugin::new().include_only(["article"])` or
//! loosen with `.exclude(["sensitive_thing"])`. The builder is
//! chainable.
//!
//! ## Auth
//!
//! v1 ships no built-in auth gate — every exposed route is open.
//! Apps that need authenticated CRUD wrap the umbra-rest router
//! with a tower layer (or write their own handler that delegates
//! after the auth check). A future round adds optional
//! `RestPlugin::require_staff()` that mirrors umbra-admin's Basic
//! Auth gate.

use std::sync::OnceLock;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::{Row, SqlitePool};
use umbra::migrate::{Column, ModelMeta};
use umbra::orm::SqlType;
use umbra::prelude::*;
use umbra::web::{Json, Path, Response, StatusCode};
use uuid::Uuid;

/// The block-list every plugin starts with. Exposing these via REST
/// would leak password hashes (auth_user), session IDs (session), or
/// the migration tracking table itself.
const DEFAULT_BLOCKED_TABLES: &[&str] = &["auth_user", "session", "umbra_migrations"];

/// The plugin. Mounts the REST routes at `/api`.
#[derive(Debug, Clone)]
pub struct RestPlugin {
    include_only: Option<Vec<String>>,
    extra_exclude: Vec<String>,
}

impl Default for RestPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl RestPlugin {
    pub fn new() -> Self {
        Self {
            include_only: None,
            extra_exclude: Vec::new(),
        }
    }

    /// Restrict exposure to exactly this set of tables. Every other
    /// model registered with the framework is hidden, including any
    /// not on the default block-list.
    pub fn include_only<I, S>(mut self, tables: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.include_only = Some(tables.into_iter().map(Into::into).collect());
        self
    }

    /// Add tables to the block-list. Defaults still apply.
    pub fn exclude<I, S>(mut self, tables: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for t in tables {
            self.extra_exclude.push(t.into());
        }
        self
    }

    fn allow(&self, table: &str) -> bool {
        if let Some(allow) = &self.include_only {
            return allow.iter().any(|t| t == table);
        }
        if DEFAULT_BLOCKED_TABLES.contains(&table) {
            return false;
        }
        if self.extra_exclude.iter().any(|t| t == table) {
            return false;
        }
        true
    }
}

/// The configured plugin instance, captured at `App::build` time so
/// the route handlers (which can't capture state through axum's
/// handler trait without a State<T>) can consult the allow/block
/// rules per request.
static CONFIG: OnceLock<RestPlugin> = OnceLock::new();

impl Plugin for RestPlugin {
    fn name(&self) -> &'static str {
        "rest"
    }

    fn routes(&self) -> Router {
        // The OnceLock-captured config is what the static handlers
        // read. `routes()` is called exactly once per App::build, so
        // setting it here is safe.
        let _ = CONFIG.set(self.clone());

        Router::new()
            .route("/api/{table}/", get(list).post(create))
            .route("/api/{table}", get(list).post(create))
            .route(
                "/api/{table}/{id}",
                get(retrieve).put(update).patch(update).delete(destroy),
            )
    }
}

// =========================================================================
// Errors. Mapped to a JSON envelope so clients get a consistent shape.
// =========================================================================

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: String,
    code: &'static str,
}

#[derive(Debug)]
enum ApiError {
    NotFound(String),
    BadInput(String),
    Sqlx(sqlx::Error),
    Json(serde_json::Error),
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl umbra::web::IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, msg) = match self {
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m),
            ApiError::BadInput(m) => (StatusCode::BAD_REQUEST, "bad_input", m),
            ApiError::Sqlx(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "database_error",
                e.to_string(),
            ),
            ApiError::Json(e) => (StatusCode::BAD_REQUEST, "invalid_json", e.to_string()),
        };
        (
            status,
            Json(ApiErrorBody {
                error: msg,
                code,
            }),
        )
            .into_response()
    }
}

// =========================================================================
// Model discovery + the allow/block check.
// =========================================================================

fn allowed_model(table: &str) -> Result<ModelMeta, ApiError> {
    let config = CONFIG.get().expect("RestPlugin::routes was called");
    if !config.allow(table) {
        return Err(ApiError::NotFound(format!("no resource at /api/{table}")));
    }
    for plugin in umbra::migrate::registered_plugins() {
        for m in umbra::migrate::models_for_plugin(&plugin) {
            if m.table == table {
                return Ok(m);
            }
        }
    }
    Err(ApiError::NotFound(format!("no resource at /api/{table}")))
}

fn pk_column(model: &ModelMeta) -> Result<&Column, ApiError> {
    model
        .fields
        .iter()
        .find(|c| c.primary_key)
        .ok_or_else(|| ApiError::BadInput(format!("`{}` has no primary key", model.table)))
}

// =========================================================================
// Handlers.
// =========================================================================

#[derive(Debug, Serialize)]
struct ListResponse {
    results: Vec<Map<String, Value>>,
    count: usize,
}

async fn list(Path(table): Path<String>) -> Result<Json<ListResponse>, ApiError> {
    let model = allowed_model(&table)?;
    let pool = umbra::db::pool();
    let rows = fetch_rows(&pool, &model, None).await?;
    let count = rows.len();
    Ok(Json(ListResponse {
        results: rows,
        count,
    }))
}

async fn retrieve(
    Path((table, id)): Path<(String, String)>,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let model = allowed_model(&table)?;
    let pk = pk_column(&model)?;
    let pool = umbra::db::pool();
    let rows = fetch_rows(&pool, &model, Some((&pk.name, &id))).await?;
    let Some(row) = rows.into_iter().next() else {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    };
    Ok(Json(row))
}

async fn create(
    Path(table): Path<String>,
    Json(body): Json<Map<String, Value>>,
) -> Result<(StatusCode, Json<Map<String, Value>>), ApiError> {
    let model = allowed_model(&table)?;
    let pool = umbra::db::pool();
    let new_id = insert_row(&pool, &model, &body).await?;
    let pk = pk_column(&model)?;
    let rows = fetch_rows(&pool, &model, Some((&pk.name, &new_id))).await?;
    let Some(row) = rows.into_iter().next() else {
        return Err(ApiError::BadInput(
            "row inserted but disappeared on read-back".into(),
        ));
    };
    Ok((StatusCode::CREATED, Json(row)))
}

async fn update(
    Path((table, id)): Path<(String, String)>,
    Json(body): Json<Map<String, Value>>,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let model = allowed_model(&table)?;
    let pk = pk_column(&model)?;
    let pool = umbra::db::pool();

    // 404 if the target row doesn't exist before we attempt the UPDATE.
    let existing = fetch_rows(&pool, &model, Some((&pk.name, &id))).await?;
    if existing.is_empty() {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    }

    update_row(&pool, &model, pk, &id, &body).await?;
    let rows = fetch_rows(&pool, &model, Some((&pk.name, &id))).await?;
    let Some(row) = rows.into_iter().next() else {
        return Err(ApiError::BadInput(
            "row updated but disappeared on read-back".into(),
        ));
    };
    Ok(Json(row))
}

async fn destroy(Path((table, id)): Path<(String, String)>) -> Result<StatusCode, ApiError> {
    let model = allowed_model(&table)?;
    let pk = pk_column(&model)?;
    let pool = umbra::db::pool();
    let result = sqlx::query(&format!(
        "DELETE FROM \"{}\" WHERE \"{}\" = ?",
        model.table, pk.name
    ))
    .bind(&id)
    .execute(&pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

// =========================================================================
// Row marshalling. Per-SqlType dispatch on both directions; same pattern
// the backup and admin modules use.
// =========================================================================

async fn fetch_rows(
    pool: &SqlitePool,
    model: &ModelMeta,
    where_clause: Option<(&str, &str)>,
) -> Result<Vec<Map<String, Value>>, ApiError> {
    let columns = model
        .fields
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = match where_clause {
        Some((col, _)) => format!(
            "SELECT {columns} FROM \"{}\" WHERE \"{}\" = ? LIMIT 1",
            model.table, col
        ),
        None => format!("SELECT {columns} FROM \"{}\" ORDER BY 1", model.table),
    };
    let mut q = sqlx::query(&sql);
    if let Some((_, val)) = where_clause {
        q = q.bind(val.to_string());
    }
    let rows = q.fetch_all(pool).await?;
    let mut out: Vec<Map<String, Value>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut obj = Map::new();
        for col in &model.fields {
            obj.insert(col.name.clone(), column_to_json(&row, col)?);
        }
        out.push(obj);
    }
    Ok(out)
}

fn column_to_json(row: &sqlx::sqlite::SqliteRow, col: &Column) -> Result<Value, ApiError> {
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
    })
}

async fn insert_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    body: &Map<String, Value>,
) -> Result<String, ApiError> {
    // PK with an integer SqlType is auto-generated by SQLite, so it
    // skips the writable set unless the client supplied it. Other
    // PK shapes (uuid::Uuid, String) the client must supply.
    let pk = pk_column(model)?;
    let pk_is_autoincrement = pk.primary_key
        && matches!(
            pk.ty,
            SqlType::Integer | SqlType::BigInt | SqlType::SmallInt
        );
    let writable: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| {
            !(c.primary_key
                && matches!(c.ty, SqlType::Integer | SqlType::BigInt | SqlType::SmallInt)
                && !body.contains_key(&c.name))
        })
        .collect();
    let names = writable
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = writable.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "INSERT INTO \"{}\" ({names}) VALUES ({placeholders})",
        model.table
    );
    let mut q = sqlx::query(&sql);
    for col in &writable {
        q = bind_json_value(q, col, body)?;
    }
    let result = q.execute(pool).await?;

    if pk_is_autoincrement {
        // SQLite hands out monotonic ids via ROWID; read back via
        // last_insert_rowid().
        Ok(result.last_insert_rowid().to_string())
    } else {
        // String / uuid PK: the client supplied it; echo it back.
        let id = body
            .get(&pk.name)
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                ApiError::BadInput(format!(
                    "non-integer primary key `{}` must be supplied in the request body",
                    pk.name
                ))
            })?;
        Ok(id)
    }
}

async fn update_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    pk: &Column,
    pk_value: &str,
    body: &Map<String, Value>,
) -> Result<(), ApiError> {
    // For PATCH semantics: update only the columns the body provided.
    // For PUT semantics: same, since missing columns we treat as
    // "leave alone" rather than clobbering with NULL/default. The
    // difference between PUT and PATCH at v1 is purely method
    // routing; both call this.
    let updates: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| !c.primary_key && body.contains_key(&c.name))
        .collect();
    if updates.is_empty() {
        return Ok(());
    }
    let setters = updates
        .iter()
        .map(|c| format!("\"{}\" = ?", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE \"{}\" SET {setters} WHERE \"{}\" = ?",
        model.table, pk.name
    );
    let mut q = sqlx::query(&sql);
    for col in &updates {
        q = bind_json_value(q, col, body)?;
    }
    q = q.bind(pk_value.to_string());
    q.execute(pool).await?;
    Ok(())
}

type SqlxQuery<'q> = sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>;

/// Bind one column's value to a `sqlx::query::Query`. The JSON value
/// is parsed against the column's `SqlType` and coerced where the
/// HTML / JSON shapes differ from sqlx's native types (RFC-3339
/// strings for timestamps, "true"/"1" for booleans coming through a
/// stringly-typed body).
fn bind_json_value<'q>(q: SqlxQuery<'q>, col: &Column, body: &Map<String, Value>) -> Result<SqlxQuery<'q>, ApiError> {
    let raw = body.get(&col.name).cloned().unwrap_or(Value::Null);
    Ok(match raw {
        Value::Null if col.nullable => bind_null(q, col),
        Value::Null => {
            return Err(ApiError::BadInput(format!(
                "field `{}` is required and was null",
                col.name
            )));
        }
        Value::Bool(b) if matches!(col.ty, SqlType::Boolean) => q.bind(b),
        Value::Number(n) if matches!(col.ty, SqlType::SmallInt | SqlType::Integer) => q.bind(
            n.as_i64()
                .ok_or_else(|| {
                    ApiError::BadInput(format!("field `{}` must be an integer", col.name))
                })? as i32,
        ),
        Value::Number(n) if matches!(col.ty, SqlType::BigInt) => q.bind(
            n.as_i64()
                .ok_or_else(|| ApiError::BadInput(format!("field `{}` must be an integer", col.name)))?,
        ),
        Value::Number(n) if matches!(col.ty, SqlType::Real | SqlType::Double) => q.bind(
            n.as_f64()
                .ok_or_else(|| ApiError::BadInput(format!("field `{}` must be a number", col.name)))?,
        ),
        Value::String(s) => bind_string(q, col, &s)?,
        other => {
            return Err(ApiError::BadInput(format!(
                "field `{}`: unsupported JSON value `{:?}` for {:?}",
                col.name, other, col.ty
            )));
        }
    })
}

fn bind_string<'q>(q: SqlxQuery<'q>, col: &Column, s: &str) -> Result<SqlxQuery<'q>, ApiError> {
    Ok(match col.ty {
        SqlType::Text => q.bind(s.to_string()),
        SqlType::SmallInt | SqlType::Integer => q.bind(
            s.parse::<i32>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::BigInt => q.bind(
            s.parse::<i64>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Real => q.bind(
            s.parse::<f32>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Double => q.bind(
            s.parse::<f64>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Boolean => q.bind(matches!(s, "true" | "1")),
        SqlType::Date => q.bind(
            s.parse::<NaiveDate>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Time => q.bind(
            s.parse::<NaiveTime>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Timestamptz => {
            let parsed = DateTime::parse_from_rfc3339(s)
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?;
            q.bind(parsed.with_timezone(&Utc))
        }
        SqlType::Uuid => q.bind(
            Uuid::parse_str(s)
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
    })
}

fn bind_null<'q>(q: SqlxQuery<'q>, col: &Column) -> SqlxQuery<'q> {
    match col.ty {
        SqlType::SmallInt | SqlType::Integer => q.bind(None::<i32>),
        SqlType::BigInt => q.bind(None::<i64>),
        SqlType::Real => q.bind(None::<f32>),
        SqlType::Double => q.bind(None::<f64>),
        SqlType::Boolean => q.bind(None::<bool>),
        SqlType::Text => q.bind(None::<String>),
        SqlType::Date => q.bind(None::<NaiveDate>),
        SqlType::Time => q.bind(None::<NaiveTime>),
        SqlType::Timestamptz => q.bind(None::<DateTime<Utc>>),
        SqlType::Uuid => q.bind(None::<Uuid>),
    }
}
