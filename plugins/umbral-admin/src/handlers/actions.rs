//! Per-model bulk-action handlers — both the legacy form-POST path
//! (`run_action`) and the HTMX-friendly per-key dispatch
//! (`dispatch_action`). The per-model `Action` set comes from the
//! developer's `AdminModel::actions(...)` config; this module just
//! resolves the right `Action` and invokes its handler.

use std::sync::Arc;

use axum::extract::{Path, State};
use umbral::web::{HeaderMap, IntoResponse, Redirect, Response, StatusCode};

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
    let path = format!("{}/{table}/action", crate::branding::current().base_path);
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    // WEB-7: same model-level gate as `dispatch_action` — the legacy
    // form-POST path runs the same mutating bulk-action handlers.
    let Some((plugin_name, _model)) = crate::discovery::find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::Change)
            .await
    {
        return r;
    }
    let pairs: Vec<(String, String)> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(e) => return AdminError::BadInput(e.to_string()).into_response(),
    };
    let action_key = pairs
        .iter()
        .find(|(k, _)| k == "action")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let selected_ids: Vec<String> = pairs
        .iter()
        .filter(|(k, _)| k.as_str() == "selected")
        .map(|(_, v)| v.clone())
        .collect();

    let cfg = state.config_for(&table);
    // gaps2 #35: a soft-delete model resolves the built-in trash actions
    // (`restore_selected`, `delete_permanently`) in addition to whatever
    // the developer configured — they're injected into the changelist's
    // trash view but dispatched through this same endpoint.
    let actions = resolve_actions(cfg, &table);
    let action = actions.iter().find(|a| a.key() == action_key);
    let Some(action) = action else {
        return AdminError::NotFound(format!("no action `{action_key}` for table `{table}`"))
            .into_response();
    };

    // gaps2 #79: enforce Action::permission before running the handler.
    // Superusers bypass the check; non-superusers need the exact codename.
    if let Some(ref required_perm) = action.permission {
        if let Err(r) = check_action_perm(&who, required_perm).await {
            return r;
        }
    }
    // gaps2 #35: the built-in "Delete permanently" is a hard delete, so
    // it needs the stronger `delete_<model>` permission — the broad
    // Change gate above isn't enough for an irreversible removal.
    if action.key() == "delete_permanently" {
        if let Err(r) =
            crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::Delete)
                .await
        {
            return r;
        }
    }

    let inv = ActionInvocation {
        ids: selected_ids.clone(),
        username: who.username.clone(),
        table: table.clone(),
        pool: umbral::db::pool_dispatched().clone(),
    };
    let handler = Arc::clone(&action.handler);
    let result = handler(inv).await;
    // Audit log — one entry per bulk-action submission.
    let summary = match &result {
        Ok(_) => format!(
            "ran action `{}` on {} #{:?} (via form)",
            action_key, table, selected_ids
        ),
        Err(e) => format!("action `{action_key}` on {table} failed: {e}"),
    };
    crate::models::log(
        who.id,
        &format!("action:{action_key}"),
        &table,
        selected_ids.first().and_then(|s| s.parse::<i64>().ok()),
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
    let location = format!(
        "{}/{table}/?flash={}",
        crate::branding::current().base_path,
        urlencoding_simple(&flash)
    );
    Redirect::to(&location).into_response()
}

