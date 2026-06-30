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
use crate::view::sidebar_apps;

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

    // Same widget-section JSON shape the dashboard handler emits
    // (`handlers/list.rs::index`), so `widget_grid` renders identically.
    let widget_sections: Vec<serde_json::Value> = view
        .sections()
        .iter()
        .map(|section| {
            let widgets_json: Vec<serde_json::Value> = section
                .widgets
                .iter()
                .map(|w| {
                    serde_json::json!({
                        "key":   w.key,
                        "title": w.title,
                        "kind":  w.kind.as_str(),
                        "span":  { "cols": w.default_span.cols, "rows": w.default_span.rows },
                    })
                })
                .collect();
            serde_json::json!({
                "title":    section.title,
                "subtitle": section.subtitle,
                "widgets":  widgets_json,
            })
        })
        .collect();

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
            view_groups => Vec::<serde_json::Value>::new(), // populated in Task 6
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
