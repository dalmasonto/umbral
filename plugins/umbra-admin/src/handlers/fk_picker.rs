//! Foreign-key combobox endpoints — paginated search (`fk_options`)
//! and pre-selected label resolution (`fk_options_resolve`).
//!
//! ⚠ Raw SQL. Same constraint as `palette_search`: the related table
//! and label column are resolved at request time from `ModelMeta`, so
//! the ORM's typed `QuerySet` can't express the query yet. A future
//! ORM extension (runtime-typed paginated SELECT over arbitrary
//! ModelMeta) takes this over.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use umbra::orm::{DynQuerySet, SqlType};
use umbra::web::{HeaderMap, IntoResponse, Json, Response, StatusCode};

use crate::AdminState;
use crate::auth::require_staff;
use crate::discovery::{find_model, pk_column};
use crate::error::AdminError;
use crate::util::{html_escape, is_htmx, urlencoding_simple};

/// `GET /admin/api/{table}/{field}/options?search=&page=&page_size=20`
///
/// Returns paginated label+value options for an FK field. HTMX
/// requests get an HTML fragment (the combobox dropdown body); plain
/// requests get JSON for programmatic consumers.
///
/// URL semantics: `{table}` is the PARENT model's table; `{field}`
/// is the FK column on it. The handler looks the column up,
/// follows `col.fk_target` to find the target model, and serves
/// options from there. The templates must construct the URL with
/// the parent's table — not the FK target — or the lookup 404s
/// with "no field `<fk_col>` on `<target_table>`" (the latter
/// table has no such column, by design).
pub(crate) async fn fk_options(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, field)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!(
        "{}/api/{table}/{field}/options",
        crate::branding::current().base_path
    );
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    // Resolve `field` against both regular columns (FK case) and the
    // model's M2M relations. M2M fields aren't in `model.fields` —
    // they live in `model.m2m_relations` and target a different table.
    // Either path yields the same `related_table` the picker queries.
    let related_table = if let Some(col) = model.fields.iter().find(|c| c.name == field) {
        col.fk_target
            .clone()
            .unwrap_or_else(|| field.trim_end_matches("_id").to_string())
    } else if let Some(rel) = model.m2m_relations.iter().find(|r| r.field_name == field) {
        rel.target_table.clone()
    } else {
        return AdminError::NotFound(format!("no field `{field}` on `{table}`")).into_response();
    };
    let Some((_, related_model)) = find_model(&related_table) else {
        return (
            StatusCode::FORBIDDEN,
            format!("related model `{related_table}` not found or not viewable"),
        )
            .into_response();
    };

    let search = params.get("search").map(|s| s.as_str()).unwrap_or("");
    let page: usize = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let page_size: usize = params
        .get("page_size")
        .and_then(|p| p.parse().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    let offset = (page - 1) * page_size;

    // Pick a label column: first non-PK text column.
    let label_col = related_model
        .fields
        .iter()
        .find(|c| !c.primary_key && matches!(c.ty, SqlType::Text))
        .map(|c| c.name.as_str())
        .unwrap_or("id");

    // Related model's search_fields from the admin config if registered.
    let rel_cfg = state.config_for(&related_table);
    let search_cols: Vec<String> = rel_cfg
        .filter(|c| !c.search_fields.is_empty())
        .map(|c| c.search_fields.clone())
        .unwrap_or_else(|| vec![label_col.to_string()]);

    let pk_col_name = pk_column(&related_model)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "id".to_string());
    let label_col_owned = label_col.to_string();
    let select_cols = if pk_col_name == label_col_owned {
        vec![pk_col_name.clone()]
    } else {
        vec![pk_col_name.clone(), label_col_owned.clone()]
    };

    // COUNT(*) and the paginated SELECT share the same WHERE clause —
    // build the chain once, count off a clone, page off the original.
    let base = DynQuerySet::for_meta(&related_model).search(&search_cols, search);
    let total: i64 = match base.count().await {
        Ok(t) => t,
        Err(e) => return AdminError::from(e).into_response(),
    };

    let rows = match DynQuerySet::for_meta(&related_model)
        .search(&search_cols, search)
        .select_cols(&select_cols)
        .order_by_col(&pk_col_name, true)
        .limit(page_size as u64)
        .offset(offset as u64)
        .fetch_as_strings()
        .await
    {
        Ok(r) => r,
        Err(e) => return AdminError::from(e).into_response(),
    };

    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let raw_pk = r.get(&pk_col_name).cloned().unwrap_or_default();
            let value: i64 = raw_pk.parse().unwrap_or(0);
            let label: String = r
                .get(&label_col_owned)
                .cloned()
                .unwrap_or_else(|| format!("#{value}"));
            serde_json::json!({ "value": value, "label": label })
        })
        .collect();

    let has_more = (offset + page_size) < total as usize;

    if is_htmx(&headers) {
        let mut html = String::new();
        html.push_str(r#"<div class="py-xs">"#);
        for item in &items {
            let value = item["value"].as_i64().unwrap_or(0);
            let label = item["label"].as_str().unwrap_or("");
            let escaped_label = html_escape(label);
            html.push_str(&format!(
                r##"<button type="button" data-fk-value="{value}" data-fk-label="{escaped_label}" class="w-full text-left px-md py-sm hover:bg-surface-container-high font-body-md text-on-surface transition-colors"><span class="block truncate"><span class="font-medium tabular-nums">{value}</span><span class="text-outline">: </span>{escaped_label}</span></button>"##
            ));
        }
        if items.is_empty() {
            html.push_str(
                r#"<p class="px-md py-sm text-outline text-body-sm italic">No results</p>"#,
            );
        }
        html.push_str("</div>");
        if total > page_size as i64 {
            let prev_page = page.saturating_sub(1).max(1);
            let next_page = page + 1;
            let encoded_search = urlencoding_simple(search);
            let prev_disabled = if page <= 1 { " disabled" } else { "" };
            let next_disabled = if !has_more { " disabled" } else { "" };
            html.push_str(&format!(
                r##"<div class="flex items-center justify-between gap-sm border-t border-outline-variant px-sm py-xs"><button type="button" class="px-sm py-xs rounded-lg border border-outline-variant text-label-sm text-on-surface-variant hover:bg-surface-container-high disabled:opacity-40 disabled:cursor-not-allowed" hx-get="/admin/api/{table}/{field}/options?search={encoded_search}&page={prev_page}&page_size={page_size}" hx-target="closest .fk-options" hx-swap="innerHTML"{prev_disabled}>Previous</button><span class="text-label-sm text-outline tabular-nums">Page {page}</span><button type="button" class="px-sm py-xs rounded-lg border border-outline-variant text-label-sm text-on-surface-variant hover:bg-surface-container-high disabled:opacity-40 disabled:cursor-not-allowed" hx-get="/admin/api/{table}/{field}/options?search={encoded_search}&page={next_page}&page_size={page_size}" hx-target="closest .fk-options" hx-swap="innerHTML"{next_disabled}>Next</button></div>"##
            ));
        }
        return axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html")
            .body(axum::body::Body::from(html))
            .unwrap_or_else(|_| StatusCode::OK.into_response());
    }

    Json(serde_json::json!({
        "items": items,
        "page": page,
        "has_more": has_more,
    }))
    .into_response()
}

