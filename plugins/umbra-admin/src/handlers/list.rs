//! Index dashboard, changelist, paginated rows fragment, filter dialog
//! fragment, and the `fetch_distinct_values` helper they share for the
//! facet-builder.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use chrono::Timelike;
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
use crate::view::{model_for_template, model_for_template_cols, sidebar_apps, sql_type_name};

/// Resolve a foreign-key id to the related model's display label
/// (first non-PK text column, same shape the FK picker uses). Returns
/// `None` for non-FK columns, unresolvable ids, or any DB error —
/// callers fall back to the raw value.
async fn resolve_fk_label(
    parent: &umbra::migrate::ModelMeta,
    field: &str,
    raw_id: &str,
) -> Option<String> {
    // Resolve `field` against both FK columns and M2M relations — the
    // chip row treats them uniformly (one chip per selected id) so the
    // resolution path also folds together.
    let related_table = if let Some(col) = parent.fields.iter().find(|c| c.name == field) {
        if !matches!(col.ty, SqlType::ForeignKey) {
            return None;
        }
        col.fk_target
            .clone()
            .unwrap_or_else(|| field.trim_end_matches("_id").to_string())
    } else if let Some(rel) = parent.m2m_relations.iter().find(|r| r.field_name == field) {
        rel.target_table.clone()
    } else {
        return None;
    };
    let (_, related) = find_model(&related_table)?;
    let pk = related.fields.iter().find(|c| c.primary_key)?;
    let label_col = related
        .fields
        .iter()
        .find(|c| !c.primary_key && matches!(c.ty, SqlType::Text))
        .map(|c| c.name.clone())?;
    let rows = DynQuerySet::for_meta(&related)
        .select_cols(&[label_col.clone()])
        .filter_eq_string(&pk.name, raw_id)
        .limit(1)
        .fetch_as_strings()
        .await
        .ok()?;
    rows.into_iter()
        .next()
        .and_then(|r| r.get(&label_col).cloned())
}

/// Build the template-facing `active_filters` JSON list, resolving FK
/// fields to their related-model labels so chips render
/// `category: Coffee` rather than `category: 1`. Multi-value selections
/// (e.g. `?filter_brand=1,2`) fan out into one chip per id so each can
/// be removed independently. Falls back to the raw value when no label
/// resolves.
/// Build the `&filter_<field>=<comma-joined>` query-string fragment
/// that pagination buttons, the Filter dialog URL, and the chip
/// remove links all reuse. Built once per request from the raw
/// `Vec<(field, value)>` so iterating the fanned-out chip list
/// downstream doesn't collapse repeated keys.
fn build_filter_qs(active_filters: &[(String, String)]) -> String {
    let mut out = String::new();
    for (field, value) in active_filters {
        if value.is_empty() {
            continue;
        }
        out.push_str("&filter_");
        out.push_str(&urlencode(field));
        out.push('=');
        out.push_str(&urlencode(value));
    }
    out
}

/// One JSON entry per unique active-filter field for the hidden
/// `#dt-active-filters` inputs. `value` is the comma-joined group
/// string — one input per field — so HTMX `hx-include` lands a
/// single `filter_<field>=<comma>` URL param on each downstream
/// request rather than the fanned-out chip list (which the HashMap
/// query extractor would collapse to one value).
fn build_filter_groups(active_filters: &[(String, String)]) -> Vec<serde_json::Value> {
    active_filters
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(f, v)| serde_json::json!({ "field": f, "value": v }))
        .collect()
}

