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
use sqlx::Row;
use umbra::orm::SqlType;
use umbra::web::{HeaderMap, IntoResponse, Json, Response, StatusCode};

use crate::auth::require_staff;
use crate::discovery::{find_model, pk_column};
use crate::error::AdminError;
use crate::util::{html_escape, is_htmx, q};
use crate::AdminState;

/// `GET /admin/api/{table}/{field}/options?search=&page=&page_size=20`
///
/// Returns paginated label+value options for an FK field. HTMX
/// requests get an HTML fragment (the combobox dropdown body); plain
/// requests get JSON for programmatic consumers.
pub(crate) async fn fk_options(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, field)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/api/{table}/{field}/options");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}` on `{table}`")).into_response();
    };
    let related_table = col
        .fk_target
        .clone()
        .unwrap_or_else(|| field.trim_end_matches("_id").to_string());
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

    let pool = umbra::db::pool();

    let mut conditions: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    if !search.is_empty() {
        let like_clauses: Vec<String> = search_cols
            .iter()
            .map(|f| format!("\"{}\" LIKE ?", q(f)))
            .collect();
        if !like_clauses.is_empty() {
            conditions.push(format!("({})", like_clauses.join(" OR ")));
            let like_val = format!("%{search}%");
            for _ in &like_clauses {
                binds.push(like_val.clone());
            }
        }
    }
    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    let count_sql = format!("SELECT COUNT(*) FROM \"{}\"{where_sql}", q(&related_table));
    let mut count_qb = sqlx::query(&count_sql);
    for b in &binds {
        count_qb = count_qb.bind(b.clone());
    }
    let total: i64 = match count_qb.fetch_one(&pool).await {
        Ok(r) => r.try_get(0).unwrap_or(0),
        Err(e) => return AdminError::Sqlx(e).into_response(),
    };

    let pk_col = pk_column(&related_model)
        .map(|c| c.name.as_str())
        .unwrap_or("id");
    let select_sql = format!(
        "SELECT \"{pk_col}\", \"{label_col}\" FROM \"{}\"{where_sql} ORDER BY \"{pk_col}\" DESC LIMIT ? OFFSET ?",
        q(&related_table)
    );
    let mut qb = sqlx::query(&select_sql);
    for b in &binds {
        qb = qb.bind(b.clone());
    }
    qb = qb.bind(page_size as i64).bind(offset as i64);

    let rows = match qb.fetch_all(&pool).await {
        Ok(r) => r,
        Err(e) => return AdminError::Sqlx(e).into_response(),
    };

    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let value: i64 = r.try_get(0).unwrap_or(0);
            let label: String = r
                .try_get::<String, _>(1)
                .or_else(|_| r.try_get::<i64, _>(1).map(|v| v.to_string()))
                .unwrap_or_else(|_| format!("#{value}"));
            serde_json::json!({ "value": value, "label": label })
        })
        .collect();

    let has_more = (offset + page_size) < total as usize;

    if is_htmx(&headers) {
        let mut html = String::new();
        for item in &items {
            let value = item["value"].as_i64().unwrap_or(0);
            let label = item["label"].as_str().unwrap_or("");
            html.push_str(&format!(
                r#"<button type="button" data-fk-value="{value}" class="w-full text-left px-md py-sm hover:bg-surface-container-high font-body-md text-on-surface transition-colors">{}</button>"#,
                html_escape(label)
            ));
        }
        if html.is_empty() {
            html.push_str(
                r#"<p class="px-md py-sm text-outline text-body-sm italic">No results</p>"#,
            );
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
/// user has typed a search query.
pub(crate) async fn fk_options_resolve(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, field)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/api/{table}/{field}/options/resolve");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    };
    let related_table = col
        .fk_target
        .clone()
        .unwrap_or_else(|| field.trim_end_matches("_id").to_string());
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

    let label_col = related_model
        .fields
        .iter()
        .find(|c| !c.primary_key && matches!(c.ty, SqlType::Text))
        .map(|c| c.name.as_str())
        .unwrap_or("id");
    let pk_col = pk_column(&related_model)
        .map(|c| c.name.as_str())
        .unwrap_or("id");

    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT \"{pk_col}\", \"{label_col}\" FROM \"{}\" WHERE \"{pk_col}\" IN ({placeholders})",
        q(&related_table)
    );
    let pool = umbra::db::pool();
    let mut qb = sqlx::query(&sql);
    for id in &ids {
        qb = qb.bind(*id);
    }

    let _ = &state; // referenced only above; explicit drop kills the warning

    match qb.fetch_all(&pool).await {
        Ok(rows) => {
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    let value: i64 = r.try_get(0).unwrap_or(0);
                    let label: String = r
                        .try_get::<String, _>(1)
                        .or_else(|_| r.try_get::<i64, _>(1).map(|v| v.to_string()))
                        .unwrap_or_else(|_| format!("#{value}"));
                    serde_json::json!({ "value": value, "label": label })
                })
                .collect();
            Json(serde_json::json!({ "items": items })).into_response()
        }
        Err(e) => AdminError::Sqlx(e).into_response(),
    }
}
