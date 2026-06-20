//! `GET /admin/{table}/{id}/history` — audit timeline for one object.

use minijinja::context;
use umbra::web::{HeaderMap, IntoResponse, Path, Response, StatusCode};

use crate::AdminState;
use crate::auth::require_staff;
use crate::discovery::{find_model, user_theme};
use crate::engine::render;
use crate::error::AdminError;
use crate::models;
use crate::view::sidebar_apps;
use axum::extract::State;

/// `GET /admin/{table}/{id}/history` — render the timeline page with
/// the 50 most recent audit entries for `(table, id)`.
pub(crate) async fn history_handler(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!(
        "{}/{table}/{id}/history",
        crate::branding::current().base_path
    );
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let object_id: i64 = match id.parse() {
        Ok(v) => v,
        Err(_) => return AdminError::BadInput(format!("invalid id: {id}")).into_response(),
    };
    let entries = match models::audit_for_object(&table, object_id, 50).await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "admin: audit_for_object failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "audit error").into_response();
        }
    };

    let apps = sidebar_apps(&state, &user).await;
    let initial_theme = user_theme(&user).await;
    match render(
        "admin/history.html",
        context!(
            model_name    => model.name.clone(),
            object_id     => object_id,
            entries       => entries,
            apps          => apps,
            active_table  => table,
            breadcrumbs   => Vec::<serde_json::Value>::new(),
            initial_theme => initial_theme,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}
