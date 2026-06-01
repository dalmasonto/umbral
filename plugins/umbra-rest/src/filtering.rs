//! Django-filter-style query-string parser for `umbra-rest`.
//!
//! Parses query-string keys of the form `<field>` or `<field>__<lookup>`
//! into SQL WHERE-clause fragments that the list endpoint ANDs together
//! before applying pagination.
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
//! ## Usage
//!
//! ```ignore
//! // In ResourceConfig (opt-in via .enable_filters()):
//! ResourceConfig::new("post").enable_filters()
//!
//! // In the list handler:
//! let filter = parse_filters(&params, &model, &cfg)?;
//! // pass filter.where_sql and filter.bindings to fetch_rows_filtered
//! ```

use umbra::migrate::Column;
use umbra::orm::SqlType;

use crate::ApiError;

/// Pagination params we skip when scanning for filter keys. These are
/// consumed by the pagination layer and must not be treated as field
/// names.
const PAGINATION_KEYS: &[&str] = &["page", "page_size", "limit", "offset"];

/// A parsed filter ready to splice into a SQL query.
///
/// `where_sql` is a possibly-empty fragment like
/// `"published" = ? AND "title" LIKE ?`. When empty, no WHERE clause
/// is added. `bindings` holds the positional values in the same order
/// as the `?` placeholders in `where_sql`.
#[derive(Debug, Default)]
pub(crate) struct FilterClause {
    /// The WHERE-clause body (without the `WHERE` keyword). Empty
    /// string means "no filter". Caller appends ` WHERE <where_sql>`
    /// when non-empty.
    pub(crate) where_sql: String,
    /// Positional bindings in `?` order.
    pub(crate) bindings: Vec<FilterValue>,
}

impl FilterClause {
    pub(crate) fn is_empty(&self) -> bool {
        self.where_sql.is_empty()
    }
}

/// A single binding value for a filter predicate.
///
/// `sqlx` requires static dispatch for each primitive; the list handler
/// matches on these variants to call the right `.bind()` overload.
#[derive(Debug, Clone)]
pub(crate) enum FilterValue {
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Multiple text fragments for `IN (...)` — stored as a flat vec
    /// of strings; the handler re-binds each one in order after
    /// expanding `IN (?, ?, ?)` in the SQL.
    InTexts(Vec<String>),
    InInts(Vec<i64>),
}

// =========================================================================
// Public entry point
// =========================================================================

/// Parse every `key=value` pair in `params` that looks like a field
/// filter (`<field>` or `<field>__<lookup>`), validate the field
/// against the model's columns and the lookup against the column's
/// type, coerce the string value to the typed binding, and return a
/// single `FilterClause` with all predicates ANDed together.
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

    let mut parts: Vec<String> = Vec::new();
    let mut bindings: Vec<FilterValue> = Vec::new();

    // Collect and sort keys for deterministic ordering (helps tests).
    let mut keys: Vec<&str> = params.keys().map(|s| s.as_str()).collect();
    keys.sort_unstable();

    for key in keys {
        if PAGINATION_KEYS.contains(&key) {
            continue;
        }
        let value = params[key].as_str();

        // Split on the first `__` (only). `title__icontains` → (title, icontains).
        // `created_at__gte` → (created_at, gte). Plain `published` → (published, eq).
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

        // Validate that the lookup makes sense for this column type.
        validate_lookup(lookup, col)?;

        // Build the SQL fragment and binding(s).
        let (sql, vals) = build_predicate(col, lookup, value)?;
        parts.push(sql);
        bindings.extend(vals);
    }

    let where_sql = parts.join(" AND ");
    Ok(FilterClause {
        where_sql,
        bindings,
    })
}

// =========================================================================
// Internal helpers
// =========================================================================

/// Split `field__lookup` at the LAST `__` that separates a known
/// lookup suffix. If no known suffix is found, the whole key is the
/// field name and the lookup is "eq".
///
/// We split on the last `__` occurrence that matches a known lookup
/// name, not the first, so column names that contain `__` (unusual
/// but possible) don't confuse the parser.
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

    // Walk from right to left looking for `__<known_lookup>`.
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

/// True when the lookup is a range/ordering op (`gte`, `lte`, `gt`,
/// `lt`). These only make sense on numeric, date, and datetime types.
fn is_range_lookup(lookup: &str) -> bool {
    matches!(lookup, "gte" | "lte" | "gt" | "lt")
}

/// True when the lookup is a string pattern op.
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