async fn build_active_filter_list(
    model: &umbra::migrate::ModelMeta,
    active_filters: &[(String, String)],
) -> Vec<serde_json::Value> {
    // Pre-split every filter so we can fan multi-value selections
    // into per-id chips AND compute each chip's "remove me" URL
    // against the same fanned-out set.
    let groups: Vec<(String, Vec<String>)> = active_filters
        .iter()
        .map(|(f, v)| {
            let parts: Vec<String> = if v.contains(',') {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            } else {
                vec![v.clone()]
            };
            (f.clone(), parts)
        })
        .collect();

    let mut out = Vec::new();
    for (field_idx, (field, values)) in groups.iter().enumerate() {
        for (val_idx, raw) in values.iter().enumerate() {
            let display = resolve_fk_label(model, field, raw)
                .await
                .unwrap_or_else(|| raw.clone());
            // Rebuild every filter except this one (field_idx, val_idx),
            // joining surviving values per-field on `,`. Empty surviving
            // sets drop the field entirely. The leading `&` lets the
            // template concatenate this against `?search=...&sort=...`.
            let mut qs = String::new();
            for (i, (other_f, other_vs)) in groups.iter().enumerate() {
                let kept: Vec<&str> = other_vs
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| !(i == field_idx && *j == val_idx))
                    .map(|(_, s)| s.as_str())
                    .collect();
                if kept.is_empty() {
                    continue;
                }
                qs.push_str("&filter_");
                qs.push_str(&urlencode(other_f));
                qs.push('=');
                qs.push_str(&urlencode(&kept.join(",")));
            }
            out.push(serde_json::json!({
                "field": field,
                "value": raw,
                "display": display,
                "remove_qs": qs,
            }));
        }
    }
    out
}

fn urlencode(s: &str) -> String {
    crate::util::urlencoding_simple(s)
}

/// Build one [`FilterFacet`] for the dialog. Centralises the
/// "column type lookup + distinct-value fetch + FK target lookup"
/// trio so both the changelist handler and the dialog handler share
/// one path. FK columns skip the distinct-value fetch entirely —
/// the dialog hits the FK picker endpoint for label-rich options.
async fn build_facet(model: &umbra::migrate::ModelMeta, field: &str) -> FilterFacet {
    // M2M fields aren't columns — they live in `model.m2m_relations`.
    // Surface them as `col_type = "m2m"` so the dialog can reuse the
    // FK searchable-picker branch (same UX, different SQL underneath).
    if let Some(rel) = model.m2m_relations.iter().find(|r| r.field_name == field) {
        return FilterFacet {
            field: field.to_string(),
            col_type: "m2m".to_string(),
            values: Vec::new(),
            related_table: rel.target_table.clone(),
        };
    }
    let col = model.fields.iter().find(|c| c.name == field);
    let col_type = col
        .map(|c| sql_type_name(c.ty).to_string())
        .unwrap_or_default();
    let related_table = col
        .filter(|c| matches!(c.ty, SqlType::ForeignKey))
        .and_then(|c| c.fk_target.clone())
        .unwrap_or_default();
    // FK fields use the picker endpoint so the dialog can render
    // labels, not raw ids. Fetching distinct ids here would be IO
    // we then throw away.
    let values = if col_type == "fk" {
        Vec::new()
    } else {
        fetch_distinct_values(&model.table, field)
            .await
            .unwrap_or_default()
    };
    FilterFacet {
        field: field.to_string(),
        col_type,
        values,
        related_table,
    }
}

/// Template-facing facet — one filter-dialog choice list.
///
/// `col_type` is computed on the server so the dialog template can
/// `{% if facet.col_type == "fk" %}` directly. Jinja's `{% set %}`
/// inside a `{% for %}` only writes to the loop scope, so the
/// previous "compute it by iterating `columns`" approach silently
/// reset to `""` after the loop and the FK branch was unreachable.
#[derive(Debug, Clone, Serialize)]
struct FilterFacet {
    field: String,
    col_type: String,
    /// Raw distinct values from the DB. For FK fields this list is
    /// intentionally empty — the dialog loads label-rich options
    /// from the FK picker endpoint instead of showing raw ids.
    values: Vec<String>,
    /// FK only: the related table name so the dialog JS can hit
    /// the right `/admin/api/{table}/{field}/options` endpoint.
    /// Empty for non-FK fields.
    related_table: String,
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

