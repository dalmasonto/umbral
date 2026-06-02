//! Per-model bulk-action handlers — both the legacy form-POST path
//! (`run_action`) and the HTMX-friendly per-key dispatch
//! (`dispatch_action`). The per-model `Action` set comes from the
//! developer's `AdminModel::actions(...)` config; this module just
//! resolves the right `Action` and invokes its handler.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use umbra::web::{HeaderMap, IntoResponse, Redirect, Response, StatusCode};

use crate::AdminState;
use crate::auth::require_staff;
use crate::config::{ActionInvocation, ActionResult, ActionScope, ActionVariant, AdminConfig};
use crate::error::AdminError;
use crate::util::urlencoding_simple;

/// `POST /admin/{table}/action` — legacy form-POST entry point. The
/// changelist's bulk action `<form>` posts here; the response is a
/// Redirect with a `?flash=...` toast.
pub(crate) async fn run_action(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/action");
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(e) => return AdminError::BadInput(e.to_string()).into_response(),
    };
    let action_key = form.get("action").cloned().unwrap_or_default();
    let selected_ids: Vec<i64> = form
        .iter()
        .filter(|(k, _)| k.as_str() == "selected")
        .filter_map(|(_, v)| v.parse::<i64>().ok())
        .collect();

    let cfg = state.config_for(&table);
    let action = cfg.and_then(|c| c.actions.iter().find(|a| a.key == action_key));
    let Some(action) = action else {
        return AdminError::NotFound(format!("no action `{action_key}` for table `{table}`"))
            .into_response();
    };

    let inv = ActionInvocation {
        ids: selected_ids.clone(),
        username: who.username.clone(),
        table: table.clone(),
        pool: umbra::db::pool().clone(),
    };
    let handler = Arc::clone(&action.handler);
    let result = handler(inv).await;
    // Audit log — one entry per bulk-action submission.
    let summary = match &result {
        Ok(_) => format!(
            "ran action `{}` on {} #{:?} (via form)",
            action_key,
            table,
            selected_ids
        ),
        Err(e) => format!("action `{action_key}` on {table} failed: {e}"),
    };
    crate::models::log(
        who.id,
        &format!("action:{action_key}"),
        &table,
        selected_ids.first().copied(),
        &summary,
    )
    .await;
    let flash = match result {
        Ok(ActionResult::Toast { message, .. }) => message,
        Ok(ActionResult::RefreshTable) => "Done.".to_string(),
        Ok(_) => "Done.".to_string(),
        Err(e) => {
            tracing::error!(error = %e, "admin: action `{action_key}` failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    };
    let location = format!("/admin/{table}/?flash={}", urlencoding_simple(&flash));
    Redirect::to(&location).into_response()
}

/// Serialise the model's actions for the template's "Actions" menu.
/// The template renders these into the bulk-action dropdown and the
/// per-row chip strip.
pub(crate) fn action_descriptors_json(cfg: &AdminConfig) -> Vec<serde_json::Value> {
    cfg.actions
        .iter()
        .map(|a| {
            serde_json::json!({
                "key":     a.key,
                "label":   a.label,
                "icon":    a.icon,
                "variant": match a.variant { ActionVariant::Danger => "danger", _ => "default" },
                "scope":   match a.scope { ActionScope::Row => "row", ActionScope::Bulk => "bulk", ActionScope::Both => "both" },
                "confirm": a.confirm,
            })
        })
        .collect()
}

/// `POST /admin/{table}/actions/{key}` — HTMX-friendly per-key action
/// dispatch. Body can be either JSON `{"ids":[...]}` or form-encoded
/// `ids=&ids=`. The response encodes the `ActionResult` variant as an
/// `HX-Trigger` header so the front-end can react without a full page
/// reload (toast, refresh table, open sheet, download, redirect).
pub(crate) async fn dispatch_action(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, key)): Path<(String, String)>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/actions/{key}");
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };

    let ids: Vec<i64> = if body.trim_start().starts_with('{') {
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => v["ids"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect())
                .unwrap_or_default(),
            Err(e) => return AdminError::BadInput(format!("bad JSON: {e}")).into_response(),
        }
    } else {
        let form: HashMap<String, String> = serde_urlencoded::from_str(&body).unwrap_or_default();
        form.iter()
            .filter(|(k, _)| k.as_str() == "ids" || k.as_str() == "selected")
            .filter_map(|(_, v)| v.parse::<i64>().ok())
            .collect()
    };

    let cfg = state.config_for(&table);
    let action = cfg.and_then(|c| c.actions.iter().find(|a| a.key == key));
    let Some(action) = action else {
        return AdminError::NotFound(format!("no action `{key}` for `{table}`")).into_response();
    };

    let inv = ActionInvocation {
        ids: ids.clone(),
        username: who.username.clone(),
        table: table.clone(),
        pool: umbra::db::pool().clone(),
    };
    let handler = Arc::clone(&action.handler);
    let result = handler(inv).await;
    // Audit log — one entry per dispatched action regardless of
    // outcome variant, so the timeline shows what was invoked and
    // by whom even when the action returns a Download or Redirect.
    let summary = match &result {
        Ok(_) => format!(
            "ran action `{}` on {} #{:?} (via dispatch)",
            key,
            table,
            ids
        ),
        Err(e) => format!("action `{key}` on {table} failed: {e}"),
    };
    crate::models::log(
        who.id,
        &format!("action:{key}"),
        &table,
        ids.first().copied(),
        &summary,
    )
    .await;
    match result {
        Ok(ActionResult::Toast { message, level }) => {
            let trigger = serde_json::json!({
                "showToast": { "message": message, "level": level.as_str() }
            });
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("HX-Trigger", trigger.to_string())
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| StatusCode::OK.into_response())
        }
        Ok(ActionResult::RefreshTable) => {
            let trigger = serde_json::json!({ "refreshTable": {} });
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("HX-Trigger", trigger.to_string())
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| StatusCode::OK.into_response())
        }
        Ok(ActionResult::OpenSheet { table: t, id }) => {
            let trigger = serde_json::json!({ "openSheet": { "table": t, "id": id } });
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("HX-Trigger", trigger.to_string())
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| StatusCode::OK.into_response())
        }
        Ok(ActionResult::Download {
            filename,
            content_type,
            bytes,
        }) => axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", content_type)
            .header(
                "Content-Disposition",
                format!("attachment; filename=\"{filename}\""),
            )
            .body(axum::body::Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::OK.into_response()),
        Ok(ActionResult::Redirect { url }) => axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("HX-Redirect", url)
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| StatusCode::OK.into_response()),
        Err(e) => {
            tracing::error!(error = %e, "admin: action `{key}` failed");
            let trigger = serde_json::json!({
                "showToast": { "message": e, "level": "error" }
            });
            axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("HX-Trigger", trigger.to_string())
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
