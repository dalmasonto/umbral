//! Row marshalling — turn `ModelMeta` + dynamic column lists into
//! parameterized SQL, bind form values, and decode result rows into
//! `HashMap<String, String>` for the templates.
//!
//! The read-side queries (`count_rows_filtered`, `fetch_rows_paged`)
//! now go through [`umbra::orm::DynQuerySet`] — the runtime-typed
//! Manager that lives in `umbra-core`. The write-side functions
//! (`insert_row`, `update_row`, the SQLite-row decoder `column_to_string`,
//! the form-value binder `bind_form_value`, the typed-NULL binder
//! `bind_null`) still hand-build SQL because the ORM extension's
//! write path is the next pass.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde_json::Value;
use sqlx::{Row, SqlitePool};
use umbra::migrate::{Column, ModelMeta};
use umbra::orm::{DynQuerySet, SqlType};
use uuid::Uuid;

use crate::AdminError;
use crate::config::AdminConfig;
use crate::q;

/// COUNT(*) for one filtered changelist query. Returns the total so
/// the Pagination footer can compute total_pages.
///
/// Backed by [`DynQuerySet`] — the search / filter clause comes from
/// the same builder the row fetch uses, so the count and the page
/// agree on what "filtered" means.
pub(crate) async fn count_rows_filtered(
    _pool: &SqlitePool,
    model: &ModelMeta,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filter: Option<(&str, &str)>,
) -> Result<usize, AdminError> {
    let mut qs = DynQuerySet::for_meta(model);
    if let Some(term) = search_term
        && let Some(c) = cfg
        && !c.search_fields.is_empty()
    {
        qs = qs.search(&c.search_fields, term);
    }
    if let Some((field, value)) = active_filter {
        qs = qs.filter_eq_string(field, value);
    }
    let count = qs.count().await?;
    Ok(count as usize)
}

/// Fetch one page of rows for the changelist. Phase 2's paginated
/// counterpart to `fetch_rows_filtered`.
///
/// Backed by [`DynQuerySet`]. `order_clause` carries the same
/// pre-built ORDER BY string the legacy path used (single
/// `"col" ASC|DESC` or comma-joined multi-column); we parse it back
/// out into `(col, descending)` pairs and feed each to
/// `order_by_col` so the ORM owns the rendering.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_rows_paged(
    _pool: &SqlitePool,
    model: &ModelMeta,
    display_cols: &[String],
    order_clause: &str,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filter: Option<(&str, &str)>,
    limit: usize,
    offset: usize,
) -> Result<Vec<HashMap<String, String>>, AdminError> {
    let mut qs = DynQuerySet::for_meta(model).select_cols(display_cols);
    if let Some(term) = search_term
        && let Some(c) = cfg
        && !c.search_fields.is_empty()
    {
        qs = qs.search(&c.search_fields, term);
    }
    if let Some((field, value)) = active_filter {
        qs = qs.filter_eq_string(field, value);
    }
    for (col, desc) in parse_order_clause(order_clause) {
        qs = qs.order_by_col(&col, desc);
    }
    qs = qs.limit(limit as u64).offset(offset as u64);
    Ok(qs.fetch_as_strings().await?)
}

/// Parse the legacy `"col" ASC, "col2" DESC` ORDER BY string back into
/// `(column_name, descending)` pairs. Whitespace tolerant; segments
/// that don't parse are silently dropped.
fn parse_order_clause(clause: &str) -> Vec<(String, bool)> {
    if clause.trim().is_empty() {
        return Vec::new();
    }
    clause
        .split(',')
        .filter_map(|part| {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Format: `"col" ASC` or `"col" DESC`.
            let (col_part, dir_part) = trimmed.rsplit_once(' ')?;
            let col = col_part.trim().trim_matches('"');
            if col.is_empty() {
                return None;
            }
            let descending = dir_part.trim().eq_ignore_ascii_case("DESC");
            Some((col.to_string(), descending))
        })
        .collect()
}

