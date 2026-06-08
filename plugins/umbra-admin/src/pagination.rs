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
/// `(search, active_filters, sort_col, sort_order, page, page_size)`.
/// `active_filters` is a `Vec<(field, value)>` because the dialog can
/// commit one selection per declared `list_filter` column; the rendered
/// SQL ANDs them. The vec stays sorted by field name so URLs and the
/// active-filter chip row render deterministically across requests.
pub(crate) type ListParams = (
    Option<String>,
    Vec<(String, String)>,
    String,
    String,
    usize,
    usize,
);

/// Parse the common query params for any list-like view.
///
/// URL shape is `?filter_<field>=<value>` per active filter — repeated for
/// every dropdown the user committed in the dialog. The phase-2
/// `?filter=field=value` single-filter shape is still accepted for
/// backward compat with old bookmarks; when both are present, the named
/// `filter_<field>=` entries win. `page_size` is clamped to `[1, 200]`.
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
    let active_filters = parse_active_filters(params);
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
        active_filters,
        sort_col,
        sort_order,
        page,
        page_size,
    )
}

/// Pull every `filter_<field>=<value>` (or the legacy `?filter=field=value`)
/// out of `params` and return them sorted by field. Empty values are
/// skipped. Split out from [`parse_list_params`] so the parser is
/// testable without having to construct a [`Column`].
fn parse_active_filters(params: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut active: Vec<(String, String)> = params
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("filter_")
                .filter(|_| !v.is_empty())
                .map(|field| (field.to_string(), v.clone()))
        })
        .collect();
    // Legacy single-filter fallback (`?filter=field=value`). Only kicks
    // in when no named filter was supplied so the canonical shape wins.
    if active.is_empty() {
        if let Some(s) = params.get("filter").filter(|s| !s.is_empty()) {
            let mut parts = s.splitn(2, '=');
            if let (Some(field), Some(value)) = (parts.next(), parts.next()) {
                active.push((field.to_string(), value.to_string()));
            }
        }
    }
    active.sort_by(|a, b| a.0.cmp(&b.0));
    active
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

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn collects_every_filter_underscore_param_sorted_by_field() {
        let filters = parse_active_filters(&p(&[
            ("filter_status", "published"),
            ("filter_author", "alice"),
            ("filter_tag", "rust"),
        ]));
        assert_eq!(
            filters,
            vec![
                ("author".into(), "alice".into()),
                ("status".into(), "published".into()),
                ("tag".into(), "rust".into()),
            ]
        );
    }

    #[test]
    fn skips_empty_filter_values() {
        let filters =
            parse_active_filters(&p(&[("filter_status", ""), ("filter_author", "alice")]));
        assert_eq!(filters, vec![("author".into(), "alice".into())]);
    }

    #[test]
    fn falls_back_to_legacy_single_filter_when_no_named_params() {
        let filters = parse_active_filters(&p(&[("filter", "status=archived")]));
        assert_eq!(filters, vec![("status".into(), "archived".into())]);
    }

    #[test]
    fn named_filters_win_over_legacy_single_form() {
        let filters = parse_active_filters(&p(&[
            ("filter", "status=archived"),
            ("filter_status", "published"),
        ]));
        // The named form is canonical; the legacy `?filter=` is only a
        // last-resort fallback when there is no `filter_<field>=` at all.
        assert_eq!(filters, vec![("status".into(), "published".into())]);
    }

    #[test]
    fn empty_params_yield_empty_filter_list() {
        assert!(parse_active_filters(&HashMap::new()).is_empty());
    }
}
