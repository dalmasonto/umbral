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

use sea_query::{Alias, Condition, Expr, Func, SimpleExpr};
use umbra::migrate::Column;
use umbra::orm::SqlType;

use crate::ApiError;

/// Query-string keys consumed elsewhere (pagination layer + the
/// free-text search handler + sparse fieldset + ordering), skipped
/// when scanning for filter keys so the parser doesn't reject them
/// as "unknown field".
const RESERVED_KEYS: &[&str] = &[
    "page",
    "page_size",
    "limit",
    "offset",
    "search",
    "fields",
    "ordering",
    // `?include=fk1,fk2` — consumed by the include parser in
    // lib.rs::parse_include for FK expansion via select_related.
    // Skipped here so the filter parser doesn't mistake it for a
    // column name and reject as "unknown field".
    "include",
    // `?format=csv` — consumed by the list handler to switch the
    // response serialization (feature #61); not a column filter.
    "format",
];

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

    /// AND-merge an additional condition into this clause. Used by the
    /// list handler to splice the `?search=` predicate alongside the
    /// parsed `?field=` / `?field__lookup=` filter set without losing
    /// either layer.
    pub(crate) fn and(mut self, extra: Condition) -> Self {
        self.condition = Some(match self.condition.take() {
            Some(c) => c.add(extra),
            None => Condition::all().add(extra),
        });
        self
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
        if RESERVED_KEYS.contains(&key) {
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
        validate_choices(col, lookup, value)?;
        let predicate = build_predicate(col, lookup, value)?;
        cond = cond.add(predicate);
        any = true;
    }

    Ok(FilterClause {
        condition: if any { Some(cond) } else { None },
    })
}

/// Build the OR-condition for a `?search=<term>` query string against
/// every searchable column. Returns `None` when:
/// - `term` is empty;
/// - no columns are searchable (e.g. all integer with a non-numeric
///   term, or all blocked types);
/// - `restrict_to` is `Some` but no column in the model matches.
///
/// The shape per column type:
///
/// | type | predicate when term applies |
/// |---|---|
/// | `Text` | `UPPER(col) LIKE UPPER('%term%')` (icontains) |
/// | `SmallInt` / `Integer` / `BigInt` / `ForeignKey` | `col = term` when `term.parse::<i64>().is_ok()` |
/// | `Real` / `Double` | `col = term` when `term.parse::<f64>().is_ok()` |
/// | `Boolean` | `col = term` when `term` is `true` / `false` |
///
/// All other types (Date, Time, Uuid, Json, Bytes, Array, …) skip —
/// parsing them from a free-text term is ambiguous and rarely what
/// the user wants. Add explicit filter parameters for those.
///
/// When `restrict_to` is supplied, only columns whose name appears in
/// the slice participate. Use this to honour
/// `ResourceConfig::search_fields(...)` opt-in lists.
pub(crate) fn parse_search(
    term: &str,
    columns: &[Column],
    restrict_to: Option<&[String]>,
) -> Option<Condition> {
    let term = term.trim();
    if term.is_empty() {
        return None;
    }

    let as_int = term.parse::<i64>().ok();
    let as_float = term.parse::<f64>().ok();
    let as_bool = match term.to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };

    let mut or = Condition::any();
    let mut any = false;

    for col in columns {
        if let Some(allow) = restrict_to {
            if !allow.iter().any(|n| n == &col.name) {
                continue;
            }
        }

        // Match on the column's effective type: a FK to a String/Uuid-PK
        // target resolves to that target type (review #4), so a FK-to-slug
        // column is searched as text (icontains) rather than coerced to int
        // and skipped.
        let predicate: Option<SimpleExpr> = match umbra::migrate::fk_effective_type(col) {
            SqlType::Text => {
                // Escape LIKE wildcards so `%`/`_` in the term match
                // literally, not as wildcards (ORM-1).
                let pattern = format!("%{}%", umbra::orm::escape_like_literal(term)).to_uppercase();
                Some(
                    Expr::expr(Func::upper(Expr::col(Alias::new(&col.name))))
                        .like(sea_query::LikeExpr::new(pattern).escape('\\')),
                )
            }
            SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::ForeignKey => {
                as_int.map(|n| Expr::col(Alias::new(&col.name)).eq(n))
            }
            SqlType::Real | SqlType::Double => {
                as_float.map(|n| Expr::col(Alias::new(&col.name)).eq(n))
            }
            SqlType::Boolean => as_bool.map(|b| Expr::col(Alias::new(&col.name)).eq(b)),
            // Full-text search: the tsvector is the purpose-built,
            // GIN-indexed search column, so `?search=` runs word-aware FTS
            // on it — `col @@ websearch_to_tsquery($term)`, the same operator
            // `FullTextCol::matches_websearch` uses. Built as a native binary
            // expr (NOT cust_with_values) so its bound term orders correctly
            // when OR'd with the LIKE / eq predicates from other columns.
            // Postgres-only, which is correct — FullText can't exist on SQLite.
            SqlType::FullText => {
                let q = Func::cust(Alias::new("websearch_to_tsquery")).arg(term.to_string());
                Some(Expr::col(Alias::new(&col.name)).binary(sea_query::BinOper::Custom("@@"), q))
            }
            // Date / Time / Timestamptz / Uuid / Json / Bytes / Array /
            // network types: free-text matching is ambiguous; callers can
            // hit those with the typed `?col__eq=` filter.
            _ => None,
        };

        if let Some(p) = predicate {
            or = or.add(p);
            any = true;
        }
    }

    if any { Some(or) } else { None }
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