    // Per-model row count for the dashboard cards. Fires every
    // COUNT concurrently so a project with 30 registered models
    // doesn't pay 30 sequential round-trips on every /admin/ load
    // (the old serial loop made the dashboard's first-paint scale
    // linearly with model count — a real N+1 from the admin user's
    // POV, even though each individual query is one COUNT). The
    // count path still routes through DynQuerySet::count so each
    // query goes through the same builder the changelist uses.
    //
    // Filtering by `state.dashboard_models`:
    //   - All     → every (app, model) pair (default)
    //   - Hidden  → empty Vec, dashboard.html skips the section
    //   - Only(t) → keep pairs whose table is in t, in t's order
    //     (so the operator controls card order via the allowlist)
    let model_cards: Vec<serde_json::Value> = {
        let all_pairs: Vec<&crate::view::SidebarModel> =
            apps.iter().flat_map(|a| a.models.iter()).collect();
        let pairs: Vec<&crate::view::SidebarModel> = match &state.dashboard_models {
            crate::DashboardModelsConfig::All => all_pairs,
            crate::DashboardModelsConfig::Hidden => Vec::new(),
            crate::DashboardModelsConfig::Only(allowlist) => allowlist
                .iter()
                .filter_map(|table| all_pairs.iter().find(|p| p.table == *table).copied())
                .collect(),
        };
        let count_futures = pairs.iter().map(|m| async move {
            match find_model(&m.table) {
                Some((_, meta)) => DynQuerySet::for_meta(&meta).count().await.unwrap_or(0),
                None => 0,
            }
        });
        let counts: Vec<i64> = futures_util::future::join_all(count_futures).await;
        pairs
            .into_iter()
            .zip(counts)
            .map(|(sidebar_model, count)| {
                serde_json::json!({
                    "table":  sidebar_model.table,
                    "label":  sidebar_model.label,
                    "icon":   if sidebar_model.icon.is_empty() { "database".to_string() } else { sidebar_model.icon.clone() },
                    "count":  count,
                    "url":    format!("{}/{}/", crate::branding::current().base_path, sidebar_model.table),
                })
            })
            .collect()
    };

    let total_rows: i64 = model_cards.iter().filter_map(|c| c["count"].as_i64()).sum();
    let model_count = model_cards.len();
    let plugin_count = apps.len();

    // Hour/minute for the time-of-day greeting.
    use chrono::Utc;
    let now = Utc::now();
    let now_hour: u32 = now.hour();
    let now_minute: u32 = now.minute();

    let initial_theme = user_theme(&user).await;

    match render(
        "admin/dashboard.html",
        context!(
            user          => user.username.clone(),
            widgets       => widgets,
            model_cards   => model_cards,
            apps          => apps,
            total_rows    => total_rows,
            model_count   => model_count,
            plugin_count  => plugin_count,
            now_hour      => now_hour,
            now_minute    => now_minute,
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
    let path = format!("{}/{table}/", crate::branding::current().base_path);
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };

    // Feature #75: require view permission before rendering the
    // changelist. Short-circuits to `Ok(())` when PermissionsPlugin
    // is not installed, preserving pre-#75 staff-only behaviour.
    if let Err(r) =
        crate::permcheck::require(&user, &plugin_name, &table, crate::permcheck::Action::View).await
    {
        return r;
    }
    let perms = crate::permcheck::AdminPerms::load(&user, &plugin_name, &table).await;

    let cfg = state.config_for(&table);

    let display_cols: Vec<String> = if let Some(c) = cfg
        && !c.list_display.is_empty()
    {
        c.list_display.clone()
    } else {
        default_list_display(&model)
    };

    let (search_term, active_filters, sort_col, sort_order, page, page_size) =
        parse_list_params(&params, cfg, pk);

    let fetch_cols: Vec<String> = {
        let mut cols = display_cols.clone();
        if !cols.contains(&pk.name) {
            cols.push(pk.name.clone());
        }
        cols
    };

    let order_clause = build_order_clause_phase2(cfg, pk, &sort_col, &sort_order);

    let total =
        match count_rows_filtered(&model, search_term.as_deref(), cfg, &active_filters).await {
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
        &active_filters,
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
            facets.push(build_facet(&model, field).await);
        }
    }

