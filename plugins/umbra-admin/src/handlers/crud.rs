//! Classic CRUD handlers for the full-page admin: `detail`, `new_form`,
//! `create`, `edit_form`, `update`, `delete`, and the HTMX `delete`
//! variant (`htmx_delete`).
//!
//! The sheet variants for the same operations live in
//! `handlers::sheet`. `update` calls `sheet::edit_sheet_handler` when
//! the form posts `_save_continue` so the saved state re-renders into
//! the open sheet without a redirect.

use std::collections::HashMap;

use axum::extract::{Path, State};
use minijinja::context;
use umbra::web::{HeaderMap, IntoResponse, Redirect, Response, StatusCode};

use umbra::orm::DynQuerySet;

use crate::AdminState;
use crate::auth::require_staff;
use crate::discovery::{find_model, pk_column, user_theme};
use crate::engine::render;
use crate::error::AdminError;
use crate::handlers::sheet::edit_sheet_handler;
use crate::rows::{fetch_rows_filtered, insert_row, update_row};
use crate::util::{is_htmx, sanitise_form_error};
use crate::view::{form_fields_for, model_for_template, sidebar_apps};

/// `GET /admin/{table}/{id}` — read-only detail page.
pub(crate) async fn detail(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}");
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
        return AdminError::NotFound(format!("no row with {} = {}", pk.name, id)).into_response();
    };
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
        serde_json::json!({ "label": format!("#{id}"), "url": format!("/admin/{table}/{id}") }),
    ];
    let initial_theme = user_theme(&user).await;
    match render(
        "admin/detail.html",
        context!(
            user          => user.username.clone(),
            model         => model_for_template(&model),
            row           => row,
            pk            => pk.name.clone(),
            apps          => apps,
            active_table  => table,
            breadcrumbs   => breadcrumbs,
            initial_theme => initial_theme,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/{table}/new` — empty create form.
pub(crate) async fn new_form(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
) -> Response {
    let path = format!("/admin/{table}/new");
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let cfg = state.config_for(&table);
    let fields = form_fields_for(&model, None, cfg);
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
        serde_json::json!({ "label": "Add", "url": format!("/admin/{table}/new") }),
    ];
    let initial_theme = user_theme(&user).await;
    match render(
        "admin/form.html",
        context!(
            user          => user.username.clone(),
            model         => model_for_template(&model),
            fields        => fields,
            verb          => "Create",
            action        => format!("/admin/{}/new", model.table),
            error         => "",
            apps          => apps,
            active_table  => table,
            breadcrumbs   => breadcrumbs,
            initial_theme => initial_theme,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /admin/{table}/new` — full-page create. Audit-logs the insert
/// then redirects to the changelist; on failure re-renders the form
/// with the inline error.
pub(crate) async fn create(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/new");
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(e) => return AdminError::BadInput(e.to_string()).into_response(),
    };
    let cfg = state.config_for(&table);
    let pool = umbra::db::pool();
    match insert_row(&pool, &model, &form, cfg).await {
        Ok(_) => {
            crate::models::log(
                user.id,
                "create",
                &table,
                None,
                &format!("created {} (via form)", model.name),
            )
            .await;
            Redirect::to(&format!("/admin/{}/", model.table)).into_response()
        }
        Err(e) => {
            let fields = form_fields_for(&model, Some(&form), cfg);
            let apps = sidebar_apps(&state, &user);
            let breadcrumbs = vec![
                serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
                serde_json::json!({ "label": "Add", "url": format!("/admin/{table}/new") }),
            ];
            let initial_theme = user_theme(&user).await;
            match render(
                "admin/form.html",
                context!(
                    user          => user.username.clone(),
                    model         => model_for_template(&model),
                    fields        => fields,
                    verb          => "Create",
                    action        => format!("/admin/{}/new", model.table),
                    error         => sanitise_form_error(&e),
                    apps          => apps,
                    active_table  => table,
                    breadcrumbs   => breadcrumbs,
                    initial_theme => initial_theme,
                ),
            ) {
                Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                Err(e2) => e2.into_response(),
            }
        }
    }
}

/// `GET /admin/{table}/{id}/edit` — prefilled edit form.
pub(crate) async fn edit_form(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/edit");
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
        return AdminError::NotFound(format!("no row with {} = {}", pk.name, id)).into_response();
    };
    let row_strings: HashMap<String, String> =
        row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let cfg = state.config_for(&table);
    let fields = form_fields_for(&model, Some(&row_strings), cfg);
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
        serde_json::json!({ "label": format!("#{id}"), "url": format!("/admin/{table}/{id}") }),
        serde_json::json!({ "label": "Edit", "url": format!("/admin/{table}/{id}/edit") }),
    ];
    let initial_theme = user_theme(&user).await;
    match render(
        "admin/form.html",
        context!(
            user          => user.username.clone(),
            model         => model_for_template(&model),
            fields        => fields,
            verb          => "Edit",
            action        => format!("/admin/{}/{}/edit", model.table, id),
            row           => row,
            pk            => pk.name.clone(),
            error         => "",
            apps          => apps,
            active_table  => table,
            breadcrumbs   => breadcrumbs,
            initial_theme => initial_theme,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /admin/{table}/{id}/edit` — full-page or HTMX update.
///
/// Two HTMX cases:
///
///   1. `_save_continue` is present in the form: re-render the edit
///      sheet so the user can keep editing.
///   2. Plain Save: emit an `HX-Trigger` that closes the sheet and
///      refreshes the table, all without a full page nav.
pub(crate) async fn update(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/{id}/edit");
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
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(e) => return AdminError::BadInput(e.to_string()).into_response(),
    };
    let pool = umbra::db::pool();
    let cfg = state.config_for(&table);
    match update_row(&pool, &model, pk, &id, &form, cfg).await {
        Ok(_) => {
            let object_id = id.parse::<i64>().ok();
            crate::models::log(
                user.id,
                "update",
                &table,
                object_id,
                &format!("updated {} #{}", model.name, id),
            )
            .await;

            if is_htmx(&headers) {
                if form.contains_key("_save_continue") {
                    return edit_sheet_handler(State(state), headers, Path((table, id))).await;
                }
                let mut resp = axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header("HX-Trigger", r#"{"closeSheet": {}, "refreshTable": {}}"#)
                    .body(axum::body::Body::empty())
                    .unwrap();
                resp.headers_mut()
                    .insert("Content-Type", "text/html; charset=utf-8".parse().unwrap());
                return resp;
            }

            Redirect::to(&format!("/admin/{}/{}", model.table, id)).into_response()
        }
        Err(e) => {
            let fields = form_fields_for(&model, Some(&form), cfg);
            let apps = sidebar_apps(&state, &user);
            let breadcrumbs = vec![
                serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
                serde_json::json!({ "label": format!("#{id}"), "url": format!("/admin/{table}/{id}") }),
                serde_json::json!({ "label": "Edit", "url": format!("/admin/{table}/{id}/edit") }),
            ];
            let initial_theme = user_theme(&user).await;
            match render(
                "admin/form.html",
                context!(
                    user          => user.username.clone(),
                    model         => model_for_template(&model),
                    fields        => fields,
                    verb          => "Edit",
                    action        => format!("/admin/{}/{}/edit", model.table, id),
                    error         => sanitise_form_error(&e),
                    apps          => apps,
                    active_table  => table,
                    breadcrumbs   => breadcrumbs,
                    initial_theme => initial_theme,
                ),
            ) {
                Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                Err(e2) => e2.into_response(),
            }
        }
    }
}

/// `POST /admin/{table}/{id}/delete` — legacy form-POST delete.
pub(crate) async fn delete(
    State(_state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/delete");
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    match DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .delete()
        .await
    {
        Ok(_) => {
            let object_id = id.parse::<i64>().ok();
            crate::models::log(
                who.id,
                "delete",
                &table,
                object_id,
                &format!("deleted {} #{}", model.name, id),
            )
            .await;
            Redirect::to(&format!("/admin/{}/", model.table)).into_response()
        }
        Err(e) => AdminError::Sqlx(e).into_response(),
    }
}

/// `DELETE /admin/{table}/{id}` — HTMX delete. Returns `HX-Redirect`
/// so HTMX reloads the changelist after the row is gone.
pub(crate) async fn htmx_delete(
    State(_state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}");
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    match DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .delete()
        .await
    {
        Ok(_) => {
            let object_id = id.parse::<i64>().ok();
            crate::models::log(
                who.id,
                "delete",
                &table,
                object_id,
                &format!("deleted {} #{}", model.name, id),
            )
            .await;
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("HX-Redirect", format!("/admin/{}/", model.table))
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| {
                    Redirect::to(&format!("/admin/{}/", model.table)).into_response()
                })
        }
        Err(e) => AdminError::Sqlx(e).into_response(),
    }
}