/// Pre-Phase-2 fetch path: hard-capped at 200 rows. Still used by the
/// detail page (`where_pk = Some(...)`) and by the legacy list view
/// before the paged variant was introduced. Kept until every call site
/// migrates to `fetch_rows_paged`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_rows_filtered(
    pool: &SqlitePool,
    model: &ModelMeta,
    where_pk: Option<(&str, &str)>,
    display_cols: &[String],
    order_clause: &str,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filter: Option<(&str, &str)>,
) -> Result<Vec<HashMap<String, String>>, AdminError> {
    let valid_names: std::collections::HashSet<&str> =
        model.fields.iter().map(|c| c.name.as_str()).collect();
    let columns = display_cols
        .iter()
        .filter(|n| valid_names.contains(n.as_str()))
        .map(|n| format!("\"{}\"", n))
        .collect::<Vec<_>>()
        .join(", ");
    let columns = if columns.is_empty() {
        model
            .fields
            .iter()
            .map(|c| format!("\"{}\"", c.name))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        columns
    };

    let mut conditions: Vec<String> = Vec::new();
    let mut bind_strings: Vec<String> = Vec::new();

    if let Some((col, _val)) = where_pk {
        conditions.push(format!("\"{}\" = ?", q(col)));
        bind_strings.push(where_pk.unwrap().1.to_string());
    }

    if let Some(term) = search_term
        && let Some(c) = cfg
        && !c.search_fields.is_empty()
    {
        let like_clauses: Vec<String> = c
            .search_fields
            .iter()
            .filter(|f| valid_names.contains(f.as_str()))
            .map(|f| format!("\"{}\" LIKE ?", q(f)))
            .collect();
        if !like_clauses.is_empty() {
            conditions.push(format!("({})", like_clauses.join(" OR ")));
            let like_val = format!("%{term}%");
            for _ in 0..like_clauses.len() {
                bind_strings.push(like_val.clone());
            }
        }
    }

    if let Some((field, value)) = active_filter {
        if valid_names.contains(field) {
            conditions.push(format!("\"{}\" = ?", q(field)));
            bind_strings.push(value.to_string());
        }
    }

    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };
    let order_sql = if order_clause.is_empty() || where_pk.is_some() {
        String::new()
    } else {
        format!(" ORDER BY {order_clause}")
    };
    let limit_sql = if where_pk.is_some() {
        " LIMIT 1"
    } else {
        " LIMIT 200"
    };

    let sql = format!(
        "SELECT {columns} FROM \"{}\"{where_sql}{order_sql}{limit_sql}",
        q(&model.table)
    );

    let mut qb = sqlx::query(&sql);
    for val in &bind_strings {
        qb = qb.bind(val.clone());
    }

    let rows = qb.fetch_all(pool).await?;
    let mut out: Vec<HashMap<String, String>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut entry: HashMap<String, String> = HashMap::new();
        for col_name in display_cols {
            if let Some(col) = model.fields.iter().find(|c| &c.name == col_name) {
                entry.insert(col.name.clone(), column_to_string(&row, col)?);
            }
        }
        out.push(entry);
    }
    Ok(out)
}

/// Decode one cell to its template-friendly string form. The branch
/// per `SqlType` mirrors `bind_form_value`'s parse step in reverse.
pub(crate) fn column_to_string(
    row: &sqlx::sqlite::SqliteRow,
    col: &Column,
) -> Result<String, AdminError> {
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

/// The Array / network-type branches should be unreachable after the
/// M4 system check — the panic is the loud failure for the case where
/// someone bypasses the check and ships an incompatible field on SQLite.
fn panic_array_unsupported(column: &str) -> ! {
    panic!(
        "umbra-admin: column `{column}` is a Postgres-only Array; the \
         field.backend system check should have failed boot."
    )
}

fn panic_pg_only_unsupported(column: &str) -> ! {
    panic!(
        "umbra-admin: column `{column}` is a Postgres-only network type \
         (Inet/Cidr/MacAddr); the field.backend system check should \
         have failed boot."
    )
}

/// INSERT one form submission. Handles `password_field` (hash + confirm
/// check) before binding and respects the merged `readonly` set
/// (config + sensitive-column defaults) so the server can't be tricked
/// into writing fields the form was supposed to skip.
pub(crate) async fn insert_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
) -> Result<(), AdminError> {
    let form_owned: HashMap<String, String>;
    let form = if let Some(pw_col) = cfg.and_then(|c| c.password_field.as_deref()) {
        if let Some(plaintext) = form.get(pw_col).filter(|v| !v.is_empty()) {
            let confirm_key = format!("{pw_col}_confirm");
            let confirm = form.get(&confirm_key).map(|s| s.as_str()).unwrap_or("");
            if plaintext != confirm {
                return Err(AdminError::BadInput("Passwords do not match.".to_string()));
            }
            let hash = umbra_auth::hash_password(plaintext)
                .map_err(|e| AdminError::BadInput(format!("password hashing failed: {e}")))?;
            let mut owned = form.clone();
            owned.insert(pw_col.to_string(), hash);
            form_owned = owned;
            &form_owned
        } else {
            form
        }
    } else {
        form
    };

    let all_col_names: Vec<&str> = model.fields.iter().map(|c| c.name.as_str()).collect();
    let readonly_owned: Vec<String> = if let Some(c) = cfg {
        c.effective_readonly_fields(&all_col_names)
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        all_col_names
            .iter()
            .filter(|n| crate::config::is_sensitive_column(n))
            .map(|s| s.to_string())
            .collect()
    };
    let readonly: std::collections::HashSet<&str> =
        readonly_owned.iter().map(|s| s.as_str()).collect();
    let writable: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| {
            !(readonly.contains(c.name.as_str())
                || (c.primary_key
                    && matches!(c.ty, SqlType::Integer | SqlType::BigInt | SqlType::SmallInt)
                    && form.get(&c.name).is_none_or(|v| v.is_empty())))
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
        q(&model.table)
    );
    let mut qb = sqlx::query(&sql);
    for col in &writable {
        qb = bind_form_value(qb, col, form)?;
    }
    qb.execute(pool).await?;
    Ok(())
}

