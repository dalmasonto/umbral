//! Index dashboard, changelist, paginated rows fragment, filter dialog
//! fragment, and the `fetch_distinct_values` helper they share for the
//! facet-builder.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use minijinja::context;
use serde::Serialize;
use umbra::orm::{DynQuerySet, SqlType};
use umbra::web::{HeaderMap, IntoResponse, Redirect, Response};

use crate::AdminState;
use crate::auth::require_staff;
use crate::discovery::{default_list_display, find_model, pk_column, user_theme};
use crate::engine::render;
use crate::error::AdminError;
use crate::handlers;
use crate::pagination::{Pagination, build_order_clause_phase2, parse_list_params};
use crate::rows::{count_rows_filtered, fetch_rows_paged};
use crate::util::is_htmx;
use crate::view::{model_for_template, model_for_template_cols, sidebar_apps};

/// Template-facing facet — one filter-dialog choice list.
#[derive(Debug, Clone, Serialize)]
struct FilterFacet {
    field: String,
    values: Vec<String>,
}

/// `GET /admin` — the dashboard. One card per registered model with a
/// row count, plus the user's widget grid.
pub(crate) async fn index(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let user = match require_staff(&headers, "/admin/").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let apps = sidebar_apps(&state, &user);

    let catalog = state.widget_catalog.as_ref();
    let widgets: Vec<serde_json::Value> = catalog
        .iter()
        .map(|w| {
            serde_json::json!({
                "key":  w.key,
                "title": w.title,
                "kind": w.kind.as_str(),
                "span": {
                    "cols": w.default_span.cols,
                    "rows": w.default_span.rows,
                },
            })
        })
        .collect();

    // Per-model row count for the dashboard cards. Goes through
    // DynQuerySet::count so the query path is identical to the
    // changelist — no hand-built `SELECT COUNT(*) FROM "<table>"` here.
    let model_cards: Vec<serde_json::Value> = {
        let mut cards = Vec::new();
        for app in &apps {
            for sidebar_model in &app.models {
                let count = match find_model(&sidebar_model.table) {
                    Some((_, meta)) => DynQuerySet::for_meta(&meta).count().await.unwrap_or(0),
                    None => 0,
                };
                cards.push(serde_json::json!({
                    "table":  sidebar_model.table,
                    "label":  sidebar_model.label,
                    "icon":   if sidebar_model.icon.is_empty() { "database".to_string() } else { sidebar_model.icon.clone() },
                    "count":  count,
                    "url":    format!("/admin/{}/", sidebar_model.table),
                }));
            }
        }
        cards
    };

    let initial_theme = user_theme(&user).await;

    match render(
        "admin/dashboard.html",
        context!(
            user          => user.username.clone(),
            widgets       => widgets,
            model_cards   => model_cards,
            apps          => apps,
            active_table  => "",
            breadcrumbs   => Vec::<serde_json::Value>::new(),
            initial_theme => initial_theme,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/{table}/` — the changelist page.
pub(crate) async fn list(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/{table}/");
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };

    let cfg = state.config_for(&table);

    let display_cols: Vec<String> = if let Some(c) = cfg
        && !c.list_display.is_empty()
    {
        c.list_display.clone()
    } else {
        default_list_display(&model)
    };

    let (search_term, active_filter, sort_col, sort_order, page, page_size) =
        parse_list_params(&params, cfg, pk);

    let fetch_cols: Vec<String> = {
        let mut cols = display_cols.clone();
        if !cols.contains(&pk.name) {
            cols.push(pk.name.clone());
        }
        cols
    };

    let order_clause = build_order_clause_phase2(cfg, pk, &sort_col, &sort_order);

    let total = match count_rows_filtered(
        &model,
        search_term.as_deref(),
        cfg,
        active_filter
            .as_ref()
            .map(|(f, v)| (f.as_str(), v.as_str())),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let pagination = Pagination::new(total, page, page_size);

    let rows = match fetch_rows_paged(
        &model,
        &fetch_cols,
        &order_clause,
        search_term.as_deref(),
        cfg,
        active_filter
            .as_ref()
            .map(|(f, v)| (f.as_str(), v.as_str())),
        pagination.page_size,
        pagination.offset(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let mut facets: Vec<FilterFacet> = Vec::new();
    if let Some(c) = cfg {
        for field in &c.list_filter {
            let values = fetch_distinct_values(&model.table, field)
                .await
                .unwrap_or_default();
            facets.push(FilterFacet {
                field: field.clone(),
                values,
            });
        }
    }

    let action_names: Vec<serde_json::Value> = cfg
        .map(handlers::action_descriptors_json)
        .unwrap_or_default();

    let has_search = cfg.is_some_and(|c| !c.search_fields.is_empty());
    let search_val = search_term.unwrap_or_default();
    let active_filter_str = active_filter
        .as_ref()
        .map(|(f, v)| format!("{f}={v}"))
        .unwrap_or_default();
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs =
        vec![serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") })];
    let flash = params.get("flash").cloned().unwrap_or_default();
    let open_row = params.get("row").cloned().unwrap_or_default();

    let columns = model_for_template_cols(&model, &display_cols).fields;

    let column_widths_json: serde_json::Value = cfg
        .map(|c| {
            let mut map = serde_json::Map::new();
            for (col, w) in &c.column_widths {
                map.insert(col.clone(), serde_json::Value::String(w.clone()));
            }
            serde_json::Value::Object(map)
        })
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let inline_edit_fields: Vec<String> = cfg
        .map(|c| c.inline_edit_fields.clone())
        .unwrap_or_default();

    let initial_theme = user_theme(&user).await;

    match render(
        "admin/changelist.html",
        context!(
            user               => user.username.clone(),
            model              => model_for_template_cols(&model, &display_cols),
            rows               => rows,
            columns            => columns,
            pk                 => pk.name.clone(),
            facets             => facets,
            actions            => action_names,
            has_search         => has_search,
            search_val         => search_val,
            active_filter      => active_filter_str,
            pagination         => pagination,
            sort_col           => sort_col,
            sort_order         => sort_order,
            flash              => flash,
            open_row           => open_row,
            apps               => apps,
            active_table       => table,
            breadcrumbs        => breadcrumbs,
            column_widths      => column_widths_json,
            inline_edit_fields => inline_edit_fields,
            initial_theme      => initial_theme,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// SELECT DISTINCT for a column — feeds the filter dialog facet lists.
/// Capped at 100 distinct values so a high-cardinality column doesn't
/// inflate the dialog. Now goes through `DynQuerySet::fetch_distinct_strings`.
async fn fetch_distinct_values(table: &str, field: &str) -> Result<Vec<String>, AdminError> {
    let Some((_, meta)) = find_model(table) else {
        return Ok(Vec::new());
    };
    Ok(DynQuerySet::for_meta(&meta)
        .limit(100)
        .fetch_distinct_strings(field)
        .await?
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect())
}

/// `GET /admin/{table}/rows` — paginated tbody fragment plus footer.
///
/// Direct browser navigation here (no `HX-Request` header) would render
/// the naked tbody without any chrome; redirect to the changelist page
/// with the same query string preserved so the page itself can HTMX-
/// load the rows.
pub(crate) async fn rows_fragment(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/{table}/rows");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    if !is_htmx(&headers) {
        let qs = serde_urlencoded::to_string(&params).unwrap_or_default();
        let target = if qs.is_empty() {
            format!("/admin/{table}/")
        } else {
            format!("/admin/{table}/?{qs}")
        };
        return Redirect::to(&target).into_response();
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };

    let cfg = state.config_for(&table);
    let (search_term, active_filter, sort_col, sort_order, page, page_size) =
        parse_list_params(&params, cfg, pk);

    let display_cols: Vec<String> = if let Some(c) = cfg
        && !c.list_display.is_empty()
    {
        c.list_display.clone()
    } else {
        default_list_display(&model)
    };

    let fetch_cols: Vec<String> = {
        let mut cols = display_cols.clone();
        if !cols.contains(&pk.name) {
            cols.push(pk.name.clone());
        }
        cols
    };

    let order_clause = build_order_clause_phase2(cfg, pk, &sort_col, &sort_order);

    let total = match count_rows_filtered(
        &model,
        search_term.as_deref(),
        cfg,
        active_filter
            .as_ref()
            .map(|(f, v)| (f.as_str(), v.as_str())),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let pagination = Pagination::new(total, page, page_size);

    let rows = match fetch_rows_paged(
        &model,
        &fetch_cols,
        &order_clause,
        search_term.as_deref(),
        cfg,
        active_filter
            .as_ref()
            .map(|(f, v)| (f.as_str(), v.as_str())),
        pagination.page_size,
        pagination.offset(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let columns = model_for_template_cols(&model, &display_cols).fields;
    let active_filter_str = active_filter
        .as_ref()
        .map(|(f, v)| format!("{f}={v}"))
        .unwrap_or_default();
    let search_val = search_term.unwrap_or_default();

    let action_names: Vec<serde_json::Value> = cfg
        .map(handlers::action_descriptors_json)
        .unwrap_or_default();

    let inline_edit_fields: Vec<String> = cfg
        .map(|c| c.inline_edit_fields.clone())
        .unwrap_or_default();

    match render(
        "admin/rows_fragment.html",
        context!(
            table              => table,
            model_name         => model.name.clone(),
            rows               => rows,
            pk                 => pk.name.clone(),
            columns            => columns,
            pagination         => pagination,
            active_filter      => active_filter_str,
            search_val         => search_val,
            sort_col           => sort_col,
            sort_order         => sort_order,
            actions            => action_names,
            inline_edit_fields => inline_edit_fields,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/{table}/filter-dialog` — filter dialog fragment.
///
/// Only filterable field types are shown; text and numeric fields
/// listed in `list_filter` are silently dropped (with a debug log)
/// because filtering them via discrete-value facets doesn't make
/// sense.
pub(crate) async fn filter_dialog_handler(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/{table}/filter-dialog");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let cfg = state.config_for(&table);

    let mut facets: Vec<FilterFacet> = Vec::new();
    if let Some(c) = cfg {
        for field in &c.list_filter {
            let col_ty = model
                .fields
                .iter()
                .find(|col| &col.name == field)
                .map(|col| col.ty);
            if let Some(ty) = col_ty {
                match ty {
                    SqlType::Text => {
                        tracing::debug!(
                            field = field.as_str(),
                            table = table.as_str(),
                            "text fields are not filterable; use search_fields"
                        );
                        continue;
                    }
                    SqlType::SmallInt
                    | SqlType::Integer
                    | SqlType::BigInt
                    | SqlType::Real
                    | SqlType::Double => {
                        tracing::debug!(
                            field = field.as_str(),
                            table = table.as_str(),
                            "numeric fields are not filterable via the filter dialog; use search_fields"
                        );
                        continue;
                    }
                    _ => {}
                }
            }
            let values = fetch_distinct_values(&model.table, field)
                .await
                .unwrap_or_default();
            facets.push(FilterFacet {
                field: field.clone(),
                values,
            });
        }
    }

    let search_val = params.get("search").cloned().unwrap_or_default();
    let sort_col = params.get("sort").cloned().unwrap_or_default();
    let sort_order = params.get("order").cloned().unwrap_or_default();
    let active_filter = params.get("active_filter").cloned().unwrap_or_default();
    let columns = model_for_template(&model).fields;

    match render(
        "admin/filter_dialog_fragment.html",
        context!(
            model         => model_for_template(&model),
            facets        => facets,
            columns       => columns,
            search_val    => search_val,
            sort_col      => sort_col,
            sort_order    => sort_order,
            active_filter => active_filter,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}
