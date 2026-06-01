//! List-view pagination + URL-param parsing + ORDER BY builder.
//!
//! `ListParams` is the parsed shape every list / fragment / changelist
//! handler consumes. `Pagination` is the template-facing footer
//! context. The order-by helpers split into two: phase 1 uses the
//! config's static `ordering`; phase 2 layers an interactive
//! click-to-sort on top.

use std::collections::HashMap;

use serde::Serialize;
use umbra::migrate::Column;

use crate::config::AdminConfig;
use crate::q;

/// Parsed query parameters for list views.
///
/// `(search, active_filter, sort_col, sort_order, page, page_size)`.
/// Kept as a tuple alias so call sites can destructure with a single
/// `let` (the alternative — a named struct — adds a per-field syntax
/// without buying type-safety the call sites need).
pub(crate) type ListParams = (
    Option<String>,
    Option<(String, String)>,
    String,
    String,
    usize,
    usize,
);

/// Parse the common query params for any list-like view.
///
/// Accepts both phase 1 (`q=`, `filter_<field>=`) and phase 2
/// (`search=`, `filter=field=value`) shapes so an old bookmark / link
/// keeps working after the upgrade. `page_size` is clamped to `[1, 200]`.
pub(crate) fn parse_list_params(
    params: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
    pk: &Column,
) -> ListParams {
    // Accept both `search=` (phase 2) and `q=` (phase 1 backward compat).
    let search_term = params
        .get("search")
        .filter(|s| !s.is_empty())
        .or_else(|| params.get("q").filter(|s| !s.is_empty()))
        .cloned();
    // Accept both new `filter=field=value` (phase 2) and old `filter_<field>=<value>` (phase 1).
    let active_filter: Option<(String, String)> = params
        .get("filter")
        .filter(|s| !s.is_empty())
        .and_then(|s| {
            let mut parts = s.splitn(2, '=');
            let field = parts.next()?.to_string();
            let value = parts.next()?.to_string();
            Some((field, value))
        })
        .or_else(|| {
            // Phase 1 style: filter_<field>=<value>
            params.iter().find_map(|(k, v)| {
                k.strip_prefix("filter_")
                    .map(|field| (field.to_string(), v.clone()))
            })
        });
    let sort_col = params.get("sort").cloned().unwrap_or_default();
    let sort_order = params
        .get("order")
        .map(|o| {
            if o == "desc" {
                "desc".to_string()
            } else {
                "asc".to_string()
            }
        })
        .unwrap_or_else(|| "asc".to_string());
    let page = params
        .get("page")
        .and_then(|p| p.parse::<usize>().ok())
        .unwrap_or(1);
    let default_page_size = cfg.map(|c| c.list_per_page).unwrap_or(25);
    let page_size = params
        .get("page_size")
        .and_then(|p| p.parse::<usize>().ok())
        .unwrap_or(default_page_size)
        .clamp(1, 200);

    let _ = pk; // pk is used for default ordering at the call-site, not here
    (
        search_term,
        active_filter,
        sort_col,
        sort_order,
        page,
        page_size,
    )
}

/// Phase 1 ORDER BY clause: honour the model's configured `ordering`,
/// fall back to `pk ASC`. A leading `-` flips a column to `DESC`.
pub(crate) fn build_order_clause(cfg: Option<&AdminConfig>, pk: &Column) -> String {
    let ordering = cfg.map(|c| c.ordering.as_slice()).unwrap_or(&[]);
    if ordering.is_empty() {
        return format!("\"{}\" ASC", q(&pk.name));
    }
    ordering
        .iter()
        .map(|s| {
            if let Some(col) = s.strip_prefix('-') {
                format!("\"{}\" DESC", q(col))
            } else {
                format!("\"{}\" ASC", q(s))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Phase 2 ORDER BY: interactive column-header click wins over the
/// configured ordering, falling back to phase 1 when no `?sort=` is set.
pub(crate) fn build_order_clause_phase2(
    cfg: Option<&AdminConfig>,
    pk: &Column,
    sort_col: &str,
    sort_order: &str,
) -> String {
    if !sort_col.is_empty() {
        let dir = if sort_order == "desc" { "DESC" } else { "ASC" };
        return format!("\"{}\" {}", q(sort_col), dir);
    }
    build_order_clause(cfg, pk)
}

/// Template-facing pagination footer context.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Pagination {
    pub page: usize,
    pub page_size: usize,
    pub total: usize,
    pub total_pages: usize,
}

impl Pagination {
    pub fn new(total: usize, page: usize, page_size: usize) -> Self {
        let page_size = page_size.max(1);
        let total_pages = total.div_ceil(page_size).max(1);
        let page = page.max(1).min(total_pages);
        Self {
            page,
            page_size,
            total,
            total_pages,
        }
    }

    pub fn offset(&self) -> usize {
        (self.page - 1) * self.page_size
    }
}
