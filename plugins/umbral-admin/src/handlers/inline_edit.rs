//! Inline cell-edit endpoints — click a cell, mutate the value
//! in-place via HTMX, no full sheet open.

use std::collections::HashMap;

use axum::extract::{Path, State};
use umbral::web::{HeaderMap, IntoResponse, Response, StatusCode};

use umbral::orm::DynQuerySet;

use crate::AdminState;
use crate::auth::require_staff;
use crate::discovery::{find_model, pk_column};
use crate::error::AdminError;
use crate::rows::fetch_rows_filtered;
use crate::util::{html_escape, sanitise_form_error};
use crate::view::input_kind;

/// `GET /admin/{table}/{id}/cell/{field}/edit` — return the field
/// editor for a single cell (HTMX swap into the `<td>`).
pub(crate) async fn cell_edit_get(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id, field)): Path<(String, String, String)>,
) -> Response {
    let path = format!(
        "{}/{table}/{id}/cell/{field}/edit",
        crate::branding::current().base_path
    );
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    if let Err(r) = crate::permcheck::require(
        &user,
        &plugin_name,
        &table,
        crate::permcheck::Action::Change,
    )
    .await
    {
        return r;
    }
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
    // Wave 4: file/image fields aren't inline-editable. The inline cell
    // POST path is urlencoded (single `name=value`), so it can't carry a
    // multipart upload; submitting one would write an empty value and
    // null the stored key. Uploads go through the full change form /
    // sheet (which switch to multipart). Refuse the cell editor.
    if matches!(input_kind(col), "file" | "image") {
        return (
            StatusCode::FORBIDDEN,
            "file fields can't be edited inline; use the change form",
        )
            .into_response();
    }

    let all_cols: Vec<String> = model.fields.iter().map(|f| f.name.clone()).collect();
    let rows = match fetch_rows_filtered(&model, Some((&pk.name, &id)), &all_cols).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let Some(row) = rows.into_iter().next() else {
        return AdminError::NotFound(format!("no row {id}")).into_response();
    };
    let value = row.get(&field).cloned().unwrap_or_default();
    let input_type = input_kind(col);
    let base = crate::branding::current().base_path;

    // The read-only value renders in two different contexts below and needs
    // two different encodings:
    //   * `value="…"` / the `<span>` text  → plain HTML escaping.
    //   * the `onkeydown` handler           → the value is placed inside a JS
    //     string that is then assigned to `innerHTML`, so it must be
    //     HTML-escaped (for the innerHTML layer) AND JS-escaped (so it can't
    //     break out of the string or the attribute). HTML escaping alone is
    //     unsafe here: `&#x27;` decodes to a real `'` in the attribute before
    //     the JS runs. See `util::escape_js`.
    let escaped_value = html_escape(&value);
    let js_escaped_value = crate::util::escape_js(&escaped_value);
    let html = format!(
        r#"<form
            hx-post="{base}/{table}/{id}/cell/{field}"
            hx-target="closest td"
            hx-swap="innerHTML"
            class="flex items-center gap-xs"
            onkeydown="if(event.key==='Escape'){{this.parentElement && (this.parentElement.innerHTML = '<span class=&quot;text-on-surface text-body-md tabular-nums&quot;>{js_escaped_value}</span>')}}"
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
    let path = format!(
        "{}/{table}/{id}/cell/{field}",
        crate::branding::current().base_path
    );
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    if let Err(r) = crate::permcheck::require(
        &user,
        &plugin_name,
        &table,
        crate::permcheck::Action::Change,
    )
    .await
    {
        return r;
    }
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render("no pk".to_string()).into_response();
    };
    let Some(col) = model.fields.iter().find(|c| c.name == field) else {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    };
    let cfg = state.config_for(&table);
    if cfg.is_some_and(|c| c.readonly_fields.contains(&field)) {
        return (StatusCode::FORBIDDEN, "field is read-only").into_response();
    }
    // gaps4 #1: a privileged / noform / noedit column is never inline-editable.
    // The full-form save path already refuses these (rows.rs skip set); this path
    // used to check only `readonly_fields`, so a staff user could inline-edit
    // `is_superuser`. Refuse here for a clean 403, and `update_one` refuses again
    // underneath as defense in depth.
    if col.privileged || col.noform || col.noedit {
        return (
            StatusCode::FORBIDDEN,
            "field is not editable inline; privileged or read-only columns must go \
             through the change form",
        )
            .into_response();
    }
    // Wave 4: reject inline edits of file/image columns (see
    // `cell_edit_get`). A urlencoded cell POST would null the stored key.
    if matches!(input_kind(col), "file" | "image") {
        return (
            StatusCode::FORBIDDEN,
            "file fields can't be edited inline; use the change form",
        )
            .into_response();
    }
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(form) => form,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid inline edit form body: {e}"),
            )
                .into_response();
        }
    };
    let new_value = form.get(&field).cloned().unwrap_or_default();
    match DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .update_one(&field, &new_value)
        .await
    {
        Ok(_) => {
            // Audit log — inline cell edit counts as an update.
            // gaps3 #59: the pk is text. Parsing it to i64 discarded it for every
            // Uuid/String-keyed model — the audit row named the table but not the row.
            let object_id = Some(id.clone());
            crate::models::log(
                user.id,
                "update",
                &table,
                object_id,
                &format!("inline-edited {}.{} (via cell)", model.name, field),
            )
            .await;
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
            // gaps2 #12: route through `AdminError::from(DynError)`
            // so Write(WriteError) variants reach `sanitise_form_error`
            // with the per-field structure intact. Pre-fix this site
            // always landed on the Sqlx arm because DynError was a
            // bare sqlx::Error alias.
            let msg = sanitise_form_error(&AdminError::from(e));
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
