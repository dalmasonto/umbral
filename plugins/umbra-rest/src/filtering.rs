//! Django-filter-style query-string parser for `umbra-rest`.
//!
//! Parses query-string keys of the form `<field>` or `<field>__<lookup>`
//! into a `sea_query::Condition` that the list endpoint splices into
//! the DynQuerySet via `filter_condition(...)` before applying
//! pagination.
//!
//! ## Lookup grammar
//!
//! | Suffix         | SQL             | Available on                  |
//! |----------------|-----------------|-------------------------------|
//! | (none)         | `=`             | every type                    |
//! | `__ne`         | `<>`            | every type                    |
//! | `__gte`        | `>=`            | numeric, date, datetime       |
//! | `__lte`        | `<=`            | same                          |
//! | `__gt`         | `>`             | same                          |
//! | `__lt`         | `<`             | same                          |
//! | `__in`         | `IN (...)`      | every type (comma-split)      |
//! | `__contains`   | `LIKE %v%`      | strings                       |
//! | `__icontains`  | `UPPER(col) LIKE UPPER(%v%)` | strings     |
//! | `__startswith` | `LIKE v%`       | strings                       |
//! | `__isnull`     | `IS NULL` / `IS NOT NULL` | nullable columns |
//!
//! ## Why a `sea_query::Condition` and not raw SQL?
//!
//! Plugin code never writes raw SQL — every row-level read or write
//! goes through the ORM (see `CLAUDE.md → Plugins use the ORM`). The
//! filter parser builds `sea_query` predicates, the list handler hands
//! them to `DynQuerySet::filter_condition(...)`, and the ORM emits the
//! dialect-correct SQL at terminal time. Same code path renders on
//! SQLite and Postgres.
//!
//! ## Usage
//!
//! ```ignore
//! // In ResourceConfig (opt-in via .enable_filters()):
//! ResourceConfig::new("post").enable_filters()
//!
//! // In the list handler:
//! let filter = parse_filters(&params, &model.fields, cfg.filters_enabled)?;
//! let qs = DynQuerySet::for_meta(&model);
//! let qs = if let Some(cond) = filter.into_condition() {
//!     qs.filter_condition(cond)
//! } else { qs };
//! let rows = qs.fetch_as_json().await?;
//! ```

use sea_query::{Alias, Condition, Expr, SimpleExpr};
use umbra::migrate::Column;
use umbra::orm::SqlType;

use crate::ApiError;

/// Pagination params we skip when scanning for filter keys. These are
/// consumed by the pagination layer and must not be treated as field
/// names.
const PAGINATION_KEYS: &[&str] = &["page", "page_size", "limit", "offset"];

/// A parsed filter ready to splice into a DynQuerySet.
///
/// Holds a single `sea_query::Condition` with all per-key predicates
/// ANDed together. `None` means "no filter" (no WHERE clause is
/// produced by the queryset).
#[derive(Debug, Default)]
pub(crate) struct FilterClause {
    condition: Option<Condition>,
}

impl FilterClause {
    /// True when no filters were parsed (or filters are disabled on
    /// this resource).
    pub(crate) fn is_empty(&self) -> bool {
        self.condition.is_none()
    }

    /// Consume the clause, returning the inner condition (if any).
    #[allow(dead_code)]
    pub(crate) fn into_condition(self) -> Option<Condition> {
        self.condition
    }

    /// Clone the inner condition (sea_query's Condition is Clone).
    /// Used by call sites that take `&FilterClause` and need the
    /// condition by value to hand into `DynQuerySet::filter_condition`.
    pub(crate) fn condition_clone(&self) -> Option<Condition> {
        self.condition.clone()
    }
}

// =========================================================================
// Public entry point
// =========================================================================

