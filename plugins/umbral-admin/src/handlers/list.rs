//! Index dashboard, changelist, paginated rows fragment, filter dialog
//! fragment, and the `fetch_distinct_values` helper they share for the
//! facet-builder.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use chrono::Timelike;
use minijinja::context;
use serde::Serialize;
use umbral::orm::{DynQuerySet, SqlType};
use umbral::web::{HeaderMap, IntoResponse, Redirect, Response};

use crate::AdminState;
use crate::auth::require_staff;
use crate::discovery::{default_list_display, find_model, pk_column, user_theme};
use crate::engine::render;
use crate::error::AdminError;
use crate::handlers;
use crate::pagination::{Pagination, build_order_clause_phase2, parse_list_params};
use crate::rows::{count_rows_filtered, fetch_rows_paged};
use crate::util::is_htmx;
use crate::view::{
    model_for_template, model_for_template_cols, sidebar_apps, sql_type_name, view_groups,
};

/// Resolve a foreign-key id to the related model's display label
/// (first non-PK text column, same shape the FK picker uses). Returns
/// `None` for non-FK columns, unresolvable ids, or any DB error —
/// callers fall back to the raw value.
async fn resolve_fk_label(
    parent: &umbral::migrate::ModelMeta,
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
    model: &umbral::migrate::ModelMeta,
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
async fn build_facet(model: &umbral::migrate::ModelMeta, field: &str) -> FilterFacet {
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
pub(crate) async fn index(
    State(state): State<AdminState>,
    headers: HeaderMap,
    // gaps2 #33 — `?dashboard=1` forces the dashboard even when
    // `restore_last_path` is enabled. Always extracted so the route
    // accepts the query string in both flag states.
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let user = match require_staff(&headers, "/admin/").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    // gaps2 #33 — "restore where I left off" feature.
    //
    // When `restore_last_path` is enabled (the default), redirect the
    // user to the last changelist they visited unless they explicitly
    // asked for the dashboard via `?dashboard=1`. This is an opt-out:
    // on by default, disable with
    // `AdminPlugin::default().restore_last_path(false)`.
    //
    // When the flag is false, always render the dashboard — no read,
    // no redirect.
    let dashboard_forced = params.get("dashboard").map(|v| v == "1").unwrap_or(false);
    if state.restore_last_path && !dashboard_forced {
        if let Ok(Some(last_path)) = crate::models::get_last_path(user.id).await {
            if !last_path.is_empty() {
                return Redirect::to(&last_path).into_response();
            }
        }
    }
    let apps = sidebar_apps(&state, &user).await;
    let view_groups = view_groups(&state, &user).await;

    // Sectioned widget list — each entry carries its own title +
    // optional subtitle + widget array. The template renders one
    // <section> per entry, so a dashboard with 20 widgets across
    // 4 sections reads as themed clusters rather than one
    // mega-grid. Falls back to a single un-named section when the
    // developer only uses the legacy `register_widget(...)` API.
    let widget_sections: Vec<serde_json::Value> = state
        .dashboard_sections
        .iter()
        .map(|section| {
            let widgets_json: Vec<serde_json::Value> = section
                .widgets
                .iter()
                .map(|w| {
                    serde_json::json!({
                        "key":   w.key,
                        "title": w.title,
                        "kind":  w.kind.as_str(),
                        "span":  {
                            "cols": w.default_span.cols,
                            "rows": w.default_span.rows,
                        },
                    })
                })
                .collect();
            serde_json::json!({
                "title":    section.title,
                "subtitle": section.subtitle,
                "widgets":  widgets_json,
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
    // Two distinct concerns:
    //
    // 1. Top-strip GLOBAL stats — Total Entries / Models / Plugins.
    //    These describe the whole project regardless of which
    //    cards the operator chose to surface, so they fan COUNTs
    //    over EVERY registered (app, model) pair.
    //
    // 2. The Models card grid — narrowed by
    //    `state.dashboard_models`:
    //      - All     → every pair (default)
    //      - Hidden  → empty Vec, the section is skipped
    //      - Only(t) → keep pairs whose table is in t, in t's order
    //
    // Both share one COUNT fan-out keyed by table → no double round-
    // trips. The fan-out runs concurrently so a 200-model project
    // still pays one parallel batch on each dashboard load.
    let all_pairs: Vec<&crate::view::SidebarModel> =
        apps.iter().flat_map(|a| a.models.iter()).collect();
    let count_futures = all_pairs.iter().map(|m| async move {
        let count = match find_model(&m.table) {
            Some((_, meta)) => DynQuerySet::for_meta(&meta).count().await.unwrap_or(0),
            None => 0,
        };
        (m.table.clone(), count)
    });
    let counts: std::collections::HashMap<String, i64> =
        futures_util::future::join_all(count_futures)
            .await
            .into_iter()
            .collect();

    // Global stats — from the FULL set, not the dashboard-filtered
    // subset. "Total entries" answers "how many rows are in this
    // project" honestly even when the operator has curated the
    // model-cards grid down to 6 tiles.
    let total_rows: i64 = counts.values().sum();
    let model_count: usize = all_pairs.len();
    let plugin_count: usize = apps.len();

    // Filtered cards — reuses the precomputed counts.
    let model_cards: Vec<serde_json::Value> = {
        let filtered: Vec<&crate::view::SidebarModel> = match &state.dashboard_models {
            crate::DashboardModelsConfig::All => all_pairs.clone(),
            crate::DashboardModelsConfig::Hidden => Vec::new(),
            crate::DashboardModelsConfig::Only(allowlist) => allowlist
                .iter()
                .filter_map(|table| all_pairs.iter().find(|p| p.table == *table).copied())
                .collect(),
        };
        filtered
            .into_iter()
            .map(|sidebar_model| {
                let count = counts.get(&sidebar_model.table).copied().unwrap_or(0);
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

    // Hour/minute for the time-of-day greeting.
    use chrono::Utc;
    let now = Utc::now();
    let now_hour: u32 = now.hour();
    let now_minute: u32 = now.minute();

    let initial_theme = user_theme(&user).await;

    match render(
        "admin/dashboard.html",
        context!(
            user                      => user.username.clone(),
            widget_sections           => widget_sections,
            model_cards               => model_cards,
            dashboard_models_title    => state.dashboard_models_title.clone(),
            dashboard_models_subtitle => state.dashboard_models_subtitle.clone(),
            apps                      => apps,
            view_groups               => view_groups,
            total_rows                => total_rows,
            model_count               => model_count,
            plugin_count              => plugin_count,
            now_hour                  => now_hour,
            now_minute                => now_minute,
            active_table              => "",
            breadcrumbs               => Vec::<serde_json::Value>::new(),
            initial_theme             => initial_theme,
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

    // gaps2 #11: paramless visit + saved per-table prefs → 303 redirect
    // to the URL with persisted query string. Cross-tab / cross-device
    // continuity: the user filters Products on their laptop, opens the
    // same URL on their phone, lands on the same view.
    //
    // The redirect runs only when the request carries NO query params at
    // all — any explicit param (including `?reset=1` if the gap's
    // follow-up adds a "clear filters" affordance) bypasses the
    // restore and the saved state gets overwritten by the write
    // below.
    if params.is_empty() {
        if let Ok(Some(pref)) = crate::models::get_table_pref(user.id, &table).await {
            if let Some(qs) = serialize_table_pref(&pref) {
                let path = crate::branding::current().base_path.clone();
                return Redirect::to(&format!("{path}/{table}/?{qs}")).into_response();
            }
        }
    }

    let cfg = state.config_for(&table);

    let mut display_cols: Vec<String> = if let Some(c) = cfg
        && !c.list_display.is_empty()
    {
        c.list_display.clone()
    } else {
        default_list_display(&model)
    };
    // gaps2 #11 round 2 — drop hidden columns from the render set.
    // Reads from the same `preferences.tables.<table>.hidden_cols`
    // the toggle endpoint writes to. The PK column is preserved
    // even if listed in hidden_cols (the row machinery needs it to
    // render edit/delete affordances); display logic in fetch_cols
    // below ensures the PK is included regardless.
    if let Ok(Some(saved)) = crate::models::get_table_pref(user.id, &table).await {
        if !saved.hidden_cols.is_empty() {
            display_cols.retain(|c| c == &pk.name || !saved.hidden_cols.contains(c));
        }
    }

    let (search_term, active_filters, sort_col, sort_order, page, page_size) =
        parse_list_params(&params, cfg, pk);

    // gaps2 #35: trash view. `?trash=1` flips the changelist to show
    // ONLY soft-deleted rows (`only_deleted()`); without it the list
    // shows the live set (the default, which already excludes
    // `deleted_at IS NOT NULL`). The toggle only renders for
    // soft-delete models, so a non-soft-delete model ignores `?trash`.
    let soft_delete = model.soft_delete;
    let trash = soft_delete && params.get("trash").map(|v| v == "1").unwrap_or(false);

    // gaps2 #11: persist the current shape so the next paramless
    // visit restores it. Fire-and-forget — a write error logs but
    // doesn't fail the page render. `page` is deliberately NOT
    // persisted: pagination state shouldn't survive across sessions
    // (the user wouldn't expect to land on page 5 after a logout),
    // but per_page IS persisted because it's a layout choice.
    // Preserve `hidden_cols` from any existing pref — the render
    // pipeline can mutate filters/search/sort/per_page each visit
    // but column visibility only changes via the toggle endpoint.
    let existing_hidden = crate::models::get_table_pref(user.id, &table)
        .await
        .ok()
        .flatten()
        .map(|p| p.hidden_cols)
        .unwrap_or_default();
    let pref = crate::models::TablePref {
        filters: active_filters.iter().cloned().collect(),
        search: search_term.clone().unwrap_or_default(),
        sort: shape_sort_directive(&sort_col, &sort_order),
        per_page: Some(page_size as u32),
        hidden_cols: existing_hidden,
    };
    if let Err(e) = crate::models::set_table_pref(user.id, &table, &pref).await {
        tracing::warn!(
            user = user.id,
            table = %table,
            error = %e,
            "gaps2 #11: failed to persist table prefs (continuing render)"
        );
    }
    // gaps2 #11 round 2 / gaps2 #33 — persist this URL as `last_path`
    // so a visit to `/admin/` can redirect the user back to the
    // changelist they were last working in. Gated on
    // `state.restore_last_path`: when the operator disabled the feature
    // we skip the write entirely so no dead data accumulates in
    // `admin_user_pref.preferences`.
    if state.restore_last_path {
        let qs = serialize_table_pref(&pref).unwrap_or_default();
        let last_path = if qs.is_empty() {
            format!("{}/{}/", crate::branding::current().base_path, table)
        } else {
            format!("{}/{}/?{qs}", crate::branding::current().base_path, table)
        };
        if let Err(e) = crate::models::set_last_path(user.id, &last_path).await {
            tracing::warn!(
                user = user.id,
                table = %table,
                error = %e,
                "gaps2 #11: failed to persist last_path (continuing render)"
            );
        }
    }
    let _ = page;

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
        &active_filters,
        trash,
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
        &active_filters,
        pagination.page_size,
        pagination.offset(),
        trash,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    // gaps2 #35: trashed-row count drives the "Trash (N)" toggle badge.
    // Only computed for soft-delete models; everything else skips the
    // extra COUNT.
    let trashed_count: i64 = if soft_delete {
        DynQuerySet::for_meta(&model)
            .only_deleted()
            .count()
            .await
            .unwrap_or(0)
    } else {
        0
    };

    let mut facets: Vec<FilterFacet> = Vec::new();
    if let Some(c) = cfg {
        for field in &c.list_filter {
            facets.push(build_facet(&model, field).await);
        }
    }

    // gaps2 #35: in trash view, swap the configured actions for the
    // Restore / Delete-permanently built-ins; in the live view, a
    // soft-delete model's configured `delete_selected` already soft-
    // deletes (moves to trash) so no extra action is injected there.
    let configured_actions: &[crate::config::Action] =
        cfg.map(|c| c.actions.as_slice()).unwrap_or(&[]);
    let effective = crate::config::effective_actions(configured_actions, soft_delete, trash);
    let action_names: Vec<serde_json::Value> = handlers::descriptors_for(&effective);

    let has_search = cfg.is_some_and(|c| !c.search_fields.is_empty());
    let search_val = search_term.unwrap_or_default();
    let active_filter_list = build_active_filter_list(&model, &active_filters).await;
    let filter_qs = build_filter_qs(&active_filters);
    let filter_groups = build_filter_groups(&active_filters);
    let apps = sidebar_apps(&state, &user).await;
    let view_groups = view_groups(&state, &user).await;
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
            view_groups        => view_groups,
            active_table       => table,
            breadcrumbs        => breadcrumbs,
            column_widths      => column_widths_json,
            inline_edit_fields => inline_edit_fields,
            initial_theme      => initial_theme,
            perms              => perms,
            soft_delete        => soft_delete,
            trash              => trash,
            trashed_count      => trashed_count,
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

    // gaps2 #35: keep the fragment in sync with the changelist's trash
    // toggle so HTMX pagination / refresh within the trash view keeps
    // showing soft-deleted rows + the Restore / Delete-permanently set.
    let soft_delete = model.soft_delete;
    let trash = soft_delete && params.get("trash").map(|v| v == "1").unwrap_or(false);

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
        &active_filters,
        trash,
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
        &active_filters,
        pagination.page_size,
        pagination.offset(),
        trash,
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

    let configured_actions: &[crate::config::Action] =
        cfg.map(|c| c.actions.as_slice()).unwrap_or(&[]);
    let effective = crate::config::effective_actions(configured_actions, soft_delete, trash);
    let action_names: Vec<serde_json::Value> = handlers::descriptors_for(&effective);

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
            trash              => trash,
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

/// gaps2 #11 round 2 — `POST /admin/{table}/columns/{column}/toggle`.
/// Flips the column's visibility on the persisted prefs. Returns
/// 204 + an `HX-Trigger: refreshTable + showToast` so the existing
/// changelist HTMX listeners do the rest (refetch rows, paint the
/// new column set).
pub(crate) async fn toggle_column_visibility(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, column)): Path<(String, String)>,
) -> Response {
    use axum::http::StatusCode;
    let path = format!(
        "{}/{table}/columns/{column}/toggle",
        crate::branding::current().base_path
    );
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, _model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&user, &plugin_name, &table, crate::permcheck::Action::View).await
    {
        return r;
    }
    let _ = &state;
    let now_visible = match crate::models::toggle_table_col(user.id, &table, &column).await {
        Ok(v) => v,
        Err(e) => return AdminError::from(e).into_response(),
    };
    let message = if now_visible {
        format!("Column `{column}` shown")
    } else {
        format!("Column `{column}` hidden")
    };
    let trigger = serde_json::json!({
        "refreshTable": {},
        "showToast": { "message": message, "level": "success" },
    });
    axum::response::Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("HX-Trigger", trigger.to_string())
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::NO_CONTENT.into_response())
}

// =========================================================================
// gaps2 #11 — per-table preference round-trip helpers
// =========================================================================

/// Turn a saved [`crate::models::TablePref`] into the query string the
/// changelist URL expects — `search=foo&filter_status=active&sort=-price
/// &page_size=50`. Returns `None` when the pref is fully empty (no
/// filters, no search, no sort, no per_page override): in that case
/// the redirect is a no-op and we'd just paint the default
/// changelist anyway.
fn serialize_table_pref(pref: &crate::models::TablePref) -> Option<String> {
    let mut parts: Vec<(String, String)> = Vec::new();
    if !pref.search.is_empty() {
        parts.push(("search".to_string(), pref.search.clone()));
    }
    // Filters key-sorted for a stable URL so two browser tabs showing
    // the same prefs hit the same cache / proxy entry.
    let mut filter_keys: Vec<&String> = pref.filters.keys().collect();
    filter_keys.sort();
    for k in filter_keys {
        if let Some(v) = pref.filters.get(k) {
            if !v.is_empty() {
                parts.push((format!("filter_{k}"), v.clone()));
            }
        }
    }
    if !pref.sort.is_empty() {
        // Round-trip the `-col` / `col` shape to the changelist's
        // `?sort=col&order=desc` URL form.
        if let Some(col) = pref.sort.strip_prefix('-') {
            parts.push(("sort".to_string(), col.to_string()));
            parts.push(("order".to_string(), "desc".to_string()));
        } else {
            parts.push(("sort".to_string(), pref.sort.clone()));
        }
    }
    if let Some(ps) = pref.per_page {
        parts.push(("page_size".to_string(), ps.to_string()));
    }
    if parts.is_empty() {
        return None;
    }
    serde_urlencoded::to_string(&parts).ok()
}

/// Collapse parsed `sort_col` + `sort_order` into the `-col` / `col`
/// directive shape that the persisted [`crate::models::TablePref::sort`]
/// field uses. Empty `sort_col` → empty string (no override).
fn shape_sort_directive(sort_col: &str, sort_order: &str) -> String {
    if sort_col.is_empty() {
        return String::new();
    }
    if sort_order.eq_ignore_ascii_case("desc") {
        format!("-{sort_col}")
    } else {
        sort_col.to_string()
    }
}
