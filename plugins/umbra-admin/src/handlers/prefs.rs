//! `GET / PUT /admin/api/prefs` — the per-user admin preferences row.

use umbra::web::{HeaderMap, IntoResponse, Json, Response, StatusCode};

use crate::auth::require_staff;
use crate::models;

/// `GET /admin/api/prefs` — return the current user's prefs row,
/// creating defaults on first access (the default is returned but
/// not inserted; the next `PUT` performs the write).
pub(crate) async fn get_prefs_handler(headers: HeaderMap) -> Response {
    let user = match require_staff(&headers, "/admin/api/prefs").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    match models::fetch_or_default(user.id).await {
        Ok(prefs) => Json(serde_json::json!({
            "theme": prefs.theme,
            "density": prefs.density,
            "sidebar_collapsed": prefs.sidebar_collapsed,
            "dashboard_layout": prefs.dashboard_layout,
            "updated_at": prefs.updated_at,
        }))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "admin: get_prefs failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "prefs error").into_response()
        }
    }
}

/// `PUT /admin/api/prefs` — update the current user's prefs.
///
/// Body: `application/json` with any subset of
/// `{theme, density, sidebar_collapsed, dashboard_layout}`. Invalid
/// values for the constrained fields are silently ignored so a stale
/// client can't flip the row into an invalid state.
pub(crate) async fn put_prefs_handler(headers: HeaderMap, body: String) -> Response {
    let user = match require_staff(&headers, "/admin/api/prefs").await {
        Ok(u) => u,
        Err(r) => return r,
    };

    let mut prefs = match models::fetch_or_default(user.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "admin: put_prefs fetch failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "prefs error").into_response();
        }
    };

    if let Ok(patch) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(t) = patch.get("theme").and_then(|v| v.as_str()) {
            if matches!(t, "light" | "dark" | "system") {
                prefs.theme = t.to_string();
            }
        }
        if let Some(d) = patch.get("density").and_then(|v| v.as_str()) {
            if matches!(d, "comfortable" | "compact") {
                prefs.density = d.to_string();
            }
        }
        if let Some(sc) = patch.get("sidebar_collapsed").and_then(|v| v.as_bool()) {
            prefs.sidebar_collapsed = sc;
        }
        if let Some(layout) = patch.get("dashboard_layout").and_then(|v| v.as_str()) {
            prefs.dashboard_layout = layout.to_string();
        }
    }

    match models::upsert(prefs).await {
        Ok(_) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "admin: put_prefs upsert failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "prefs save error").into_response()
        }
    }
}
