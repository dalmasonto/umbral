//! Right-side sheet fragments — preview, edit, create — plus the
//! confirm-delete dialog, the sheet-create POST handler, and the
//! change-password handler that the edit sheet's "Change password"
//! button hits.

use std::collections::HashMap;

use axum::extract::{Path, State};
use minijinja::context;
use umbra::web::{HeaderMap, IntoResponse, Redirect, Response, StatusCode};

use umbra::orm::DynQuerySet;

use crate::AdminState;
use crate::auth::require_staff;
use crate::discovery::{find_model, pk_column};
use crate::engine::render;
use crate::error::AdminError;
use crate::rows::{fetch_rows_filtered, insert_row};
use crate::util::{is_htmx, sanitise_form_error};
use crate::view::{form_fields_for, model_for_template};

/// `GET /admin/{table}/{id}/sheet` — preview sheet fragment. Falls
/// back to redirecting non-HTMX requests to the changelist with
/// `?row=<id>` so the JS can open the sheet on load.
pub(crate) async fn preview_sheet(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/sheet");
    let _user = match require_staff(&headers, &path).await {
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
    let model_view = model_for_template(&model);

    if is_htmx(&headers) {
        match render(
            "admin/sheet_preview.html",
            context!(
                model       => model_view,
                instance_id => id,
                fields      => fields,
            ),
        ) {
            Ok(html) => html.into_response(),
            Err(e) => e.into_response(),
        }
    } else {
        Redirect::to(&format!("/admin/{table}/?row={id}")).into_response()
    }
}

/// `GET /admin/{table}/{id}/edit-sheet` — edit sheet fragment. Also
/// called directly by `update` on `_save_continue` to re-render after a
/// save without closing the sheet.
pub(crate) async fn edit_sheet_handler(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/edit-sheet");
    let _user = match require_staff(&headers, &path).await {
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
    let model_view = model_for_template(&model);

    let password_field = cfg.and_then(|c| c.password_field.as_deref()).unwrap_or("");

    if is_htmx(&headers) {
        match render(
            "admin/sheet_edit.html",
            context!(
                model          => model_view,
                instance_id    => id,
                fields         => fields,
                error          => "",
                password_field => password_field,
            ),
        ) {
            Ok(html) => html.into_response(),
            Err(e) => e.into_response(),
        }
    } else {
        Redirect::to(&format!("/admin/{table}/?row={id}")).into_response()
    }
}

/// `GET /admin/{table}/new-sheet` — create sheet fragment.
pub(crate) async fn new_sheet(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
) -> Response {
    let path = format!("/admin/{table}/new-sheet");
    let _user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let cfg = state.config_for(&table);
    let fields = form_fields_for(&model, None, cfg);
    let model_view = model_for_template(&model);

    match render(
        "admin/sheet_create.html",
        context!(
            model       => model_view,
            instance_id => "",
            fields      => fields,
            error       => "",
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/{table}/{id}/_confirm-delete` — delete confirm modal.
pub(crate) async fn confirm_delete_dialog(
    State(_state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/_confirm-delete");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let model_view = model_for_template(&model);
    // Use the id as the display label — FK label resolution lands later.
    let display_label = format!("#{id}");
    match render(
        "admin/confirm_delete.html",
        context!(
            model         => model_view,
            instance_id   => id,
            display_label => display_label,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /admin/{table}/create` — sheet create flow. On success,
/// returns an `HX-Redirect` so HTMX reloads the full changelist with
/// the new row in place; on failure, returns the create-sheet
/// fragment with the inline error.
pub(crate) async fn sheet_create(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/create");
    let who = match require_staff(&headers, &path).await {
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
                who.id,
                "create",
                &table,
                None,
                &format!("created {} (via sheet)", model.name),
            )
            .await;
            if is_htmx(&headers) {
                axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header("HX-Redirect", format!("/admin/{}/", model.table))
                    .body(axum::body::Body::empty())
                    .unwrap_or_else(|_| {
                        Redirect::to(&format!("/admin/{}/", model.table)).into_response()
                    })
            } else {
                Redirect::to(&format!("/admin/{}/", model.table)).into_response()
            }
        }
        Err(e) => {
            let fields = form_fields_for(&model, Some(&form), cfg);
            let model_view = model_for_template(&model);
            match render(
                "admin/sheet_create.html",
                context!(
                    model       => model_view,
                    instance_id => "",
                    fields      => fields,
                    error       => sanitise_form_error(&e),
                ),
            ) {
                Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                Err(e2) => e2.into_response(),
            }
        }
    }
}

/// `POST /admin/{table}/{id}/change-password` — dedicated password
/// change endpoint for any model that configures `password_field`.
/// Body: `new_password` + `confirm_password`. On success, returns an
/// `HX-Trigger` with a toast event so the UI can react.
pub(crate) async fn change_password_handler(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/{id}/change-password");
    let actor = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let cfg = state.config_for(&table);
    let pw_col = match cfg.and_then(|c| c.password_field.as_deref()) {
        Some(col) => col,
        None => {
            return AdminError::BadInput("no password_field configured for this model".to_string())
                .into_response();
        }
    };
    let form: HashMap<String, String> = serde_urlencoded::from_str(&body).unwrap_or_default();
    let new_pw = form.get("new_password").map(|s| s.as_str()).unwrap_or("");
    let confirm_pw = form
        .get("confirm_password")
        .map(|s| s.as_str())
        .unwrap_or("");

    if new_pw.is_empty() {
        return AdminError::BadInput("Password cannot be empty".to_string()).into_response();
    }
    if new_pw != confirm_pw {
        return AdminError::BadInput("Passwords do not match".to_string()).into_response();
    }
    let hash = match umbra_auth::hash_password(new_pw) {
        Ok(h) => h,
        Err(e) => {
            return AdminError::BadInput(format!("password hashing failed: {e}")).into_response();
        }
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render("no pk".to_string()).into_response();
    };
    if let Err(e) = DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .update_one(pw_col, &hash)
        .await
    {
        return AdminError::Sqlx(e).into_response();
    }
    // Audit log — password change is a special-cased update we want
    // visible in the timeline. Don't log the new hash itself.
    let object_id = id.parse::<i64>().ok();
    crate::models::log(
        actor.id,
        "update",
        &table,
        object_id,
        &format!("changed password on {} #{}", model.name, id),
    )
    .await;
    let trigger = serde_json::json!({
        "showToast": { "message": "Password changed successfully.", "level": "success" },
        "closeDialog": {}
    });
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("HX-Trigger", trigger.to_string())
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::OK.into_response())
}