/// `GET /admin/api/{table}/{field}/options/resolve?ids=1,2,3`
///
/// Returns labels for pre-selected ids — used on edit-form load so
/// the combobox can render the existing FK value's label before the
/// user has typed a search query. Same URL semantics as `fk_options`:
/// `{table}` is the parent's table, `{field}` is the FK column on
/// it; the handler derives the target via `col.fk_target`.
pub(crate) async fn fk_options_resolve(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, field)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!(
        "{}/api/{table}/{field}/options/resolve",
        crate::branding::current().base_path
    );
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    // Same dual-lookup as `fk_options`: FK columns live in
    // `model.fields`, M2M relations in `model.m2m_relations`. Either
    // path resolves to the same related table.
    let related_table = if let Some(col) = model.fields.iter().find(|c| c.name == field) {
        col.fk_target
            .clone()
            .unwrap_or_else(|| field.trim_end_matches("_id").to_string())
    } else if let Some(rel) = model.m2m_relations.iter().find(|r| r.field_name == field) {
        rel.target_table.clone()
    } else {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    };
    let Some((_, related_model)) = find_model(&related_table) else {
        return (StatusCode::FORBIDDEN, "related model not found").into_response();
    };

    let ids_param = params.get("ids").cloned().unwrap_or_default();
    let ids: Vec<i64> = ids_param
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if ids.is_empty() {
        return Json(serde_json::json!({ "items": [] })).into_response();
    }

    let label_col_owned = related_model
        .fields
        .iter()
        .find(|c| !c.primary_key && matches!(c.ty, SqlType::Text))
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "id".to_string());
    let pk_col_name = pk_column(&related_model)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "id".to_string());

    let select_cols = if pk_col_name == label_col_owned {
        vec![pk_col_name.clone()]
    } else {
        vec![pk_col_name.clone(), label_col_owned.clone()]
    };

    let _ = &state; // referenced only above

    match DynQuerySet::for_meta(&related_model)
        .select_cols(&select_cols)
        .filter_in_i64(&pk_col_name, &ids)
        .fetch_as_strings()
        .await
    {
        Ok(rows) => {
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    let value: i64 = r
                        .get(&pk_col_name)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let label: String = r
                        .get(&label_col_owned)
                        .cloned()
                        .unwrap_or_else(|| format!("#{value}"));
                    serde_json::json!({ "value": value, "label": label })
                })
                .collect();
            Json(serde_json::json!({ "items": items })).into_response()
        }
        Err(e) => AdminError::from(e).into_response(),
    }
}