    let action_names: Vec<serde_json::Value> = cfg
        .map(handlers::action_descriptors_json)
        .unwrap_or_default();

    let has_search = cfg.is_some_and(|c| !c.search_fields.is_empty());
    let search_val = search_term.unwrap_or_default();
    let active_filter_list = build_active_filter_list(&model, &active_filters).await;
    let filter_qs = build_filter_qs(&active_filters);
    let filter_groups = build_filter_groups(&active_filters);
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("{}/{table}/", crate::branding::current().base_path) }),
    ];
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
            active_filters     => active_filter_list,
            filter_qs          => filter_qs,
            filter_groups      => filter_groups,
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
            perms              => perms,
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
    let path = format!("{}/{table}/rows", crate::branding::current().base_path);
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    if !is_htmx(&headers) {
        let qs = serde_urlencoded::to_string(&params).unwrap_or_default();
        let target = if qs.is_empty() {
            format!("{}/{table}/", crate::branding::current().base_path)
        } else {
            format!("{}/{table}/?{qs}", crate::branding::current().base_path)
        };
        return Redirect::to(&target).into_response();
    }
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&user, &plugin_name, &table, crate::permcheck::Action::View).await
    {
        return r;
    }
    let perms = crate::permcheck::AdminPerms::load(&user, &plugin_name, &table).await;
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };

    let cfg = state.config_for(&table);
    let (search_term, active_filters, sort_col, sort_order, page, page_size) =
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

    let total =
        match count_rows_filtered(&model, search_term.as_deref(), cfg, &active_filters).await {
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
        &active_filters,
        pagination.page_size,
        pagination.offset(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let columns = model_for_template_cols(&model, &display_cols).fields;
    let active_filter_list = build_active_filter_list(&model, &active_filters).await;
    let filter_qs = build_filter_qs(&active_filters);
    let filter_groups = build_filter_groups(&active_filters);
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
            active_filters     => active_filter_list,
            filter_qs          => filter_qs,
            filter_groups      => filter_groups,
            search_val         => search_val,
            sort_col           => sort_col,
            sort_order         => sort_order,
            actions            => action_names,
            inline_edit_fields => inline_edit_fields,
            perms              => perms,
        ),
    ) {
        Ok(html) => {
            let mut response = html.into_response();
            // Push the changelist URL (not this /rows partial URL) so the
            // browser bar reflects the page a user would refresh into.
            // Overrides any client-side `hx-push-url="true"` on the
            // pagination buttons, chip remove links, and the page-size
            // select — one fix covers every request that lands here.
            let query = serde_urlencoded::to_string(&params).unwrap_or_default();
            let push_url = format!("{}/{table}/?{query}", crate::branding::current().base_path);
            if let Ok(v) = axum::http::HeaderValue::from_str(&push_url) {
                response.headers_mut().insert("HX-Push-Url", v);
            }
            response
        }
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
    let path = format!(
        "{}/{table}/filter-dialog",
        crate::branding::current().base_path
    );
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
            facets.push(build_facet(&model, field).await);
        }
    }

    let search_val = params.get("search").cloned().unwrap_or_default();
    let sort_col = params.get("sort").cloned().unwrap_or_default();
    let sort_order = params.get("order").cloned().unwrap_or_default();
    let columns = model_for_template(&model).fields;
    // Build a `{field: value}` JSON map of currently-active filters so
    // the dialog can pre-select each facet's committed value when
    // re-opened. Comes from the same `filter_<field>=value` query
    // params the list handler parses — keeps a single source of truth.
    let mut active_map = serde_json::Map::new();
    for (k, v) in &params {
        if let Some(field) = k.strip_prefix("filter_") {
            if !v.is_empty() {
                active_map.insert(field.to_string(), serde_json::Value::String(v.clone()));
            }
        }
    }

    match render(
        "admin/filter_dialog_fragment.html",
        context!(
            model          => model_for_template(&model),
            facets         => facets,
            columns        => columns,
            search_val     => search_val,
            sort_col       => sort_col,
            sort_order     => sort_order,
            active_filters => serde_json::Value::Object(active_map),
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}