/// Public view of the lookups applicable to a column. Used by
/// `umbra-openapi` to emit per-column query parameters on list
/// endpoints — clients (Swagger UI, codegen, the umbra-playground)
/// can then discover what `?<field>__<lookup>=` keys are valid
/// without re-implementing the validation rules here.
///
/// Ordering: most-common first (eq, then range, then string-shape,
/// then set-membership, then nullability). Stable across calls so
/// the emitted OpenAPI parameter ordering is deterministic.
///
/// Skips columns the list handler can't filter on (no rule today, but
/// the function shape leaves room to add `noform`-style opt-outs).
pub fn applicable_lookups(col: &Column) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::with_capacity(8);
    // eq + ne work for everything.
    out.push("eq");
    out.push("ne");
    if is_numeric_or_temporal(col.ty) {
        out.extend(["gte", "lte", "gt", "lt"]);
    }
    if is_string_type(col.ty) {
        out.extend(["contains", "icontains", "startswith"]);
    }
    // `__in` is universally accepted (CSV-split of any scalar type).
    out.push("in");
    if col.nullable {
        out.push("isnull");
    }
    out
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

/// Validate that the supplied value matches one of the column's
/// declared `choices` when the column is a closed-set (enum) column.
/// The API is the source of truth — without this check, a typo like
/// `?status=pub` against an enum whose values are `draft|published|...`
/// silently returns zero rows instead of telling the caller the value
/// is wrong, leaving them debugging an empty list.
///
/// Skips when:
/// - the column has no `choices` (regular text/int columns);
/// - the column is `is_multichoice` — its stored values are CSV
///   subsets, so `__eq` / `__in` semantics differ and the per-value
///   check would over-reject;
/// - the lookup is a substring shape (`contains`, `icontains`,
///   `startswith`) where partial matches against a choice are
///   legitimate (`?status__startswith=pub` is a fuzzy search, not
///   an assertion of equality);
/// - the lookup is `isnull` — its value is a boolean, not a choice.
///
/// For `__in` the value is comma-separated; every CSV element gets
/// validated separately so the error message names exactly which
/// pieces failed.
fn validate_choices(col: &Column, lookup: &str, value: &str) -> Result<(), ApiError> {
    if col.choices.is_empty() || col.is_multichoice {
        return Ok(());
    }
    if matches!(lookup, "isnull" | "contains" | "icontains" | "startswith") {
        return Ok(());
    }

    // `__in` splits on `,`; everything else is a single value.
    let candidates: Vec<&str> = if lookup == "in" {
        value
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![value]
    };

    let invalid: Vec<&str> = candidates
        .iter()
        .filter(|v| !col.choices.iter().any(|c| c == *v))
        .copied()
        .collect();

    if invalid.is_empty() {
        return Ok(());
    }

    let allowed = col.choices.join(", ");
    let supplied = invalid.join(", ");
    let plural = if invalid.len() == 1 { "" } else { "s" };
    Err(ApiError::BadInput(format!(
        "value{plural} `{supplied}` not in the allowed choices for `{}`; valid values: {allowed}",
        col.name,
    )))
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
        // The contains/icontains/startswith lookups treat `value` as a
        // literal substring, so LIKE wildcards (`%`, `_`) in it must be
        // escaped — otherwise `?name__contains=100%` over-matches (ORM-1).
        // Paired with `.escape('\\')`. `eq`/`ne`/etc. bind the value
        // directly and need no escaping.
        "contains" => {
            let pat = format!("%{}%", umbra::orm::escape_like_literal(value));
            Ok(expr.like(sea_query::LikeExpr::new(pat).escape('\\')))
        }
        "icontains" => {
            // `UPPER(col) LIKE UPPER(?)` — case-insensitive contains.
            // sea_query's `Expr::expr(...)` lets us nest UPPER around
            // the column.
            let pat = format!("%{}%", umbra::orm::escape_like_literal(value)).to_uppercase();
            Ok(
                Expr::expr(sea_query::Func::upper(Expr::col(Alias::new(&col.name))))
                    .like(sea_query::LikeExpr::new(pat).escape('\\')),
            )
        }
        "startswith" => {
            let pat = format!("{}%", umbra::orm::escape_like_literal(value));
            Ok(expr.like(sea_query::LikeExpr::new(pat).escape('\\')))
        }
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
    // FK columns to a String/Uuid-PK target store TEXT/uuid, not BIGINT —
    // build the IN-list against the target PK type (review #4), so codename
    // or uuid FK values aren't rejected as "not an integer".
    match umbra::migrate::fk_effective_type(col) {
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
    // FK columns coerce by their target PK type (review #4): a FK to a
    // String/Uuid-PK target routes to the Text/Uuid arm below instead of
    // being parsed as an integer and 400'd.
    let v = match umbra::migrate::fk_effective_type(col) {
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
        | SqlType::FullText
        | SqlType::Bytes
        | SqlType::Decimal => {
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

#[cfg(test)]
mod search {
    use super::*;

    fn col(name: &str, ty: SqlType) -> Column {
        Column {
            name: name.into(),
            ty,
            primary_key: false,
            nullable: false,
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
            on_delete: ::umbra::orm::FkAction::NoAction,
            on_update: ::umbra::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        }
    }

    fn columns() -> Vec<Column> {
        vec![
            col("id", SqlType::BigInt),
            col("title", SqlType::Text),
            col("body", SqlType::Text),
            col("author_id", SqlType::ForeignKey),
            col("published", SqlType::Boolean),
            col("score", SqlType::Double),
            col("created_at", SqlType::Timestamptz),
        ]
    }

    #[test]
    fn empty_term_returns_none() {
        assert!(parse_search("", &columns(), None).is_none());
        assert!(parse_search("   ", &columns(), None).is_none());
    }

    #[test]
    fn text_term_matches_text_columns_only() {
        let cond = parse_search("rust", &columns(), None);
        assert!(cond.is_some(), "Text columns should produce an OR clause");
        // No int/bool/float predicate should land — `rust` doesn't
        // parse as i64 / f64 / bool. The OR is non-empty thanks to
        // title + body Text columns.
    }

    #[test]
    fn numeric_term_matches_int_and_float_and_fk() {
        let cond = parse_search("42", &columns(), None);
        assert!(
            cond.is_some(),
            "term that parses as both int + float should produce a non-empty OR"
        );
    }

    #[test]
    fn boolean_term_matches_boolean_column() {
        let cond = parse_search("true", &columns(), None);
        assert!(
            cond.is_some(),
            "boolean column should join the OR for `true`"
        );
    }

    #[test]
    fn term_with_no_matching_column_returns_none() {
        // A model with only a Date column and a non-text non-numeric term.
        let cols = vec![col("when", SqlType::Date)];
        assert!(
            parse_search("hello", &cols, None).is_none(),
            "Date column doesn't match free-text terms"
        );
    }

    #[test]
    fn restrict_to_honoured_when_subset_present() {
        let allow = vec!["title".to_string()];
        let cond = parse_search("rust", &columns(), Some(&allow));
        assert!(cond.is_some(), "title is in the allow-list and is Text");
    }

    #[test]
    fn restrict_to_empty_subset_returns_none() {
        let allow: Vec<String> = Vec::new();
        assert!(
            parse_search("rust", &columns(), Some(&allow)).is_none(),
            "empty allow-list excludes every column"
        );
    }

    #[test]
    fn restrict_to_unknown_column_returns_none() {
        let allow = vec!["does_not_exist".to_string()];
        assert!(parse_search("rust", &columns(), Some(&allow)).is_none());
    }
}

#[cfg(test)]
mod choice_validation {
    use super::*;

    fn status_col() -> Column {
        Column {
            name: "status".into(),
            ty: SqlType::Text,
            primary_key: false,
            nullable: false,
            fk_target: None,
            noform: false,
            db_constraint: true,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: vec!["draft".into(), "published".into(), "archived".into()],
            choice_labels: Vec::new(),
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: ::umbra::orm::FkAction::NoAction,
            on_update: ::umbra::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        }
    }

    fn assert_bad_input(res: Result<(), ApiError>, msg_contains: &[&str]) {
        match res {
            Err(ApiError::BadInput(m)) => {
                for needle in msg_contains {
                    assert!(
                        m.contains(needle),
                        "expected error message to contain `{needle}`, got: {m}"
                    );
                }
            }
            other => panic!("expected BadInput, got: {other:?}"),
        }
    }

    #[test]
    fn eq_with_valid_choice_passes() {
        assert!(validate_choices(&status_col(), "eq", "published").is_ok());
    }

    #[test]
    fn eq_with_unknown_choice_rejects_with_allowed_list() {
        let res = validate_choices(&status_col(), "eq", "pub");
        assert_bad_input(res, &["pub", "status", "draft", "published", "archived"]);
    }

    #[test]
    fn in_with_one_invalid_csv_element_rejects_naming_only_that_one() {
        let res = validate_choices(&status_col(), "in", "draft,pub,archived");
        match res {
            Err(ApiError::BadInput(m)) => {
                // Mentions the invalid token and the column.
                assert!(m.contains("pub"), "missing invalid token in: {m}");
                assert!(m.contains("status"), "missing column name in: {m}");
                // Doesn't quote the valid CSV items as "supplied".
                let supplied_section = m.split("not in the allowed").next().unwrap_or("");
                assert!(
                    !supplied_section.contains("draft"),
                    "draft (valid) should not be listed as supplied: {m}"
                );
                assert!(
                    !supplied_section.contains("archived"),
                    "archived (valid) should not be listed as supplied: {m}"
                );
            }
            other => panic!("expected BadInput, got: {other:?}"),
        }
    }

    #[test]
    fn substring_lookups_skip_choices_check() {
        // contains/icontains/startswith are fuzzy searches; the value
        // is a substring, not an assertion of equality.
        for lookup in ["contains", "icontains", "startswith"] {
            assert!(
                validate_choices(&status_col(), lookup, "pub").is_ok(),
                "{lookup} should bypass choices validation"
            );
        }
    }

    #[test]
    fn isnull_skips_choices_check() {
        // isnull's value is a boolean — never a choice.
        assert!(validate_choices(&status_col(), "isnull", "true").is_ok());
    }

    #[test]
    fn column_with_no_choices_is_always_ok() {
        let mut col = status_col();
        col.choices.clear();
        assert!(validate_choices(&col, "eq", "anything").is_ok());
    }

    #[test]
    fn multichoice_column_skips_check() {
        let mut col = status_col();
        col.is_multichoice = true;
        // Multichoice values are CSV subsets — `__eq` against the
        // raw CSV doesn't map cleanly to a per-value check, so we
        // skip the validator and trust the user knows the CSV shape.
        assert!(validate_choices(&col, "eq", "draft,published").is_ok());
    }
}