/// Parse every `key=value` pair in `params` that looks like a field
/// filter (`<field>` or `<field>__<lookup>`), validate the field
/// against the model's columns and the lookup against the column's
/// type, coerce the string value to a typed `sea_query` predicate,
/// and return a single `FilterClause` with all predicates ANDed
/// together.
///
/// Unknown field names, lookups that don't apply to the column type,
/// and malformed values all return `ApiError::BadInput(...)` with a
/// descriptive message.
///
/// Pagination keys (`page`, `page_size`, `limit`, `offset`) are
/// silently skipped so the caller doesn't have to pre-filter the map.
pub(crate) fn parse_filters(
    params: &std::collections::HashMap<String, String>,
    columns: &[Column],
    filters_enabled: bool,
) -> Result<FilterClause, ApiError> {
    if !filters_enabled {
        return Ok(FilterClause::default());
    }

    // Collect and sort keys for deterministic ordering (helps tests).
    let mut keys: Vec<&str> = params.keys().map(|s| s.as_str()).collect();
    keys.sort_unstable();

    let mut cond = Condition::all();
    let mut any = false;

    for key in keys {
        if PAGINATION_KEYS.contains(&key) {
            continue;
        }
        let value = params[key].as_str();

        // Split on `__<known_lookup>`. `title__icontains` → (title, icontains).
        let (field_name, lookup) = split_key(key);

        // Validate field exists on this model.
        let col = columns
            .iter()
            .find(|c| c.name == field_name)
            .ok_or_else(|| {
                ApiError::BadInput(format!(
                    "unknown field `{field_name}`; valid fields are: {}",
                    columns
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;

        // Reject empty values — they're almost always a caller mistake.
        if value.is_empty() && lookup != "isnull" {
            return Err(ApiError::BadInput(format!(
                "missing value for filter `{key}`"
            )));
        }

        validate_lookup(lookup, col)?;
        let predicate = build_predicate(col, lookup, value)?;
        cond = cond.add(predicate);
        any = true;
    }

    Ok(FilterClause {
        condition: if any { Some(cond) } else { None },
    })
}

// =========================================================================
// Internal helpers
// =========================================================================

/// Split `field__lookup` at the LAST `__` that separates a known
/// lookup suffix. If no known suffix is found, the whole key is the
/// field name and the lookup is "eq".
fn split_key(key: &str) -> (&str, &str) {
    const LOOKUPS: &[&str] = &[
        "eq",
        "ne",
        "gte",
        "lte",
        "gt",
        "lt",
        "in",
        "contains",
        "icontains",
        "startswith",
        "isnull",
    ];

    let mut last = key.len();
    while let Some(pos) = key[..last].rfind("__") {
        let candidate = &key[pos + 2..last];
        if LOOKUPS.contains(&candidate) {
            return (&key[..pos], candidate);
        }
        last = pos;
    }
    (key, "eq")
}

fn is_range_lookup(lookup: &str) -> bool {
    matches!(lookup, "gte" | "lte" | "gt" | "lt")
}

fn is_string_lookup(lookup: &str) -> bool {
    matches!(lookup, "contains" | "icontains" | "startswith")
}

fn is_numeric_or_temporal(ty: SqlType) -> bool {
    matches!(
        ty,
        SqlType::SmallInt
            | SqlType::Integer
            | SqlType::BigInt
            | SqlType::Real
            | SqlType::Double
            | SqlType::Date
            | SqlType::Timestamptz
            | SqlType::Time
    )
}

fn is_string_type(ty: SqlType) -> bool {
    matches!(ty, SqlType::Text)
}

/// Validate that `lookup` is applicable for `col`'s type. Returns
/// `BadInput` with a human-readable message on mismatch.
fn validate_lookup(lookup: &str, col: &Column) -> Result<(), ApiError> {
    if is_range_lookup(lookup) && !is_numeric_or_temporal(col.ty) {
        return Err(ApiError::BadInput(format!(
            "`{lookup}` is not available for field `{}` of type {:?}; it applies to numeric and date/datetime fields only",
            col.name, col.ty
        )));
    }
    if is_string_lookup(lookup) && !is_string_type(col.ty) {
        return Err(ApiError::BadInput(format!(
            "`{lookup}` is not available for field `{}` of type {:?}; it applies to text fields only",
            col.name, col.ty
        )));
    }
    if lookup == "isnull" && !col.nullable {
        return Err(ApiError::BadInput(format!(
            "`isnull` is not available for field `{}` because the column is NOT NULL",
            col.name
        )));
    }
    Ok(())
}

/// Build a typed `sea_query::SimpleExpr` predicate for one
/// (column, lookup, raw_value) triple. The expression carries its
/// bindings inline — the queryset binds them through `sea_query_binder`
/// at terminal time so the dialect choice (SqliteQueryBuilder vs
/// PostgresQueryBuilder) drives the placeholder rendering.
fn build_predicate(col: &Column, lookup: &str, value: &str) -> Result<SimpleExpr, ApiError> {
    let expr = Expr::col(Alias::new(&col.name));

    match lookup {
        "isnull" => {
            let is_null = parse_bool(value, &col.name)?;
            if is_null {
                Ok(expr.is_null())
            } else {
                Ok(expr.is_not_null())
            }
        }
        "in" => build_in_predicate(col, value),
        "contains" => Ok(expr.like(format!("%{value}%"))),
        "icontains" => {
            // `UPPER(col) LIKE UPPER(?)` — case-insensitive contains.
            // sea_query's `Expr::expr(...)` lets us nest UPPER around
            // the column.
            Ok(Expr::expr(
                sea_query::Func::upper(Expr::col(Alias::new(&col.name))),
            )
            .like(format!("%{}%", value.to_uppercase())))
        }
        "startswith" => Ok(expr.like(format!("{value}%"))),
        op => {
            let sea_value = coerce_value(col, value)?;
            match op {
                "eq" => Ok(expr.eq(sea_value)),
                "ne" => Ok(expr.ne(sea_value)),
                "gte" => Ok(expr.gte(sea_value)),
                "lte" => Ok(expr.lte(sea_value)),
                "gt" => Ok(expr.gt(sea_value)),
                "lt" => Ok(expr.lt(sea_value)),
                other => Err(ApiError::BadInput(format!("unknown lookup `{other}`"))),
            }
        }
    }
}

/// Build an `IN (?, ?, ...)` predicate from a comma-separated value.
fn build_in_predicate(col: &Column, value: &str) -> Result<SimpleExpr, ApiError> {
    let parts: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
    if parts.is_empty() || (parts.len() == 1 && parts[0].is_empty()) {
        return Err(ApiError::BadInput(format!(
            "field `{}`: `in` lookup requires at least one value (comma-separated)",
            col.name
        )));
    }

    let expr = Expr::col(Alias::new(&col.name));
    match col.ty {
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::ForeignKey => {
            let mut ints: Vec<i64> = Vec::with_capacity(parts.len());
            for p in &parts {
                let n = p.parse::<i64>().map_err(|_| {
                    ApiError::BadInput(format!(
                        "field `{}`: cannot parse `{p}` as integer for `in` lookup",
                        col.name
                    ))
                })?;
                ints.push(n);
            }
            Ok(expr.is_in(ints))
        }
        _ => {
            let texts: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
            Ok(expr.is_in(texts))
        }
    }
}

/// Coerce a query-string value to the typed `sea_query::Value` the
/// predicate expects. The conversion validates the input shape (a
/// non-numeric string for an integer column returns a 400) and runs
/// the same dispatch as the ORM's `json_to_sea_value` would for the
/// equivalent JSON input.
fn coerce_value(col: &Column, value: &str) -> Result<sea_query::Value, ApiError> {
    let v = match col.ty {
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::ForeignKey => {
            let n = value.parse::<i64>().map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as integer",
                    col.name
                ))
            })?;
            sea_query::Value::BigInt(Some(n))
        }
        SqlType::Real | SqlType::Double => {
            let f = value.parse::<f64>().map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as number",
                    col.name
                ))
            })?;
            sea_query::Value::Double(Some(f))
        }
        SqlType::Boolean => sea_query::Value::Bool(Some(parse_bool(value, &col.name)?)),
        SqlType::Date => {
            // Validate the shape, then store as ISO-8601 text. Both
            // backends accept the text form for ordering / equality on
            // date columns (sea_query also accepts NaiveDate directly,
            // but routing through text keeps the parse error close to
            // the user input).
            value.parse::<chrono::NaiveDate>().map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as ISO-8601 date (YYYY-MM-DD)",
                    col.name
                ))
            })?;
            sea_query::Value::String(Some(Box::new(value.to_string())))
        }
        SqlType::Timestamptz => {
            chrono::DateTime::parse_from_rfc3339(value).map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as RFC-3339 datetime",
                    col.name
                ))
            })?;
            sea_query::Value::String(Some(Box::new(value.to_string())))
        }
        SqlType::Time => {
            value.parse::<chrono::NaiveTime>().map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as HH:MM:SS time",
                    col.name
                ))
            })?;
            sea_query::Value::String(Some(Box::new(value.to_string())))
        }
        SqlType::Uuid => {
            uuid::Uuid::parse_str(value).map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as UUID",
                    col.name
                ))
            })?;
            sea_query::Value::String(Some(Box::new(value.to_string())))
        }
        SqlType::Text => sea_query::Value::String(Some(Box::new(value.to_string()))),
        SqlType::Json => sea_query::Value::String(Some(Box::new(value.to_string()))),
        SqlType::Array(_)
        | SqlType::Inet
        | SqlType::Cidr
        | SqlType::MacAddr
        | SqlType::FullText => {
            return Err(ApiError::BadInput(format!(
                "field `{}`: filtering on {:?} columns is not supported",
                col.name, col.ty
            )));
        }
    };
    Ok(v)
}

/// Parse "true" / "1" → true, "false" / "0" → false. Everything else
/// is a 400.
fn parse_bool(value: &str, field_name: &str) -> Result<bool, ApiError> {
    match value {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(ApiError::BadInput(format!(
            "field `{field_name}`: cannot parse `{value}` as boolean; use `true`/`false`"
        ))),
    }
}
