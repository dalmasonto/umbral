//! Inline cell-edit endpoints — click a cell, mutate the value
//! in-place via HTMX, no full sheet open.

use std::collections::HashMap;

use axum::extract::{Path, State};
use umbra::web::{HeaderMap, IntoResponse, Response, StatusCode};

use umbra::orm::DynQuerySet;

use crate::auth::require_staff;
use crate::discovery::{find_model, pk_column};
use crate::error::AdminError;
use crate::rows::fetch_rows_filtered;
use crate::util::{html_escape, sanitise_form_error};
use crate::view::input_kind;
use crate::AdminState;

/// `GET /admin/{table}/{id}/cell/{field}/edit` — return the field
/// editor for a single cell (HTMX swap into the `<td>`).
pub(crate) async fn cell_edit_get(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id, field)): Path<(String, String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/cell/{field}/edit");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render("no pk".to_string()).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    };
    let cfg = state.config_for(&table);
    let is_readonly = cfg.is_some_and(|c| c.readonly_fields.contains(&field));
    if is_readonly {
        return (StatusCode::FORBIDDEN, "field is read-only").into_response();
    }

    let pool = umbra::db::pool();
    let all_cols: Vec<String> = model.fields.iter().map(|f| f.name.clone()).collect();
    let rows = match fetch_rows_filtered(
        &pool,
        &model,
        Some((&pk.name, &id)),
        &all_cols,
        "",
        None,
        None,
        None,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let Some(row) = rows.into_iter().next() else {
        return AdminError::NotFound(format!("no row {id}")).into_response();
    };
    let value = row.get(&field).cloned().unwrap_or_default();
    let input_type = input_kind(col.ty);

    let html = format!(
        r#"<form
            hx-post="/admin/{table}/{id}/cell/{field}"
            hx-target="closest td"
            hx-swap="innerHTML"
            class="flex items-center gap-xs"
            onkeydown="if(event.key==='Escape'){{this.parentElement && (this.parentElement.innerHTML = '<span class=&quot;text-on-surface text-body-md tabular-nums&quot;>{escaped_value}</span>')}}"
          >
          <input type="{input_type}" name="{field}" value="{escaped_value}"
            class="flex-1 bg-surface-container-low border border-primary rounded-lg px-sm py-xs text-on-surface text-body-md focus:outline-none focus:ring-1 focus:ring-primary"
            autofocus
            onblur="this.form.requestSubmit()"
          />
          <button type="submit" class="p-xs text-primary hover:bg-primary/10 rounded" title="Save">
            <i data-lucide="check" class="w-3 h-3"></i>
          </button>
        </form>
        <script>if(window.lucide)lucide.createIcons();</script>"#,
        table = table,
        id = id,
        field = field,
        input_type = input_type,
        escaped_value = html_escape(&value),
    );
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(axum::body::Body::from(html))
        .unwrap_or_else(|_| StatusCode::OK.into_response())
}

/// `POST /admin/{table}/{id}/cell/{field}` — save the inline cell
/// edit. Returns the read-only cell value on success or an error span
/// on failure (both as HTML for the HTMX swap).
pub(crate) async fn cell_edit_post(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id, field)): Path<(String, String, String)>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/{id}/cell/{field}");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render("no pk".to_string()).into_response();
    };
    if !model.fields.iter().any(|c| c.name == field) {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    }
    let cfg = state.config_for(&table);
    if cfg.is_some_and(|c| c.readonly_fields.contains(&field)) {
        return (StatusCode::FORBIDDEN, "field is read-only").into_response();
    }
    let form: HashMap<String, String> = serde_urlencoded::from_str(&body).unwrap_or_default();
    let new_value = form.get(&field).cloned().unwrap_or_default();
    match DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .update_one(&field, &new_value)
        .await
    {
        Ok(_) => {
            let display = html_escape(&new_value);
            let cell_html = format!(
                r#"<span class="text-on-surface text-body-md tabular-nums">{display}</span>"#
            );
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(axum::body::Body::from(cell_html))
                .unwrap_or_else(|_| StatusCode::OK.into_response())
        }
        Err(e) => {
            // sqlx::Error::Protocol carries WriteError messages; sanitise_form_error
            // already special-cases AdminError::Sqlx vs the others.
            let msg = sanitise_form_error(&AdminError::Sqlx(e));
            let err_html = format!(
                r#"<span class="text-error text-body-sm">{}</span>"#,
                html_escape(&msg)
            );
            axum::response::Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "text/html")
                .body(axum::body::Body::from(err_html))
                .unwrap_or_else(|_| StatusCode::BAD_REQUEST.into_response())
        }
    }
}