/// Build the SQL predicate fragment and its binding values for one
/// (column, lookup, raw_value) triple.
///
/// Returns `(sql_fragment, bindings)` where `sql_fragment` uses `?`
/// placeholders. For `IN` lookups with N values, the fragment contains
/// N placeholders.
fn build_predicate(
    col: &Column,
    lookup: &str,
    value: &str,
) -> Result<(String, Vec<FilterValue>), ApiError> {
    let col_sql = format!("\"{}\"", col.name.replace('"', "\"\""));

    match lookup {
        "isnull" => {
            let is_null = parse_bool(value, &col.name)?;
            let fragment = if is_null {
                format!("{col_sql} IS NULL")
            } else {
                format!("{col_sql} IS NOT NULL")
            };
            Ok((fragment, vec![]))
        }
        "in" => build_in_predicate(col, &col_sql, value),
        "contains" => {
            let pattern = format!("%{value}%");
            Ok((
                format!("{col_sql} LIKE ?"),
                vec![FilterValue::Text(pattern)],
            ))
        }
        "icontains" => {
            let pattern = format!("%{}%", value.to_uppercase());
            Ok((
                format!("UPPER({col_sql}) LIKE ?"),
                vec![FilterValue::Text(pattern)],
            ))
        }
        "startswith" => {
            let pattern = format!("{value}%");
            Ok((
                format!("{col_sql} LIKE ?"),
                vec![FilterValue::Text(pattern)],
            ))
        }
        op => {
            let sql_op = match op {
                "eq" => "=",
                "ne" => "<>",
                "gte" => ">=",
                "lte" => "<=",
                "gt" => ">",
                "lt" => "<",
                other => {
                    return Err(ApiError::BadInput(format!("unknown lookup `{other}`")));
                }
            };
            let (val, binding) = coerce_value(col, value)?;
            Ok((format!("{col_sql} {sql_op} {val}"), vec![binding]))
        }
    }
}

/// Build an `IN (?, ?, ...)` predicate from a comma-separated value
/// string.
fn build_in_predicate(
    col: &Column,
    col_sql: &str,
    value: &str,
) -> Result<(String, Vec<FilterValue>), ApiError> {
    let parts: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
    if parts.is_empty() || (parts.len() == 1 && parts[0].is_empty()) {
        return Err(ApiError::BadInput(format!(
            "field `{}`: `in` lookup requires at least one value (comma-separated)",
            col.name
        )));
    }

    match col.ty {
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::ForeignKey => {
            let mut ints = Vec::with_capacity(parts.len());
            for p in &parts {
                let n = p.parse::<i64>().map_err(|_| {
                    ApiError::BadInput(format!(
                        "field `{}`: cannot parse `{p}` as integer for `in` lookup",
                        col.name
                    ))
                })?;
                ints.push(n);
            }
            let placeholders = vec!["?"; ints.len()].join(", ");
            Ok((
                format!("{col_sql} IN ({placeholders})"),
                vec![FilterValue::InInts(ints)],
            ))
        }
        _ => {
            let texts: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
            let placeholders = vec!["?"; texts.len()].join(", ");
            Ok((
                format!("{col_sql} IN ({placeholders})"),
                vec![FilterValue::InTexts(texts)],
            ))
        }
    }
}

/// Coerce a query-string value to a typed binding.
///
/// Returns `("?", FilterValue)` for the vast majority of types.
/// Returns a `("true"/"false"/"1"/"0", FilterValue::Bool(_))` for
/// booleans — SQLite stores booleans as integers 0/1 and the direct
/// `= true` comparison can trip up; binding a `bool` through sqlx
/// handles the translation.
fn coerce_value(col: &Column, value: &str) -> Result<(&'static str, FilterValue), ApiError> {
    let binding = match col.ty {
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::ForeignKey => {
            let n = value.parse::<i64>().map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as integer",
                    col.name
                ))
            })?;
            FilterValue::Int(n)
        }
        SqlType::Real | SqlType::Double => {
            let f = value.parse::<f64>().map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as number",
                    col.name
                ))
            })?;
            FilterValue::Float(f)
        }
        SqlType::Boolean => {
            let b = parse_bool(value, &col.name)?;
            FilterValue::Bool(b)
        }
        SqlType::Date => {
            // Store date as TEXT in SQLite; equality / range comparisons on
            // ISO-8601 strings work because the sort order matches.
            // Validate it really is a date so bad inputs get a 400.
            value.parse::<chrono::NaiveDate>().map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as ISO-8601 date (YYYY-MM-DD)",
                    col.name
                ))
            })?;
            FilterValue::Text(value.to_string())
        }
        SqlType::Timestamptz => {
            // Same pattern: validate + store as ISO-8601 string for SQLite.
            chrono::DateTime::parse_from_rfc3339(value).map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as RFC-3339 datetime",
                    col.name
                ))
            })?;
            FilterValue::Text(value.to_string())
        }
        SqlType::Time => {
            value.parse::<chrono::NaiveTime>().map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as HH:MM:SS time",
                    col.name
                ))
            })?;
            FilterValue::Text(value.to_string())
        }
        SqlType::Uuid => {
            uuid::Uuid::parse_str(value).map_err(|_| {
                ApiError::BadInput(format!(
                    "field `{}`: cannot parse `{value}` as UUID",
                    col.name
                ))
            })?;
            FilterValue::Text(value.to_string())
        }
        SqlType::Text => FilterValue::Text(value.to_string()),
        SqlType::Json => FilterValue::Text(value.to_string()),
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
    Ok(("?", binding))
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
