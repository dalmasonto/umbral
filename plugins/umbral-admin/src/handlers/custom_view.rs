//! Custom-view page handler. Renders a developer-registered widget page
//! inside the admin chrome. See `src/views.rs` + the design spec.

use axum::extract::State;
use axum::http::HeaderMap;
use minijinja::context;
use umbral::web::{IntoResponse, Response, StatusCode};

use crate::AdminState;
use crate::auth::require_staff;
use crate::engine::render;
use crate::permcheck;
use crate::view::{sidebar_apps, view_groups};

pub(crate) async fn custom_view(
    State(state): State<AdminState>,
    headers: HeaderMap,
    slug: String,
) -> Response {
    let current_path = format!("{}/{}", crate::branding::current().base_path, slug);
    let user = match require_staff(&headers, &current_path).await {
        Ok(u) => u,
        Err(r) => return r,
    };

    let view = match state.custom_views.iter().find(|v| v.slug() == slug) {
        Some(v) => v,
        None => return (StatusCode::NOT_FOUND, "umbral-admin: unknown view").into_response(),
    };

    if let Some(code) = view.permission() {
        if let Err(r) = permcheck::require_codename(&user, code).await {
            return r;
        }
    }

    let apps = sidebar_apps(&state, &user).await;
    let view_groups = view_groups(&state, &user).await;

    // Permission-filtered widget-section JSON — same shape the dashboard
    // handler emits so `widget_grid` renders identically. Widgets whose
    // Widget::permission codename the viewing user lacks are omitted.
    // Falls back to all widgets when PermissionsPlugin is absent.
    let widget_sections =
        crate::view::accessible_widget_sections_json(view.sections(), &user).await;

    let breadcrumbs = vec![serde_json::json!({ "label": view.title(), "url": "" })];

    let initial_theme = crate::discovery::user_theme(&user).await;

    match render(
        "admin/custom_view.html",
        context! {
            user => user.username.clone(),
            page_title => view.title(),
            page_subtitle => view.subtitle(),
            widget_sections => widget_sections,
            apps => apps,
            view_groups => view_groups,
            active_view => slug,
            active_table => "",
            breadcrumbs => breadcrumbs,
            initial_theme => initial_theme,
        },
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}