/// Serialise an action slice for the template's "Actions" menu — the
/// bulk-action toolbar and the per-row chip strip. The changelist passes
/// the effective set (config + soft-delete trash built-ins) computed via
/// [`crate::config::effective_actions`].
pub(crate) fn descriptors_for(actions: &[crate::config::Action]) -> Vec<serde_json::Value> {
    actions
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
    let path = format!(
        "{}/{table}/actions/{key}",
        crate::branding::current().base_path
    );
    let who = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };

    // WEB-7: bulk actions run developer-defined handlers that can mutate
    // or delete rows, so they need the same model-level permission gate as
    // the CRUD handlers — `require_staff` alone lets any staff user fire
    // them regardless of `change_<model>`. Gate on Change (the broadest
    // thing an action can do); a no-permissions install (no
    // umbral-permissions) still passes, matching the rest of the admin.
    let Some((plugin_name, _model)) = crate::discovery::find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    if let Err(r) =
        crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::Change)
            .await
    {
        return r;
    }

    let ids: Vec<String> = if body.trim_start().starts_with('{') {
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => v["ids"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| {
                            // Accept both JSON numbers and JSON strings so callers
                            // can send either `{"ids":[1,2]}` or `{"ids":["a","b"]}`.
                            x.as_str()
                                .map(|s| s.to_string())
                                .or_else(|| x.as_i64().map(|n| n.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Err(e) => return AdminError::BadInput(format!("bad JSON: {e}")).into_response(),
        }
    } else {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_str(&body).unwrap_or_default();
        pairs
            .into_iter()
            .filter(|(k, _)| k.as_str() == "ids" || k.as_str() == "selected")
            .map(|(_, v)| v)
            .collect()
    };

    let cfg = state.config_for(&table);
    // gaps2 #35: include the soft-delete trash built-ins (see `run_action`).
    let actions = resolve_actions(cfg, &table);
    let action = actions.iter().find(|a| a.key() == key);
    let Some(action) = action else {
        return AdminError::NotFound(format!("no action `{key}` for `{table}`")).into_response();
    };

    // gaps2 #79: enforce Action::permission before running the handler.
    // Superusers bypass the check; non-superusers need the exact codename.
    if let Some(ref required_perm) = action.permission {
        if let Err(r) = crate::handlers::actions::check_action_perm(&who, required_perm).await {
            return r;
        }
    }
    // gaps2 #35: the built-in "Delete permanently" hard-deletes, so it
    // needs `delete_<model>` — stronger than the broad Change gate above.
    if action.key() == "delete_permanently" {
        if let Err(r) =
            crate::permcheck::require(&who, &plugin_name, &table, crate::permcheck::Action::Delete)
                .await
        {
            return r;
        }
    }

    let inv = ActionInvocation {
        ids: ids.clone(),
        username: who.username.clone(),
        table: table.clone(),
        pool: umbral::db::pool_dispatched().clone(),
    };
    let handler = Arc::clone(&action.handler);
    let result = handler(inv).await;
    // Audit log — one entry per dispatched action regardless of
    // outcome variant, so the timeline shows what was invoked and
    // by whom even when the action returns a Download or Redirect.
    let summary = match &result {
        Ok(_) => format!(
            "ran action `{}` on {} #{:?} (via dispatch)",
            key, table, ids
        ),
        Err(e) => format!("action `{key}` on {table} failed: {e}"),
    };
    crate::models::log(
        who.id,
        &format!("action:{key}"),
        &table,
        ids.first().and_then(|s| s.parse::<i64>().ok()),
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

/// Resolve the dispatchable action set for `table` (gaps2 #35).
///
/// The developer's configured actions, plus — for a `soft_delete`
/// model — the built-in trash actions (`restore_selected`,
/// `delete_permanently`). The built-ins are appended only when not
/// already present so a developer who lists them explicitly doesn't get
/// duplicates. A non-soft-delete model returns its configured set
/// verbatim.
pub(crate) fn resolve_actions(
    cfg: Option<&AdminConfig>,
    table: &str,
) -> Vec<crate::config::Action> {
    let mut actions: Vec<crate::config::Action> =
        cfg.map(|c| c.actions.clone()).unwrap_or_default();
    let soft_delete = crate::discovery::find_model(table)
        .map(|(_, meta)| meta.soft_delete)
        .unwrap_or(false);
    if soft_delete {
        for builtin in [
            crate::config::Action::restore_selected(),
            crate::config::Action::delete_permanently(),
        ] {
            if !actions.iter().any(|a| a.key() == builtin.key()) {
                actions.push(builtin);
            }
        }
    }
    actions
}

/// Check that `who` holds the required action permission codename.
///
/// Mirrors `permcheck::check` but operates on a raw codename string
/// (as stored in `Action::permission`) rather than deriving one from
/// (plugin, table, verb).  Superusers always pass; the check is a
/// no-op when `umbral-permissions` is not installed.
pub(crate) async fn check_action_perm(
    who: &umbral_auth::AuthUser,
    required_perm: &str,
) -> Result<(), Response> {
    // No-op when the permissions plugin isn't installed (matches the
    // rest of the admin's graceful-fallback behaviour from permcheck.rs).
    if !crate::permcheck::permissions_installed() {
        return Ok(());
    }
    let user_id = who.id.to_string();
    let allowed =
        umbral_permissions::has_perm_for_superuser(&user_id, who.is_superuser, required_perm)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(
                    user_id = user_id.as_str(),
                    perm = required_perm,
                    error = %err,
                    "action permission check failed; denying by default"
                );
                false
            });
    if allowed {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "umbral-admin: permission denied for this action",
        )
            .into_response())
    }
}