/// UPDATE one row identified by its PK. Same readonly enforcement as
/// `insert_row` — fields can't be smuggled back in via the form.
pub(crate) async fn update_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    pk: &Column,
    pk_value: &str,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
) -> Result<(), AdminError> {
    let all_col_names: Vec<&str> = model.fields.iter().map(|c| c.name.as_str()).collect();
    let readonly_owned: Vec<String> = if let Some(c) = cfg {
        c.effective_readonly_fields(&all_col_names)
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        all_col_names
            .iter()
            .filter(|n| crate::config::is_sensitive_column(n))
            .map(|s| s.to_string())
            .collect()
    };
    let readonly: std::collections::HashSet<&str> =
        readonly_owned.iter().map(|s| s.as_str()).collect();
    let writable: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| !c.primary_key && !readonly.contains(c.name.as_str()))
        .collect();
    let setters = writable
        .iter()
        .map(|c| format!("\"{}\" = ?", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE \"{}\" SET {setters} WHERE \"{}\" = ?",
        q(&model.table),
        q(&pk.name)
    );
    let mut qb = sqlx::query(&sql);
    for col in &writable {
        qb = bind_form_value(qb, col, form)?;
    }
    qb = qb.bind(pk_value.to_string());
    qb.execute(pool).await?;
    Ok(())
}

/// Bind one form value to a sqlx query at its native type. Drives the
/// `SqlType`-typed parse step that turns the form's `String` into the
/// concrete Rust value sqlx will encode for the backend. Used by
/// `insert_row` / `update_row` and by the inline-cell-edit handler.
pub(crate) fn bind_form_value<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    col: &Column,
    form: &HashMap<String, String>,
) -> Result<sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>, AdminError> {
    let raw = form.get(&col.name).cloned().unwrap_or_default();
    if raw.is_empty() {
        return Ok(match col.ty {
            SqlType::Boolean => q.bind(false),
            _ if col.nullable => bind_null(q, col),
            _ => {
                return Err(AdminError::BadInput(format!(
                    "field `{}` is required",
                    col.name
                )));
            }
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => q.bind(
            raw.parse::<i32>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::BigInt => q.bind(
            raw.parse::<i64>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Real => q.bind(
            raw.parse::<f32>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Double => q.bind(
            raw.parse::<f64>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Boolean => q.bind(matches!(raw.as_str(), "true" | "on" | "1")),
        SqlType::Text => q.bind(raw),
        SqlType::Date => q.bind(
            raw.parse::<NaiveDate>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Time => q.bind(
            raw.parse::<NaiveTime>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Timestamptz => {
            let s = if raw.contains(':') && !raw.contains('+') && !raw.ends_with('Z') {
                format!("{raw}:00Z")
            } else {
                raw.clone()
            };
            let parsed = DateTime::parse_from_rfc3339(&s)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?;
            q.bind(parsed.with_timezone(&Utc))
        }
        SqlType::Uuid => q.bind(
            Uuid::parse_str(&raw)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Json => q.bind(
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        SqlType::ForeignKey => q.bind(
            raw.parse::<i64>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
    })
}

/// Bind a typed `NULL` for an empty nullable column. Per-`SqlType`
/// because sqlx needs the concrete type even for NULL.
fn bind_null<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    col: &Column,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
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
        SqlType::Json => q.bind(None::<Value>),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        SqlType::ForeignKey => q.bind(None::<i64>),
    }
}
