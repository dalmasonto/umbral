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
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    // WEB-7: the FK autocomplete returns rows of a model; without a
    // permission gate any staff user could enumerate a model they have no
    // `view_<model>` right to. Require View on the parent table, mirroring
    // the CRUD/list handlers (a no-permissions install still passes).
    if let Err(r) =
        crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::View).await
    {
        return r;
    }
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

    // PK lift Pass B: render the PK in whatever shape it lives in
    // the DB. Pre-fix, `let value: i64 = raw_pk.parse().unwrap_or(0)`
    // silently rewrote every non-integer PK to 0 — so a String-PK
    // model (e.g. `permissions_permission` keyed by codename) ended
    // up with every row sharing `value: 0` and the picker becoming
    // unusable. The JSON payload now emits the raw value (Number for
    // integer PKs, String for codename / slug / UUID); the HTML
    // template renders it via `Display`.
    let pk_col_type = related_model
        .fields
        .iter()
        .find(|c| c.name == pk_col_name)
        .map(|c| c.ty);
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let raw_pk = r.get(&pk_col_name).cloned().unwrap_or_default();
            let value_json = pk_string_to_json(&raw_pk, pk_col_type);
            let label: String = r
                .get(&label_col_owned)
                .cloned()
                .unwrap_or_else(|| format!("#{raw_pk}"));
            serde_json::json!({ "value": value_json, "label": label })
        })
        .collect();

    let has_more = (offset + page_size) < total as usize;

    if is_htmx(&headers) {
        let mut html = String::new();
        html.push_str(r#"<div class="py-xs">"#);
        for item in &items {
            // `value` may be a Number or a String (PK lift Pass B).
            // `data-fk-value` is a string attribute either way, so
            // render via Display on the underlying JSON value.
            let value_display = match &item["value"] {
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let label = item["label"].as_str().unwrap_or("");
            let escaped_value = html_escape(&value_display);
            let escaped_label = html_escape(label);
            html.push_str(&format!(
                r##"<button type="button" data-fk-value="{escaped_value}" data-fk-label="{escaped_label}" class="w-full text-left px-md py-sm hover:bg-surface-container-high font-body-md text-on-surface transition-colors"><span class="block truncate"><span class="font-medium tabular-nums">{escaped_value}</span><span class="text-outline">: </span>{escaped_label}</span></button>"##
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
            let base = crate::branding::current().base_path;
            html.push_str(&format!(
                r##"<div class="flex items-center justify-between gap-sm border-t border-outline-variant px-sm py-xs"><button type="button" class="px-sm py-xs rounded-lg border border-outline-variant text-label-sm text-on-surface-variant hover:bg-surface-container-high disabled:opacity-40 disabled:cursor-not-allowed" hx-get="{base}/api/{table}/{field}/options?search={encoded_search}&page={prev_page}&page_size={page_size}" hx-target="closest .fk-options" hx-swap="innerHTML"{prev_disabled}>Previous</button><span class="text-label-sm text-outline tabular-nums">Page {page}</span><button type="button" class="px-sm py-xs rounded-lg border border-outline-variant text-label-sm text-on-surface-variant hover:bg-surface-container-high disabled:opacity-40 disabled:cursor-not-allowed" hx-get="{base}/api/{table}/{field}/options?search={encoded_search}&page={next_page}&page_size={page_size}" hx-target="closest .fk-options" hx-swap="innerHTML"{next_disabled}>Next</button></div>"##
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
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    // WEB-7: the FK autocomplete returns rows of a model; without a
    // permission gate any staff user could enumerate a model they have no
    // `view_<model>` right to. Require View on the parent table, mirroring
    // the CRUD/list handlers (a no-permissions install still passes).
    if let Err(r) =
        crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::View).await
    {
        return r;
    }
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

    // PK lift Pass B: `?ids=` now carries raw PK strings (could be
    // "1,2,3" for integer PKs OR "blog.publish_post,blog.edit_post"
    // for String-PK models like `permissions_permission`). Bind via
    // `filter_in_strings` which dispatches on the column's SqlType
    // and coerces accordingly — so the same parser works for either
    // shape without the caller having to know in advance.
    let ids_param = params.get("ids").cloned().unwrap_or_default();
    let ids: Vec<String> = ids_param
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        })
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
    let pk_col = pk_column(&related_model);
    let pk_col_name = pk_col
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "id".to_string());
    let pk_col_type = pk_col.as_ref().map(|c| c.ty);

    let select_cols = if pk_col_name == label_col_owned {
        vec![pk_col_name.clone()]
    } else {
        vec![pk_col_name.clone(), label_col_owned.clone()]
    };

    let _ = &state; // referenced only above

    match DynQuerySet::for_meta(&related_model)
        .select_cols(&select_cols)
        .filter_in_strings(&pk_col_name, &ids)
        .fetch_as_strings()
        .await
    {
        Ok(rows) => {
            // Same `value` rendering rule as `fk_options`: emit the
            // PK in whatever JSON shape matches its SqlType so the
            // frontend's hidden input round-trips the right value.
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    let raw_pk = r.get(&pk_col_name).cloned().unwrap_or_default();
                    let value_json = pk_string_to_json(&raw_pk, pk_col_type);
                    let label: String = r
                        .get(&label_col_owned)
                        .cloned()
                        .unwrap_or_else(|| format!("#{raw_pk}"));
                    serde_json::json!({ "value": value_json, "label": label })
                })
                .collect();
            Json(serde_json::json!({ "items": items })).into_response()
        }
        Err(e) => AdminError::from(e).into_response(),
    }
}

/// PK lift Pass B helper — turn a stringified PK (the shape
/// `DynQuerySet::fetch_as_strings` produces) into the JSON value
/// the FK picker emits. Integer / FK / float PKs round-trip as
/// `serde_json::Value::Number`; Text / Uuid / unknown PKs land as
/// `Value::String`. The `Display` form is preserved for both —
/// HTMX consumers can read `data-fk-value` as a string verbatim,
/// and the JSON consumer's hidden input gets the right type when
/// submitted.
fn pk_string_to_json(raw: &str, pk_ty: Option<umbra::orm::SqlType>) -> serde_json::Value {
    use umbra::orm::SqlType;
    match pk_ty {
        Some(SqlType::SmallInt)
        | Some(SqlType::Integer)
        | Some(SqlType::BigInt)
        | Some(SqlType::ForeignKey) => raw
            .parse::<i64>()
            .ok()
            .map(serde_json::Value::from)
            .unwrap_or_else(|| serde_json::Value::String(raw.to_string())),
        Some(SqlType::Real) | Some(SqlType::Double) => raw
            .parse::<f64>()
            .ok()
            .map(serde_json::Value::from)
            .unwrap_or_else(|| serde_json::Value::String(raw.to_string())),
        // Text / Uuid / anything else: keep as string.
        _ => serde_json::Value::String(raw.to_string()),
    }
}
