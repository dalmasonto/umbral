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
use crate::rows::fetch_rows_filtered;
use crate::util::{
    apply_write_error_to_fields, is_htmx, parse_unique_violation_column, sanitise_form_error,
};
use crate::view::{form_fields_for, form_m2m_fields_for, model_for_template, validate_form};

/// `GET /admin/{table}/{id}/sheet` — preview sheet fragment. Falls
/// back to redirecting non-HTMX requests to the changelist with
/// `?row=<id>` so the JS can open the sheet on load.
pub(crate) async fn preview_sheet(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!(
        "{}/{table}/{id}/sheet",
        crate::branding::current().base_path
    );
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
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
    let all_cols: Vec<String> = model.fields.iter().map(|f| f.name.clone()).collect();
    let rows = match fetch_rows_filtered(&model, Some((&pk.name, &id)), &all_cols).await {
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
                perms       => perms,
            ),
        ) {
            Ok(html) => html.into_response(),
            Err(e) => e.into_response(),
        }
    } else {
        Redirect::to(&format!(
            "{}/{table}/?row={id}",
            crate::branding::current().base_path
        ))
        .into_response()
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
    let path = format!(
        "{}/{table}/{id}/edit-sheet",
        crate::branding::current().base_path
    );
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
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
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    let all_cols: Vec<String> = model.fields.iter().map(|f| f.name.clone()).collect();
    let rows = match fetch_rows_filtered(&model, Some((&pk.name, &id)), &all_cols).await {
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
    let m2m_fields = form_m2m_fields_for(&model, Some(&id)).await;
    // Inlines: existing children (prefilled) + `extra` blank rows, the
    // same builder the full-page `edit_form` uses.
    let inlines = match crate::inlines::build_inline_views(&model, Some(&id), cfg).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let model_view = model_for_template(&model);

    let password_field = cfg.and_then(|c| c.password_field.as_deref()).unwrap_or("");

    if is_htmx(&headers) {
        match render(
            "admin/sheet_edit.html",
            context!(
                model          => model_view,
                instance_id    => id,
                fields         => fields,
                m2m_fields     => m2m_fields,
                inlines        => inlines,
                error          => "",
                password_field => password_field,
            ),
        ) {
            Ok(html) => html.into_response(),
            Err(e) => e.into_response(),
        }
    } else {
        Redirect::to(&format!(
            "{}/{table}/?row={id}",
            crate::branding::current().base_path
        ))
        .into_response()
    }
}

/// `GET /admin/{table}/new-sheet` — create sheet fragment.
pub(crate) async fn new_sheet(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
) -> Response {
    let path = format!("{}/{table}/new-sheet", crate::branding::current().base_path);
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&user, &plugin_name, &table, crate::permcheck::Action::Add).await
    {
        return r;
    }
    let cfg = state.config_for(&table);
    let fields = form_fields_for(&model, None, cfg);
    let m2m_fields = form_m2m_fields_for(&model, None).await;
    // Inlines: blank `extra` rows only (no parent PK yet), mirroring the
    // full-page `new_form`.
    let inlines = match crate::inlines::build_inline_views(&model, None, cfg).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let model_view = model_for_template(&model);

    match render(
        "admin/sheet_create.html",
        context!(
            model       => model_view,
            instance_id => "",
            fields      => fields,
            m2m_fields  => m2m_fields,
            inlines     => inlines,
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
    let path = format!(
        "{}/{table}/{id}/_confirm-delete",
        crate::branding::current().base_path
    );
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) = crate::permcheck::require(
        &user,
        &plugin_name,
        &table,
        crate::permcheck::Action::Delete,
    )
    .await
    {
        return r;
    }
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
    body: axum::body::Bytes,
) -> Response {
    let path = format!("{}/{table}/create", crate::branding::current().base_path);
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::Add).await
    {
        return r;
    }
    // Wave 4: the sheet-create form submits every field (file/image
    // included), so decode urlencoded or multipart the same way the
    // full-page `crud::create` does, storing uploads via the ambient
    // Storage backend.
    let content_type = headers
        .get(umbra::web::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let pairs = match crate::handlers::crud::body_to_pairs(content_type, body).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let form: HashMap<String, String> = pairs.iter().cloned().collect();
    let multi_form: Vec<(String, String)> = pairs;
    let cfg = state.config_for(&table);
    // Validate up front and surface ALL field failures at once (each below its
    // own input), mirroring the full-page `crud::create`. Without this the sheet
    // form only ever showed a flattened top banner on a DB error; now a
    // required/choice/FK failure highlights the OFFENDING field. Re-render the
    // sheet fragment with the per-field errors instead of attempting the write.
    let field_errors = validate_form(&model, &form, cfg);
    if !field_errors.is_empty() {
        let mut fields = form_fields_for(&model, Some(&form), cfg);
        for f in &mut fields {
            if let Some(m) = field_errors.get(&f.name) {
                f.error = m.clone();
            }
        }
        let m2m_fields = form_m2m_fields_for(&model, None).await;
        let inlines = crate::inlines::build_inline_views_from_submitted(&model, cfg, &multi_form);
        let model_view = model_for_template(&model);
        return match render(
            "admin/sheet_create.html",
            context!(
                model       => model_view,
                instance_id => "",
                fields      => fields,
                m2m_fields  => m2m_fields,
                inlines     => inlines,
                error       => "",
            ),
        ) {
            Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
            Err(e2) => e2.into_response(),
        };
    }
    // Atomic save: parent INSERT + inline children in one transaction,
    // via the SAME shared path the full-page `crud::create` uses. A bad
    // child rolls back the parent too.
    match crate::handlers::crud::create_parent_and_inlines_pub(&model, &form, cfg, &multi_form).await
    {
        Ok(new_pk) => {
            // BUG-16 admin: apply M2M selections to the auto-junction
            // tables. Same shape as `crud::create`.
            if let Err(e) =
                crate::handlers::crud::apply_m2m_selections_pub(&model, &new_pk, &multi_form).await
            {
                return AdminError::Sqlx(e).into_response();
            }
            crate::models::log(
                who.id,
                "create",
                &table,
                None,
                &format!("created {} (via sheet)", model.name),
            )
            .await;
            if is_htmx(&headers) {
                // In-place refresh: close the sheet + re-fetch rows so
                // the new record appears without a full page nav.
                // Matches `crud::update`'s success path.
                //
                // gaps2 #13: emit `showToast` alongside `closeSheet` +
                // `refreshTable` so the user gets visible confirmation
                // a row landed. The wrapper.html listener at line ~1233
                // already handles `{message, level}` payloads.
                let trigger = serde_json::json!({
                    "closeSheet": {},
                    "refreshTable": {},
                    "showToast": {
                        "message": format!("{} created", model.name),
                        "level": "success"
                    },
                });
                let mut resp = axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header("HX-Trigger", trigger.to_string())
                    .body(axum::body::Body::empty())
                    .unwrap_or_else(|_| {
                        Redirect::to(&format!(
                            "{}/{}/",
                            crate::branding::current().base_path,
                            model.table
                        ))
                        .into_response()
                    });
                resp.headers_mut()
                    .insert("Content-Type", "text/html; charset=utf-8".parse().unwrap());
                resp
            } else {
                Redirect::to(&format!(
                    "{}/{}/",
                    crate::branding::current().base_path,
                    model.table
                ))
                .into_response()
            }
        }
        Err(e) => {
            let mut fields = form_fields_for(&model, Some(&form), cfg);
            let m2m_fields = form_m2m_fields_for(&model, None).await;
            // Repopulate the inline rows from the submission so a bad
            // child re-renders the sheet with the user's in-flight inline
            // edits intact (mirrors the full-page `create` error path).
            let inlines =
                crate::inlines::build_inline_views_from_submitted(&model, cfg, &multi_form);
            // Per-field errors: merge `WriteError::field_errors()` into each
            // field's `.error` slot (exactly like the full-page `create` path),
            // so a validation failure highlights the OFFENDING field instead of
            // only showing a flattened top banner. Unmatched / non-field errors
            // fall to the banner; a unique violation is attributed to its column
            // when parseable; an inline-child `BadInput` keeps its row-level
            // diagnostic verbatim in the banner.
            let error = match &e {
                AdminError::BadInput(msg) => msg.clone(),
                AdminError::Write(we) => {
                    sanitise_form_error(&e); // fires the tracing::error! log
                    apply_write_error_to_fields(we, &mut fields)
                }
                AdminError::Sqlx(sqlx_err) => {
                    match parse_unique_violation_column(&sqlx_err.to_string())
                        .and_then(|col| fields.iter_mut().find(|f| f.name == col).map(|f| (col, f)))
                    {
                        Some((col, f)) => {
                            f.error = format!("A record with this `{col}` already exists.");
                            String::new()
                        }
                        None => sanitise_form_error(&e),
                    }
                }
                _ => sanitise_form_error(&e),
            };
            let model_view = model_for_template(&model);
            match render(
                "admin/sheet_create.html",
                context!(
                    model       => model_view,
                    instance_id => "",
                    fields      => fields,
                    m2m_fields  => m2m_fields,
                    inlines     => inlines,
                    error       => error,
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
    let path = format!(
        "{}/{table}/{id}/change-password",
        crate::branding::current().base_path
    );
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
    let hash = match umbra_auth::hash_password_async(new_pw).await {
        Ok(h) => h,
        Err(e) => {
            return AdminError::BadInput(format!("password hashing failed: {e}")).into_response();
        }
    };
    let Some((plugin_name, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    if let Err(r) = crate::permcheck::require(
        &actor,
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
    if let Err(e) = DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .update_one(pw_col, &hash)
        .await
    {
        // gaps2 #12: `update_one` returns `DynError`; route via
        // `From<DynError>` so the per-field WriteError survives.
        return AdminError::from(e).into_response();
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
