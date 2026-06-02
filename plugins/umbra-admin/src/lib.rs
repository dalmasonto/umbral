//! umbra-admin — auto-generated CRUD admin for umbra models.
//!
//! Drop-in admin interface for any umbra project. Register the
//! [`AdminPlugin`] on `App::builder()` and every model the
//! migration registry knows about gets:
//!
//! - A list view at `/admin/<table>/` with all rows in a table
//! - A detail view at `/admin/<table>/<id>` with every field
//! - A create form at `/admin/<table>/new`
//! - An edit form at `/admin/<table>/<id>/edit`
//! - A delete action at `POST /admin/<table>/<id>/delete`
//!
//! Plus a registered-models index at `/admin/`.
//!
//! ## Customizing per-model display
//!
//! Register an [`AdminModel`] for a model to control list columns, filter
//! facets, search, ordering, bulk actions, and readonly fields. See
//! [`AdminPlugin::register`] and the [`config`] module.
//!
//! ## Auth
//!
//! Every admin route requires a session-backed staff user. If the
//! session is missing or the user is not staff, the handler redirects
//! to `GET /admin/login?next=<current-url>`. `POST /admin/login` verifies
//! credentials via [`umbra_auth::authenticate`], creates a session via
//! [`umbra_sessions::login`], then redirects to `next`.
//!
//! ## Templates
//!
//! Six `include_str!`-embedded Jinja templates live in `templates/`.
//! The admin owns its own minijinja `Environment`. `admin/base.html`
//! is the shell (sidebar + topbar + content slot); the other five
//! extend it.
//!
//! ## Form widgets
//!
//! Inputs dispatch per [`SqlType`]:
//!
//! | SqlType | Input |
//! |---|---|
//! | `SmallInt`, `Integer`, `BigInt` | `<input type="number">` |
//! | `Real`, `Double` | `<input type="number" step="any">` |
//! | `Boolean` | `<input type="checkbox">` |
//! | `Text`, `Uuid` | `<input type="text">` |
//! | `Date` | `<input type="date">` |
//! | `Time` | `<input type="time">` |
//! | `Timestamptz` | `<input type="datetime-local">` |
//!
//! Nullable fields skip the `required` attribute.

pub mod config;
pub mod models;
pub mod registry;
pub mod widgets;

mod auth;
mod discovery;
mod engine;
mod error;
mod pagination;
mod static_assets;
mod util;
mod view;

pub mod files;

pub(crate) use auth::{login_get, login_post, logout_handler, require_staff};
pub(crate) use discovery::{discover_models, find_model, pk_column, user_theme};
pub(crate) use engine::render;
pub(crate) use error::AdminError;
pub use files::{file_descriptor, resolve_preview_kind};
pub(crate) use pagination::{Pagination, build_order_clause_phase2, parse_list_params};
pub(crate) use static_assets::serve_admin_css;
pub(crate) use util::{html_escape, is_htmx, q, urlencoding_simple};
pub(crate) use view::{
    form_fields_for, input_kind, model_for_template, model_for_template_cols, sidebar_apps,
};


pub use config::{
    Action, ActionInvocation, ActionResult, ActionScope, ActionVariant, AdminConfig, AdminContext,
    AdminModel, InlineModel, ToastLevel,
};
pub use registry::{AdminRegistration, AdminRegistry, App as AdminApp};
pub use widgets::{
    BarPayload, CatalogEntry, FeedItem, FeedPayload, KpiPayload, LinePayload, Series, Span,
    TableColumn, TablePayload, Widget, WidgetDataFn, WidgetInstance, WidgetKind, WidgetPayload,
};

use std::collections::HashMap;
use std::sync::Arc;

// Action types are re-exported via `pub use config::...` above; use them directly.

use axum::extract::{Query, State};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use minijinja::context;
use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, SqlitePool};
use umbra::migrate::{Column, ModelMeta};
use umbra::orm::SqlType;
use umbra::prelude::*;
use umbra::web::{HeaderMap, IntoResponse, Json, Path, Redirect, Response, StatusCode, post};
use uuid::Uuid;

// =========================================================================
// Plugin struct
// =========================================================================

/// The plugin. Mounts every admin route under `/admin`.
///
/// Use [`AdminPlugin::register`] to attach an [`AdminModel`] before
/// passing the plugin to `App::builder().plugin(...)`.
///
/// ```ignore
/// use umbra_admin::{AdminPlugin, AdminModel, Action};
///
/// let admin = AdminPlugin::default()
///     .register(
///         AdminModel::new("post")
///             .list_display(&["title", "author", "published_at"])
///             .list_filter(&["published"])
///             .search_fields(&["title", "body"])
///             .ordering(&["-published_at"])
///             .readonly_fields(&["created_at"])
///             .actions(vec![Action::delete_selected()]),
///     );
///
/// App::builder()
///     .plugin(AuthPlugin::default())
///     .plugin(admin)
///     .build()?;
/// ```
#[derive(Debug, Default, Clone)]
pub struct AdminPlugin {
    registry: AdminRegistry,
    widget_catalog: Vec<Widget>,
}

impl AdminPlugin {
    /// Register an [`AdminModel`] for one model. Chainable.
    ///
    /// If two configs are registered for the same table the last one wins
    /// (same semantics as Django's `site.register` overwriting on duplicate).
    ///
    /// The plugin name defaults to `"admin"` for models registered before
    /// the plugin is installed into the app. From M7+ plugins will pass
    /// their own name via `Plugin::admin_register` on the registry.
    pub fn register(mut self, model: AdminModel) -> Self {
        self.registry.register("admin", model);
        self
    }

    /// Register an [`AdminModel`] for a specific plugin name.
    ///
    /// This is the method the `Plugin::routes` / `on_ready` pathway uses
    /// when a plugin contributes its own admin registrations. The sidebar
    /// groups models by the `plugin_name` supplied here.
    pub fn register_for(mut self, plugin_name: &str, model: AdminModel) -> Self {
        self.registry.register(plugin_name, model);
        self
    }

    /// Register a dashboard widget. Chainable.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use umbra_admin::{AdminPlugin, Widget, WidgetKind, WidgetDataFn, WidgetPayload, KpiPayload, Span};
    ///
    /// AdminPlugin::default()
    ///     .register_widget(Widget {
    ///         key:          "total_posts",
    ///         title:        "Total Posts".to_string(),
    ///         kind:         WidgetKind::Kpi,
    ///         default_span: Span { cols: 3, rows: 1 },
    ///         permission:   None,
    ///         data:         WidgetDataFn::new(|_user| async move {
    ///             WidgetPayload::Kpi(KpiPayload {
    ///                 value: "0".to_string(),
    ///                 unit: None, delta: None, sparkline: None,
    ///             })
    ///         }),
    ///     });
    /// ```
    pub fn register_widget(mut self, widget: Widget) -> Self {
        self.widget_catalog.push(widget);
        self
    }
}

/// Shared state injected into every route via [`axum::extract::State`].
///
/// `Arc` makes the clone cheap; the registry is immutable after `build()`.
#[derive(Clone, Debug)]
struct AdminState {
    registry: Arc<AdminRegistry>,
    /// Dashboard widget catalog — registered at plugin-build time.
    widget_catalog: Arc<Vec<Widget>>,
}

impl AdminState {
    fn config_for(&self, table: &str) -> Option<&AdminConfig> {
        self.registry.get(table).map(|r| &r.model)
    }
}

impl Plugin for AdminPlugin {
    fn name(&self) -> &'static str {
        "admin"
    }

    fn dependencies(&self) -> &'static [&'static str] {
        // Auth is required: login verifies credentials via umbra-auth.
        // Sessions is required: login creates sessions.
        &["auth", "sessions"]
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![
            umbra::migrate::ModelMeta::for_::<crate::models::AdminUserPref>(),
            umbra::migrate::ModelMeta::for_::<crate::models::AdminAuditLog>(),
        ]
    }

    fn routes(&self) -> Router {
        // Seed the catalog with the two built-in widgets, then append
        // developer-registered ones.
        let mut catalog = vec![builtin_total_models_widget(), builtin_recent_users_widget()];
        catalog.extend(self.widget_catalog.iter().cloned());

        let state = AdminState {
            registry: Arc::new(self.registry.clone()),
            widget_catalog: Arc::new(catalog),
        };
        Router::new()
            // Login / logout (no auth required)
            .route(
                "/admin/login",
                axum::routing::get(login_get).post(login_post),
            )
            .route("/admin/logout", axum::routing::get(logout_handler))
            // Index + CRUD routes (all require staff session)
            .route("/admin", axum::routing::get(index))
            .route("/admin/", axum::routing::get(index))
            .route("/admin/{table}/", axum::routing::get(list))
            .route(
                "/admin/{table}/new",
                axum::routing::get(new_form).post(create),
            )
            .route("/admin/{table}/action", post(run_action))
            // Phase 2: fragment-only rows endpoint (search/sort/filter/paginate)
            .route("/admin/{table}/rows", axum::routing::get(rows_fragment))
            // Filter dialog fragment
            .route(
                "/admin/{table}/filter-dialog",
                axum::routing::get(filter_dialog_handler),
            )
            // Phase 2: new-record sheet (create mode)
            .route("/admin/{table}/new-sheet", axum::routing::get(new_sheet))
            // Phase 2: delete confirm dialog fragment
            .route(
                "/admin/{table}/{id}/_confirm-delete",
                axum::routing::get(confirm_delete_dialog),
            )
            // Phase 2: sheet fragments (preview + edit)
            .route(
                "/admin/{table}/{id}/sheet",
                axum::routing::get(preview_sheet),
            )
            .route(
                "/admin/{table}/{id}/edit-sheet",
                axum::routing::get(edit_sheet_handler),
            )
            .route("/admin/{table}/{id}", axum::routing::get(detail))
            .route(
                "/admin/{table}/{id}/edit",
                axum::routing::get(edit_form).post(update),
            )
            // Phase 2: create via sheet (POST)
            .route("/admin/{table}/create", axum::routing::post(sheet_create))
            // Phase 2: DELETE method for HTMX delete button
            .route("/admin/{table}/{id}", axum::routing::delete(htmx_delete))
            .route("/admin/{table}/{id}/delete", post(delete))
            // Phase 3: per-key action dispatch
            .route(
                "/admin/{table}/actions/{key}",
                axum::routing::post(dispatch_action),
            )
            // Phase 3: FK/M2M async picker endpoints
            .route(
                "/admin/api/{table}/{field}/options/resolve",
                axum::routing::get(fk_options_resolve),
            )
            .route(
                "/admin/api/{table}/{field}/options",
                axum::routing::get(fk_options),
            )
            // Phase 3: inline cell edit
            .route(
                "/admin/{table}/{id}/cell/{field}/edit",
                axum::routing::get(cell_edit_get),
            )
            .route(
                "/admin/{table}/{id}/cell/{field}",
                axum::routing::post(cell_edit_post),
            )
            // Password change for models with password_field set
            .route(
                "/admin/{table}/{id}/change-password",
                axum::routing::post(change_password_handler),
            )
            // Phase 4: user prefs
            .route(
                "/admin/api/prefs",
                axum::routing::get(get_prefs_handler).put(put_prefs_handler),
            )
            // Phase 4: audit history
            .route(
                "/admin/{table}/{id}/history",
                axum::routing::get(history_handler),
            )
            // Phase 4: dashboard
            .route(
                "/admin/api/dashboard/catalog",
                axum::routing::get(dashboard_catalog),
            )
            .route(
                "/admin/api/dashboard/layout",
                axum::routing::get(dashboard_layout_get).put(dashboard_layout_put),
            )
            .route(
                "/admin/api/dashboard/widgets/{key}/data",
                axum::routing::get(dashboard_widget_data),
            )
            // Phase 4: command palette fragment + global record search
            .route("/admin/api/palette", axum::routing::get(palette_fragment))
            .route(
                "/admin/api/palette/search",
                axum::routing::get(palette_search),
            )
            // Static CSS (embedded at compile time; served in prod, CDN used in dev)
            .route(
                "/admin/static/admin.css",
                axum::routing::get(serve_admin_css),
            )
            .with_state(state)
    }

    fn on_ready(&self, _ctx: &umbra::plugin::AppContext) -> Result<(), umbra::plugin::PluginError> {
        // Tables are produced by the migration engine off
        // `Self::models()` — same path as every other plugin's models.
        // No bootstrap DDL here.
        Ok(())
    }
}

// =========================================================================
// Sidebar context helpers.
//
// Every handler that renders the authenticated shell calls `sidebar_apps`
// to pass the nav tree into the template.
// =========================================================================



// =========================================================================
// Handlers.
// =========================================================================

async fn index(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let user = match require_staff(&headers, "/admin/").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let apps = sidebar_apps(&state, &user);

    // Build the widget list for the dashboard from the user's layout
    // (or default = all widgets in catalog order).
    let catalog = state.widget_catalog.as_ref();
    let widgets: Vec<serde_json::Value> = catalog
        .iter()
        .map(|w| {
            serde_json::json!({
                "key":  w.key,
                "title": w.title,
                "kind": w.kind.as_str(),
                "span": {
                    "cols": w.default_span.cols,
                    "rows": w.default_span.rows,
                },
            })
        })
        .collect();

    // Build model cards: one card per registered model with a row count.
    let pool = umbra::db::pool();
    let model_cards: Vec<serde_json::Value> = {
        let mut cards = Vec::new();
        for app in &apps {
            for sidebar_model in &app.models {
                let count: i64 = {
                    let sql = format!(
                        "SELECT COUNT(*) FROM \"{}\"",
                        sidebar_model.table.replace('"', "\"\"")
                    );
                    sqlx::query(&sql)
                        .fetch_one(&pool)
                        .await
                        .and_then(|r| r.try_get::<i64, _>(0))
                        .unwrap_or(0)
                };
                cards.push(serde_json::json!({
                    "table":  sidebar_model.table,
                    "label":  sidebar_model.label,
                    "icon":   if sidebar_model.icon.is_empty() { "database".to_string() } else { sidebar_model.icon.clone() },
                    "count":  count,
                    "url":    format!("/admin/{}/", sidebar_model.table),
                }));
            }
        }
        cards
    };

    let initial_theme = user_theme(&user).await;

    match render(
        "admin/dashboard.html",
        context!(
            user          => user.username.clone(),
            widgets       => widgets,
            model_cards   => model_cards,
            apps          => apps,
            active_table  => "",
            breadcrumbs   => Vec::<serde_json::Value>::new(),
            initial_theme => initial_theme,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

// ---- list ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct FilterFacet {
    field: String,
    values: Vec<String>,
}

async fn list(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/{table}/");
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

    let cfg = state.config_for(&table);

    let display_cols: Vec<String> = if let Some(c) = cfg
        && !c.list_display.is_empty()
    {
        c.list_display.clone()
    } else {
        model.fields.iter().map(|f| f.name.clone()).collect()
    };

    let (search_term, active_filter, sort_col, sort_order, page, page_size) =
        parse_list_params(&params, cfg, pk);

    // Always fetch the pk column even if it's not in list_display.
    let fetch_cols: Vec<String> = {
        let mut cols = display_cols.clone();
        if !cols.contains(&pk.name) {
            cols.push(pk.name.clone());
        }
        cols
    };

    let order_clause = build_order_clause_phase2(cfg, pk, &sort_col, &sort_order);
    let pool = umbra::db::pool();

    let total = match count_rows_filtered(
        &pool,
        &model,
        search_term.as_deref(),
        cfg,
        active_filter
            .as_ref()
            .map(|(f, v)| (f.as_str(), v.as_str())),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let pagination = Pagination::new(total, page, page_size);

    let rows = match fetch_rows_paged(
        &pool,
        &model,
        &fetch_cols,
        &order_clause,
        search_term.as_deref(),
        cfg,
        active_filter
            .as_ref()
            .map(|(f, v)| (f.as_str(), v.as_str())),
        pagination.page_size,
        pagination.offset(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let mut facets: Vec<FilterFacet> = Vec::new();
    if let Some(c) = cfg {
        for field in &c.list_filter {
            let values = fetch_distinct_values(&pool, &model.table, field)
                .await
                .unwrap_or_default();
            facets.push(FilterFacet {
                field: field.clone(),
                values,
            });
        }
    }

    let action_names: Vec<serde_json::Value> = cfg.map(action_descriptors_json).unwrap_or_default();

    let has_search = cfg.is_some_and(|c| !c.search_fields.is_empty());
    let search_val = search_term.unwrap_or_default();
    let active_filter_str = active_filter
        .as_ref()
        .map(|(f, v)| format!("{f}={v}"))
        .unwrap_or_default();
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs =
        vec![serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") })];
    let flash = params.get("flash").cloned().unwrap_or_default();

    // Check if we need to auto-open a sheet (e.g. redirected from preview_sheet)
    let open_row = params.get("row").cloned().unwrap_or_default();

    let columns = model_for_template_cols(&model, &display_cols).fields;

    // Serialize column_widths for template as a JSON object {col_name: css_width}
    // so templates can do direct column_widths[col.name] lookups.
    let column_widths_json: serde_json::Value = cfg
        .map(|c| {
            let mut map = serde_json::Map::new();
            for (col, w) in &c.column_widths {
                map.insert(col.clone(), serde_json::Value::String(w.clone()));
            }
            serde_json::Value::Object(map)
        })
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let inline_edit_fields: Vec<String> = cfg
        .map(|c| c.inline_edit_fields.clone())
        .unwrap_or_default();

    let initial_theme = user_theme(&user).await;

    match render(
        "admin/changelist.html",
        context!(
            user               => user.username.clone(),
            model              => model_for_template_cols(&model, &display_cols),
            rows               => rows,
            columns            => columns,
            pk                 => pk.name.clone(),
            facets             => facets,
            actions            => action_names,
            has_search         => has_search,
            search_val         => search_val,
            active_filter      => active_filter_str,
            pagination         => pagination,
            sort_col           => sort_col,
            sort_order         => sort_order,
            flash              => flash,
            open_row           => open_row,
            apps               => apps,
            active_table       => table,
            breadcrumbs        => breadcrumbs,
            column_widths      => column_widths_json,
            inline_edit_fields => inline_edit_fields,
            initial_theme      => initial_theme,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn fetch_distinct_values(
    pool: &SqlitePool,
    table: &str,
    field: &str,
) -> Result<Vec<String>, AdminError> {
    let sql = format!(
        "SELECT DISTINCT \"{}\" FROM \"{}\" WHERE \"{}\" IS NOT NULL ORDER BY 1 LIMIT 100",
        q(field),
        q(table),
        q(field)
    );
    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    let mut out = Vec::new();
    for row in rows {
        if let Ok(v) = row.try_get::<String, _>(0) {
            out.push(v);
        } else if let Ok(v) = row.try_get::<i64, _>(0) {
            out.push(v.to_string());
        } else if let Ok(v) = row.try_get::<bool, _>(0) {
            out.push(if v { "true" } else { "false" }.to_string());
        }
    }
    Ok(out)
}

// ---- run_action ------------------------------------------------------------

async fn run_action(
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
        ids: selected_ids,
        username: who.username.clone(),
        table: table.clone(),
        pool: umbra::db::pool().clone(),
    };
    let handler = Arc::clone(&action.handler);
    let flash = match handler(inv).await {
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

// =========================================================================
// Phase 3: action_descriptors_json helper
// =========================================================================

fn action_descriptors_json(cfg: &AdminConfig) -> Vec<serde_json::Value> {
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

// =========================================================================
// Phase 3: dispatch_action handler
// =========================================================================

/// `POST /admin/{table}/actions/{key}` — phase 3 action dispatch.
///
/// Body: `application/json` with `{ "ids": [1, 2, 3] }`.
/// Response encoding follows `ActionResult`.
async fn dispatch_action(
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

    // Parse body: try JSON first, fall back to form-encoded.
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
        ids,
        username: who.username.clone(),
        table: table.clone(),
        pool: umbra::db::pool().clone(),
    };
    let handler = Arc::clone(&action.handler);
    match handler(inv).await {
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
            // Signal HTMX to refresh the rows fragment.
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

// =========================================================================
// Phase 3: FK picker endpoints
// =========================================================================

/// `GET /admin/api/{table}/{field}/options?search=&page=&page_size=20`
///
/// Returns paginated label+value options for an FK field.
/// Returns HTML (for HTMX swap) or JSON (for API consumers).
async fn fk_options(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, field)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/api/{table}/{field}/options");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}` on `{table}`")).into_response();
    };
    // Resolve the related table from fk_target or strip _id suffix.
    let related_table = col
        .fk_target
        .clone()
        .unwrap_or_else(|| field.trim_end_matches("_id").to_string());
    let Some((_, related_model)) = find_model(&related_table) else {
        return (
            StatusCode::FORBIDDEN,
            format!("related model `{related_table}` not found or not viewable"),
        )
            .into_response();
    };

    let search = params.get("search").map(|s| s.as_str()).unwrap_or("");
    let page: usize = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let page_size: usize = params
        .get("page_size")
        .and_then(|p| p.parse().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    let offset = (page - 1) * page_size;

    // Pick a label column: first text column that isn't the PK.
    let label_col = related_model
        .fields
        .iter()
        .find(|c| !c.primary_key && matches!(c.ty, umbra::orm::SqlType::Text))
        .map(|c| c.name.as_str())
        .unwrap_or("id");

    // Related model's search_fields from the admin config if registered.
    let rel_cfg = state.config_for(&related_table);
    let search_cols: Vec<String> = rel_cfg
        .filter(|c| !c.search_fields.is_empty())
        .map(|c| c.search_fields.clone())
        .unwrap_or_else(|| vec![label_col.to_string()]);

    let pool = umbra::db::pool();

    // Build WHERE clause for search.
    let mut conditions: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    if !search.is_empty() {
        let like_clauses: Vec<String> = search_cols
            .iter()
            .map(|f| format!("\"{}\" LIKE ?", q(f)))
            .collect();
        if !like_clauses.is_empty() {
            conditions.push(format!("({})", like_clauses.join(" OR ")));
            let like_val = format!("%{search}%");
            for _ in &like_clauses {
                binds.push(like_val.clone());
            }
        }
    }
    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    // Count total for has_more.
    let count_sql = format!("SELECT COUNT(*) FROM \"{}\"{where_sql}", q(&related_table));
    let mut count_qb = sqlx::query(&count_sql);
    for b in &binds {
        count_qb = count_qb.bind(b.clone());
    }
    let total: i64 = match count_qb.fetch_one(&pool).await {
        Ok(r) => r.try_get(0).unwrap_or(0),
        Err(e) => return AdminError::Sqlx(e).into_response(),
    };

    // Fetch page.
    let pk_col = pk_column(&related_model)
        .map(|c| c.name.as_str())
        .unwrap_or("id");
    let select_sql = format!(
        "SELECT \"{pk_col}\", \"{label_col}\" FROM \"{}\"{where_sql} ORDER BY \"{pk_col}\" DESC LIMIT ? OFFSET ?",
        q(&related_table)
    );
    let mut qb = sqlx::query(&select_sql);
    for b in &binds {
        qb = qb.bind(b.clone());
    }
    qb = qb.bind(page_size as i64).bind(offset as i64);

    let rows = match qb.fetch_all(&pool).await {
        Ok(r) => r,
        Err(e) => return AdminError::Sqlx(e).into_response(),
    };

    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let value: i64 = r.try_get(0).unwrap_or(0);
            let label: String = r
                .try_get::<String, _>(1)
                .or_else(|_| r.try_get::<i64, _>(1).map(|v| v.to_string()))
                .unwrap_or_else(|_| format!("#{value}"));
            serde_json::json!({ "value": value, "label": label })
        })
        .collect();

    let has_more = (offset + page_size) < total as usize;

    // HTMX requests get HTML; plain requests get JSON.
    if is_htmx(&headers) {
        let mut html = String::new();
        for item in &items {
            let value = item["value"].as_i64().unwrap_or(0);
            let label = item["label"].as_str().unwrap_or("");
            html.push_str(&format!(
                r#"<button type="button" data-fk-value="{value}" class="w-full text-left px-md py-sm hover:bg-surface-container-high font-body-md text-on-surface transition-colors">{}</button>"#,
                html_escape(label)
            ));
        }
        if html.is_empty() {
            html.push_str(
                r#"<p class="px-md py-sm text-outline text-body-sm italic">No results</p>"#,
            );
        }
        return axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html")
            .body(axum::body::Body::from(html))
            .unwrap_or_else(|_| StatusCode::OK.into_response());
    }

    Json(serde_json::json!({
        "items": items,
        "page": page,
        "has_more": has_more,
    }))
    .into_response()
}

/// `GET /admin/api/{table}/{field}/options/resolve?ids=1,2,3`
///
/// Returns labels for pre-selected ids — used on edit-form load.
async fn fk_options_resolve(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, field)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/api/{table}/{field}/options/resolve");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    };
    let related_table = col
        .fk_target
        .clone()
        .unwrap_or_else(|| field.trim_end_matches("_id").to_string());
    let Some((_, related_model)) = find_model(&related_table) else {
        return (StatusCode::FORBIDDEN, "related model not found").into_response();
    };

    let ids_param = params.get("ids").cloned().unwrap_or_default();
    let ids: Vec<i64> = ids_param
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if ids.is_empty() {
        return Json(serde_json::json!({ "items": [] })).into_response();
    }

    let label_col = related_model
        .fields
        .iter()
        .find(|c| !c.primary_key && matches!(c.ty, umbra::orm::SqlType::Text))
        .map(|c| c.name.as_str())
        .unwrap_or("id");
    let pk_col = pk_column(&related_model)
        .map(|c| c.name.as_str())
        .unwrap_or("id");

    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT \"{pk_col}\", \"{label_col}\" FROM \"{}\" WHERE \"{pk_col}\" IN ({placeholders})",
        q(&related_table)
    );
    let pool = umbra::db::pool();
    let mut qb = sqlx::query(&sql);
    for id in &ids {
        qb = qb.bind(*id);
    }

    // Suppress unused variable warning from state parameter
    let _ = &state;

    match qb.fetch_all(&pool).await {
        Ok(rows) => {
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    let value: i64 = r.try_get(0).unwrap_or(0);
                    let label: String = r
                        .try_get::<String, _>(1)
                        .or_else(|_| r.try_get::<i64, _>(1).map(|v| v.to_string()))
                        .unwrap_or_else(|_| format!("#{value}"));
                    serde_json::json!({ "value": value, "label": label })
                })
                .collect();
            Json(serde_json::json!({ "items": items })).into_response()
        }
        Err(e) => AdminError::Sqlx(e).into_response(),
    }
}

// =========================================================================
// Phase 3: inline cell edit handlers
// =========================================================================

/// `GET /admin/{table}/{id}/cell/{field}/edit`
/// Returns the field editor for a single cell (HTMX swap into the <td>).
async fn cell_edit_get(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id, field)): Path<(String, String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/cell/{field}/edit");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
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
        return AdminError::NotFound(format!("no row {id}")).into_response();
    };
    let value = row.get(&field).cloned().unwrap_or_default();
    let input_type = input_kind(col.ty);

    let html = format!(
        r#"<form
            hx-post="/admin/{table}/{id}/cell/{field}"
            hx-target="closest td"
            hx-swap="innerHTML"
            class="flex items-center gap-xs"
            onkeydown="if(event.key==='Escape'){{this.parentElement && (this.parentElement.innerHTML = '<span class=&quot;text-on-surface text-body-md tabular-nums&quot;>{escaped_value}</span>')}}"
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
        escaped_value = html_escape(&value),
    );
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(axum::body::Body::from(html))
        .unwrap_or_else(|_| StatusCode::OK.into_response())
}

/// `POST /admin/{table}/{id}/cell/{field}`
/// Save inline cell edit. Returns the read-only cell value on success,
/// or an error span on failure.
async fn cell_edit_post(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id, field)): Path<(String, String, String)>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/{id}/cell/{field}");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render("no pk".to_string()).into_response();
    };
    let col = model.fields.iter().find(|c| c.name == field);
    let Some(col) = col else {
        return AdminError::NotFound(format!("no field `{field}`")).into_response();
    };
    let cfg = state.config_for(&table);
    if cfg.is_some_and(|c| c.readonly_fields.contains(&field)) {
        return (StatusCode::FORBIDDEN, "field is read-only").into_response();
    }
    let form: HashMap<String, String> = serde_urlencoded::from_str(&body).unwrap_or_default();
    let pool = umbra::db::pool();
    let sql = format!(
        "UPDATE \"{}\" SET \"{}\" = ? WHERE \"{}\" = ?",
        q(&model.table),
        q(&field),
        q(&pk.name)
    );
    let qb = sqlx::query(&sql);
    let qb = match bind_form_value(qb, col, &form) {
        Ok(q) => q,
        Err(e) => {
            let err_html = format!(
                r#"<span class="text-error text-body-sm">{}</span>"#,
                html_escape(&sanitise_form_error(&e))
            );
            return axum::response::Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "text/html")
                .body(axum::body::Body::from(err_html))
                .unwrap_or_else(|_| StatusCode::BAD_REQUEST.into_response());
        }
    };
    match qb.bind(id.clone()).execute(&pool).await {
        Ok(_) => {
            let new_value = form.get(&field).cloned().unwrap_or_default();
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
            let err_html = format!(
                r#"<span class="text-error text-body-sm">{}</span>"#,
                html_escape(&e.to_string())
            );
            axum::response::Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "text/html")
                .body(axum::body::Body::from(err_html))
                .unwrap_or_else(|_| StatusCode::BAD_REQUEST.into_response())
        }
    }
}

// ---- detail / new_form / create / edit_form / update / delete -------------

async fn detail(
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

async fn new_form(
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

async fn create(
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
            // Audit log
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

async fn edit_form(
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

async fn update(
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
            // Audit log
            let object_id = id.parse::<i64>().ok();
            crate::models::log(
                user.id,
                "update",
                &table,
                object_id,
                &format!("updated {} #{}", model.name, id),
            )
            .await;

            // HTMX from the Sheet form. Two cases:
            //   1. `_save_continue` is set: re-render the edit-sheet
            //      fragment so the sheet stays open with the saved
            //      values + a "Saved" flash banner. The user can keep
            //      editing.
            //   2. Plain Save: emit an HX-Trigger that closes the sheet
            //      and refreshes the table body, all without a full
            //      page nav.
            if is_htmx(&headers) {
                if form.contains_key("_save_continue") {
                    // Re-fetch + render the edit sheet inline.
                    return edit_sheet_handler(State(state), headers, Path((table, id))).await;
                }
                // Default Save: tell the page to close the sheet + refresh rows.
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

async fn delete(
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
    let pool = umbra::db::pool();
    let sql = format!(
        "DELETE FROM \"{}\" WHERE \"{}\" = ?",
        q(&model.table),
        q(&pk.name)
    );
    match sqlx::query(&sql).bind(&id).execute(&pool).await {
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


// =========================================================================
// Phase 2 — count helper.
// =========================================================================

async fn count_rows_filtered(
    pool: &SqlitePool,
    model: &ModelMeta,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filter: Option<(&str, &str)>,
) -> Result<usize, AdminError> {
    let valid_names: std::collections::HashSet<&str> =
        model.fields.iter().map(|c| c.name.as_str()).collect();

    let mut conditions: Vec<String> = Vec::new();
    let mut bind_strings: Vec<String> = Vec::new();

    if let Some(term) = search_term
        && let Some(c) = cfg
        && !c.search_fields.is_empty()
    {
        let like_clauses: Vec<String> = c
            .search_fields
            .iter()
            .filter(|f| valid_names.contains(f.as_str()))
            .map(|f| format!("\"{}\" LIKE ?", q(f)))
            .collect();
        if !like_clauses.is_empty() {
            conditions.push(format!("({})", like_clauses.join(" OR ")));
            let like_val = format!("%{term}%");
            for _ in 0..like_clauses.len() {
                bind_strings.push(like_val.clone());
            }
        }
    }

    if let Some((field, value)) = active_filter {
        if valid_names.contains(field) {
            conditions.push(format!("\"{}\" = ?", q(field)));
            bind_strings.push(value.to_string());
        }
    }

    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    let sql = format!("SELECT COUNT(*) FROM \"{}\"{where_sql}", q(&model.table));
    let mut qb = sqlx::query(&sql);
    for val in &bind_strings {
        qb = qb.bind(val.clone());
    }
    let row = qb.fetch_one(pool).await?;
    let count: i64 = row.try_get(0)?;
    Ok(count as usize)
}

// Fetch rows with explicit LIMIT/OFFSET for pagination.
#[allow(clippy::too_many_arguments)]
async fn fetch_rows_paged(
    pool: &SqlitePool,
    model: &ModelMeta,
    display_cols: &[String],
    order_clause: &str,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filter: Option<(&str, &str)>,
    limit: usize,
    offset: usize,
) -> Result<Vec<HashMap<String, String>>, AdminError> {
    let valid_names: std::collections::HashSet<&str> =
        model.fields.iter().map(|c| c.name.as_str()).collect();
    let columns = display_cols
        .iter()
        .filter(|n| valid_names.contains(n.as_str()))
        .map(|n| format!("\"{}\"", n))
        .collect::<Vec<_>>()
        .join(", ");
    let columns = if columns.is_empty() {
        model
            .fields
            .iter()
            .map(|c| format!("\"{}\"", c.name))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        columns
    };

    let mut conditions: Vec<String> = Vec::new();
    let mut bind_strings: Vec<String> = Vec::new();

    if let Some(term) = search_term
        && let Some(c) = cfg
        && !c.search_fields.is_empty()
    {
        let like_clauses: Vec<String> = c
            .search_fields
            .iter()
            .filter(|f| valid_names.contains(f.as_str()))
            .map(|f| format!("\"{}\" LIKE ?", q(f)))
            .collect();
        if !like_clauses.is_empty() {
            conditions.push(format!("({})", like_clauses.join(" OR ")));
            let like_val = format!("%{term}%");
            for _ in 0..like_clauses.len() {
                bind_strings.push(like_val.clone());
            }
        }
    }

    if let Some((field, value)) = active_filter {
        if valid_names.contains(field) {
            conditions.push(format!("\"{}\" = ?", q(field)));
            bind_strings.push(value.to_string());
        }
    }

    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };
    let order_sql = if order_clause.is_empty() {
        String::new()
    } else {
        format!(" ORDER BY {order_clause}")
    };

    let sql = format!(
        "SELECT {columns} FROM \"{}\"{where_sql}{order_sql} LIMIT ? OFFSET ?",
        q(&model.table)
    );

    let mut qb = sqlx::query(&sql);
    for val in &bind_strings {
        qb = qb.bind(val.clone());
    }
    qb = qb.bind(limit as i64).bind(offset as i64);

    let rows = qb.fetch_all(pool).await?;
    let mut out: Vec<HashMap<String, String>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut entry: HashMap<String, String> = HashMap::new();
        for col_name in display_cols {
            if let Some(col) = model.fields.iter().find(|c| &c.name == col_name) {
                entry.insert(col.name.clone(), column_to_string(&row, col)?);
            }
        }
        out.push(entry);
    }
    Ok(out)
}

// =========================================================================
// Phase 2 handlers.
// =========================================================================

/// `GET /admin/{table}/rows` — HTMX fragment: tbody + pagination footer.
async fn rows_fragment(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/{table}/rows");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    // Direct browser navigation to /rows would render the naked tbody
    // fragment without any chrome (no <head>, no fonts, no Tailwind,
    // default browser checkbox styling). Redirect to the changelist
    // page with the same query string preserved; the page itself will
    // HTMX-load the rows.
    if !is_htmx(&headers) {
        let qs = serde_urlencoded::to_string(&params).unwrap_or_default();
        let target = if qs.is_empty() {
            format!("/admin/{table}/")
        } else {
            format!("/admin/{table}/?{qs}")
        };
        return Redirect::to(&target).into_response();
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };

    let cfg = state.config_for(&table);
    let (search_term, active_filter, sort_col, sort_order, page, page_size) =
        parse_list_params(&params, cfg, pk);

    let display_cols: Vec<String> = if let Some(c) = cfg
        && !c.list_display.is_empty()
    {
        c.list_display.clone()
    } else {
        model.fields.iter().map(|f| f.name.clone()).collect()
    };

    // Always fetch the pk column so row actions have a valid ID.
    let fetch_cols: Vec<String> = {
        let mut cols = display_cols.clone();
        if !cols.contains(&pk.name) {
            cols.push(pk.name.clone());
        }
        cols
    };

    let order_clause = build_order_clause_phase2(cfg, pk, &sort_col, &sort_order);

    let pool = umbra::db::pool();
    let total = match count_rows_filtered(
        &pool,
        &model,
        search_term.as_deref(),
        cfg,
        active_filter
            .as_ref()
            .map(|(f, v)| (f.as_str(), v.as_str())),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let pagination = Pagination::new(total, page, page_size);

    let rows = match fetch_rows_paged(
        &pool,
        &model,
        &fetch_cols,
        &order_clause,
        search_term.as_deref(),
        cfg,
        active_filter
            .as_ref()
            .map(|(f, v)| (f.as_str(), v.as_str())),
        pagination.page_size,
        pagination.offset(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let columns = model_for_template_cols(&model, &display_cols).fields;
    let active_filter_str = active_filter
        .as_ref()
        .map(|(f, v)| format!("{f}={v}"))
        .unwrap_or_default();
    let search_val = search_term.unwrap_or_default();

    let action_names: Vec<serde_json::Value> = cfg.map(action_descriptors_json).unwrap_or_default();

    let inline_edit_fields: Vec<String> = cfg
        .map(|c| c.inline_edit_fields.clone())
        .unwrap_or_default();

    match render(
        "admin/rows_fragment.html",
        context!(
            table              => table,
            model_name         => model.name.clone(),
            rows               => rows,
            pk                 => pk.name.clone(),
            columns            => columns,
            pagination         => pagination,
            active_filter      => active_filter_str,
            search_val         => search_val,
            sort_col           => sort_col,
            sort_order         => sort_order,
            actions            => action_names,
            inline_edit_fields => inline_edit_fields,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/{table}/filter-dialog` — filter dialog fragment.
///
/// Renders the filter dialog modal for the given table. Only filterable
/// field types are shown; text fields in list_filter are silently dropped
/// with a debug log message.
async fn filter_dialog_handler(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path = format!("/admin/{table}/filter-dialog");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let cfg = state.config_for(&table);

    // Build facets — same as in list handler.
    let pool = umbra::db::pool();
    let mut facets: Vec<FilterFacet> = Vec::new();
    if let Some(c) = cfg {
        for field in &c.list_filter {
            // Find the column type.
            let col_ty = model
                .fields
                .iter()
                .find(|col| &col.name == field)
                .map(|col| col.ty);
            // Silently drop plain text and numeric fields.
            if let Some(ty) = col_ty {
                match ty {
                    SqlType::Text => {
                        tracing::debug!(
                            field = field.as_str(),
                            table = table.as_str(),
                            "text fields are not filterable; use search_fields"
                        );
                        continue;
                    }
                    SqlType::SmallInt
                    | SqlType::Integer
                    | SqlType::BigInt
                    | SqlType::Real
                    | SqlType::Double => {
                        tracing::debug!(
                            field = field.as_str(),
                            table = table.as_str(),
                            "numeric fields are not filterable via the filter dialog; use search_fields"
                        );
                        continue;
                    }
                    _ => {}
                }
            }
            let values = fetch_distinct_values(&pool, &model.table, field)
                .await
                .unwrap_or_default();
            facets.push(FilterFacet {
                field: field.clone(),
                values,
            });
        }
    }

    let search_val = params.get("search").cloned().unwrap_or_default();
    let sort_col = params.get("sort").cloned().unwrap_or_default();
    let sort_order = params.get("order").cloned().unwrap_or_default();
    let active_filter = params.get("active_filter").cloned().unwrap_or_default();
    let columns = model_for_template(&model).fields;

    match render(
        "admin/filter_dialog_fragment.html",
        context!(
            model         => model_for_template(&model),
            facets        => facets,
            columns       => columns,
            search_val    => search_val,
            sort_col      => sort_col,
            sort_order    => sort_order,
            active_filter => active_filter,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/{table}/{id}/sheet` — preview sheet fragment (or full page for non-HTMX).
async fn preview_sheet(
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
        // Non-HTMX: render the full changelist with the sheet pre-opened via JS.
        // Simplest approach: redirect to changelist with ?row=id so the page
        // can open the sheet on load via a small inline script.
        Redirect::to(&format!("/admin/{table}/?row={id}")).into_response()
    }
}

/// `GET /admin/{table}/{id}/edit-sheet` — edit sheet fragment (or full page).
async fn edit_sheet_handler(
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
async fn new_sheet(
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

/// `GET /admin/{table}/{id}/_confirm-delete` — delete confirm dialog fragment.
async fn confirm_delete_dialog(
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
    // Use the id as the display label — FK label resolution is phase 3.
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

/// `POST /admin/{table}/create` — sheet create flow.
/// On success: returns updated rows fragment for the full changelist.
/// On failure: returns the create sheet with errors.
async fn sheet_create(
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
            // Audit log
            crate::models::log(
                who.id,
                "create",
                &table,
                None,
                &format!("created {} (via sheet)", model.name),
            )
            .await;
            // Return HX-Redirect so HTMX refreshes the full changelist.
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

/// `DELETE /admin/{table}/{id}` — HTMX delete (returns HX-Redirect to refresh list).
async fn htmx_delete(
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
    let pool = umbra::db::pool();
    let sql = format!(
        "DELETE FROM \"{}\" WHERE \"{}\" = ?",
        q(&model.table),
        q(&pk.name)
    );
    match sqlx::query(&sql).bind(&id).execute(&pool).await {
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

/// `POST /admin/{table}/{id}/change-password`
///
/// Accepts `new_password` + `confirm_password` form fields. Hashes and
/// writes the new password if they match. Returns an HTMX-friendly
/// response with a toast trigger on success.
async fn change_password_handler(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
    body: String,
) -> Response {
    let path = format!("/admin/{table}/{id}/change-password");
    if let Err(r) = require_staff(&headers, &path).await {
        return r;
    }
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
    let pool = umbra::db::pool();
    let sql = format!(
        "UPDATE \"{}\" SET \"{}\" = ? WHERE \"{}\" = ?",
        q(&model.table),
        q(pw_col),
        q(&pk.name)
    );
    if let Err(e) = sqlx::query(&sql).bind(hash).bind(&id).execute(&pool).await {
        return AdminError::Sqlx(e).into_response();
    }
    // Return a toast trigger so the UI shows a success message.
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

fn sanitise_form_error(e: &AdminError) -> String {
    match e {
        AdminError::Sqlx(sqlx_err) => {
            tracing::error!(error = %sqlx_err, "admin: form submission database error");
            "database error".to_string()
        }
        AdminError::NotFound(msg) | AdminError::Render(msg) | AdminError::BadInput(msg) => {
            msg.clone()
        }
    }
}

// =========================================================================
// Row marshalling.
// =========================================================================

#[allow(clippy::too_many_arguments)]
async fn fetch_rows_filtered(
    pool: &SqlitePool,
    model: &ModelMeta,
    where_pk: Option<(&str, &str)>,
    display_cols: &[String],
    order_clause: &str,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filter: Option<(&str, &str)>,
) -> Result<Vec<HashMap<String, String>>, AdminError> {
    let valid_names: std::collections::HashSet<&str> =
        model.fields.iter().map(|c| c.name.as_str()).collect();
    let columns = display_cols
        .iter()
        .filter(|n| valid_names.contains(n.as_str()))
        .map(|n| format!("\"{}\"", n))
        .collect::<Vec<_>>()
        .join(", ");
    let columns = if columns.is_empty() {
        model
            .fields
            .iter()
            .map(|c| format!("\"{}\"", c.name))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        columns
    };

    let mut conditions: Vec<String> = Vec::new();
    let mut bind_strings: Vec<String> = Vec::new();

    if let Some((col, _val)) = where_pk {
        conditions.push(format!("\"{}\" = ?", q(col)));
        bind_strings.push(where_pk.unwrap().1.to_string());
    }

    if let Some(term) = search_term
        && let Some(c) = cfg
        && !c.search_fields.is_empty()
    {
        let like_clauses: Vec<String> = c
            .search_fields
            .iter()
            .filter(|f| valid_names.contains(f.as_str()))
            .map(|f| format!("\"{}\" LIKE ?", q(f)))
            .collect();
        if !like_clauses.is_empty() {
            conditions.push(format!("({})", like_clauses.join(" OR ")));
            let like_val = format!("%{term}%");
            for _ in 0..like_clauses.len() {
                bind_strings.push(like_val.clone());
            }
        }
    }

    if let Some((field, value)) = active_filter {
        if valid_names.contains(field) {
            conditions.push(format!("\"{}\" = ?", q(field)));
            bind_strings.push(value.to_string());
        }
    }

    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };
    let order_sql = if order_clause.is_empty() || where_pk.is_some() {
        String::new()
    } else {
        format!(" ORDER BY {order_clause}")
    };
    let limit_sql = if where_pk.is_some() {
        " LIMIT 1"
    } else {
        " LIMIT 200"
    };

    let sql = format!(
        "SELECT {columns} FROM \"{}\"{where_sql}{order_sql}{limit_sql}",
        q(&model.table)
    );

    let mut qb = sqlx::query(&sql);
    for val in &bind_strings {
        qb = qb.bind(val.clone());
    }

    let rows = qb.fetch_all(pool).await?;
    let mut out: Vec<HashMap<String, String>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut entry: HashMap<String, String> = HashMap::new();
        for col_name in display_cols {
            if let Some(col) = model.fields.iter().find(|c| &c.name == col_name) {
                entry.insert(col.name.clone(), column_to_string(&row, col)?);
            }
        }
        out.push(entry);
    }
    Ok(out)
}

fn column_to_string(row: &sqlx::sqlite::SqliteRow, col: &Column) -> Result<String, AdminError> {
    let name = col.name.as_str();
    if col.nullable {
        return Ok(match col.ty {
            SqlType::SmallInt | SqlType::Integer => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::BigInt => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Real => row
                .try_get::<Option<f32>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Double => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Boolean => row
                .try_get::<Option<bool>, _>(name)?
                .map_or(String::new(), |v| {
                    if v { "true" } else { "false" }.to_string()
                }),
            SqlType::Text => row.try_get::<Option<String>, _>(name)?.unwrap_or_default(),
            SqlType::Date => row
                .try_get::<Option<NaiveDate>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Time => row
                .try_get::<Option<NaiveTime>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Timestamptz => row
                .try_get::<Option<DateTime<Utc>>, _>(name)?
                .map_or(String::new(), |v| v.to_rfc3339()),
            SqlType::Uuid => row
                .try_get::<Option<Uuid>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Json => row
                .try_get::<Option<Value>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Array(_) => panic_array_unsupported(&col.name),
            SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
                panic_pg_only_unsupported(&col.name)
            }
            SqlType::ForeignKey => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => row.try_get::<i32, _>(name)?.to_string(),
        SqlType::BigInt => row.try_get::<i64, _>(name)?.to_string(),
        SqlType::Real => row.try_get::<f32, _>(name)?.to_string(),
        SqlType::Double => row.try_get::<f64, _>(name)?.to_string(),
        SqlType::Boolean => if row.try_get::<bool, _>(name)? {
            "true"
        } else {
            "false"
        }
        .to_string(),
        SqlType::Text => row.try_get::<String, _>(name)?,
        SqlType::Date => row.try_get::<NaiveDate, _>(name)?.to_string(),
        SqlType::Time => row.try_get::<NaiveTime, _>(name)?.to_string(),
        SqlType::Timestamptz => row.try_get::<DateTime<Utc>, _>(name)?.to_rfc3339(),
        SqlType::Uuid => row.try_get::<Uuid, _>(name)?.to_string(),
        SqlType::Json => row.try_get::<Value, _>(name)?.to_string(),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        SqlType::ForeignKey => row.try_get::<i64, _>(name)?.to_string(),
    })
}

fn panic_array_unsupported(column: &str) -> ! {
    panic!(
        "umbra-admin: column `{column}` is a Postgres-only Array; the \
         field.backend system check should have failed boot."
    )
}

fn panic_pg_only_unsupported(column: &str) -> ! {
    panic!(
        "umbra-admin: column `{column}` is a Postgres-only network type \
         (Inet/Cidr/MacAddr); the field.backend system check should \
         have failed boot."
    )
}

async fn insert_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
) -> Result<(), AdminError> {
    // If a password_field is configured and the form contains a plaintext
    // password value, hash it before binding. Also validate confirm match.
    let form_owned: HashMap<String, String>;
    let form = if let Some(pw_col) = cfg.and_then(|c| c.password_field.as_deref()) {
        if let Some(plaintext) = form.get(pw_col).filter(|v| !v.is_empty()) {
            let confirm_key = format!("{pw_col}_confirm");
            let confirm = form.get(&confirm_key).map(|s| s.as_str()).unwrap_or("");
            if plaintext != confirm {
                return Err(AdminError::BadInput("Passwords do not match.".to_string()));
            }
            let hash = umbra_auth::hash_password(plaintext)
                .map_err(|e| AdminError::BadInput(format!("password hashing failed: {e}")))?;
            let mut owned = form.clone();
            owned.insert(pw_col.to_string(), hash);
            form_owned = owned;
            &form_owned
        } else {
            form
        }
    } else {
        form
    };

    let all_col_names: Vec<&str> = model.fields.iter().map(|c| c.name.as_str()).collect();
    let readonly_owned: Vec<String> = if let Some(c) = cfg {
        c.effective_readonly_fields(&all_col_names)
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        all_col_names
            .iter()
            .filter(|n| crate::config::is_sensitive_column(n))
            .map(|s| s.to_string())
            .collect()
    };
    let readonly: std::collections::HashSet<&str> =
        readonly_owned.iter().map(|s| s.as_str()).collect();
    let writable: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| {
            !(readonly.contains(c.name.as_str())
                || (c.primary_key
                    && matches!(c.ty, SqlType::Integer | SqlType::BigInt | SqlType::SmallInt)
                    && form.get(&c.name).is_none_or(|v| v.is_empty())))
        })
        .collect();
    let names = writable
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = writable.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "INSERT INTO \"{}\" ({names}) VALUES ({placeholders})",
        q(&model.table)
    );
    let mut qb = sqlx::query(&sql);
    for col in &writable {
        qb = bind_form_value(qb, col, form)?;
    }
    qb.execute(pool).await?;
    Ok(())
}

async fn update_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    pk: &Column,
    pk_value: &str,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
) -> Result<(), AdminError> {
    let all_col_names: Vec<&str> = model.fields.iter().map(|c| c.name.as_str()).collect();
    let readonly_owned: Vec<String> = if let Some(c) = cfg {
        c.effective_readonly_fields(&all_col_names)
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        all_col_names
            .iter()
            .filter(|n| crate::config::is_sensitive_column(n))
            .map(|s| s.to_string())
            .collect()
    };
    let readonly: std::collections::HashSet<&str> =
        readonly_owned.iter().map(|s| s.as_str()).collect();
    let writable: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| !c.primary_key && !readonly.contains(c.name.as_str()))
        .collect();
    let setters = writable
        .iter()
        .map(|c| format!("\"{}\" = ?", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE \"{}\" SET {setters} WHERE \"{}\" = ?",
        q(&model.table),
        q(&pk.name)
    );
    let mut qb = sqlx::query(&sql);
    for col in &writable {
        qb = bind_form_value(qb, col, form)?;
    }
    qb = qb.bind(pk_value.to_string());
    qb.execute(pool).await?;
    Ok(())
}

fn bind_form_value<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    col: &Column,
    form: &HashMap<String, String>,
) -> Result<sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>, AdminError> {
    let raw = form.get(&col.name).cloned().unwrap_or_default();
    if raw.is_empty() {
        return Ok(match col.ty {
            SqlType::Boolean => q.bind(false),
            _ if col.nullable => bind_null(q, col),
            _ => {
                return Err(AdminError::BadInput(format!(
                    "field `{}` is required",
                    col.name
                )));
            }
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => q.bind(
            raw.parse::<i32>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::BigInt => q.bind(
            raw.parse::<i64>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Real => q.bind(
            raw.parse::<f32>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Double => q.bind(
            raw.parse::<f64>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Boolean => q.bind(matches!(raw.as_str(), "true" | "on" | "1")),
        SqlType::Text => q.bind(raw),
        SqlType::Date => q.bind(
            raw.parse::<NaiveDate>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Time => q.bind(
            raw.parse::<NaiveTime>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Timestamptz => {
            let s = if raw.contains(':') && !raw.contains('+') && !raw.ends_with('Z') {
                format!("{raw}:00Z")
            } else {
                raw.clone()
            };
            let parsed = DateTime::parse_from_rfc3339(&s)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?;
            q.bind(parsed.with_timezone(&Utc))
        }
        SqlType::Uuid => q.bind(
            Uuid::parse_str(&raw)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Json => q.bind(
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        SqlType::ForeignKey => q.bind(
            raw.parse::<i64>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
    })
}

fn bind_null<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    col: &Column,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match col.ty {
        SqlType::SmallInt | SqlType::Integer => q.bind(None::<i32>),
        SqlType::BigInt => q.bind(None::<i64>),
        SqlType::Real => q.bind(None::<f32>),
        SqlType::Double => q.bind(None::<f64>),
        SqlType::Boolean => q.bind(None::<bool>),
        SqlType::Text => q.bind(None::<String>),
        SqlType::Date => q.bind(None::<NaiveDate>),
        SqlType::Time => q.bind(None::<NaiveTime>),
        SqlType::Timestamptz => q.bind(None::<DateTime<Utc>>),
        SqlType::Uuid => q.bind(None::<Uuid>),
        SqlType::Json => q.bind(None::<Value>),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        SqlType::ForeignKey => q.bind(None::<i64>),
    }
}

// =========================================================================
// Pagination helpers.
// =========================================================================

// =========================================================================
// Template helpers.
// =========================================================================


// =========================================================================
// Phase 4: built-in dashboard widget definitions.
// =========================================================================

fn builtin_total_models_widget() -> Widget {
    Widget {
        key: "umbra_total_models",
        title: "Total Models".to_string(),
        kind: WidgetKind::Kpi,
        default_span: Span { cols: 3, rows: 1 },
        permission: None,
        data: WidgetDataFn::new(|_user| async move {
            let count = discover_models().len();
            WidgetPayload::Kpi(KpiPayload {
                value: count.to_string(),
                unit: Some("models".to_string()),
                delta: None,
                sparkline: None,
            })
        }),
    }
}

fn builtin_recent_users_widget() -> Widget {
    Widget {
        key: "umbra_recent_users",
        title: "Recent Signups".to_string(),
        kind: WidgetKind::Feed,
        default_span: Span { cols: 4, rows: 2 },
        permission: None,
        data: WidgetDataFn::new(|_user| async move {
            // Attempt to read from auth_user table; gracefully degrade if absent.
            // Column is `date_joined` (as defined in umbra-auth); fall back to
            // empty list on any error so the dashboard still renders.
            let pool = umbra::db::pool();
            let rows_result = sqlx::query(
                "SELECT username, date_joined FROM auth_user ORDER BY date_joined DESC LIMIT 5",
            )
            .fetch_all(&pool)
            .await;
            let items = match rows_result {
                Ok(rows) => rows
                    .into_iter()
                    .map(|r| {
                        use sqlx::Row;
                        let actor: String = r.try_get("username").unwrap_or_default();
                        // date_joined is a Timestamptz; format as a short string.
                        let at: String = r
                            .try_get::<DateTime<Utc>, _>("date_joined")
                            .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                            .or_else(|_| r.try_get::<String, _>("date_joined"))
                            .unwrap_or_default();
                        crate::widgets::FeedItem {
                            actor,
                            verb: "joined".to_string(),
                            object: "account".to_string(),
                            object_link: None,
                            at,
                        }
                    })
                    .collect(),
                Err(e) => {
                    tracing::debug!(error = %e, "umbra_recent_users: auth_user query failed; returning empty feed");
                    vec![]
                }
            };
            WidgetPayload::Feed(FeedPayload { items })
        }),
    }
}

// =========================================================================
// Phase 4: user preferences handlers.
// =========================================================================

/// `GET /admin/api/prefs` — return the current user's prefs row, creating
/// defaults on first access.
async fn get_prefs_handler(headers: HeaderMap) -> Response {
    let user = match require_staff(&headers, "/admin/api/prefs").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    match crate::models::fetch_or_default(user.id).await {
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
/// Body: `application/json` with `{theme?, density?, sidebar_collapsed?}`.
async fn put_prefs_handler(headers: HeaderMap, body: String) -> Response {
    let user = match require_staff(&headers, "/admin/api/prefs").await {
        Ok(u) => u,
        Err(r) => return r,
    };

    // Fetch existing (or default) prefs, then overlay the submitted fields.
    let mut prefs = match crate::models::fetch_or_default(user.id).await {
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

    match crate::models::upsert(prefs).await {
        Ok(_) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "admin: put_prefs upsert failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "prefs save error").into_response()
        }
    }
}

// =========================================================================
// Phase 4: audit history handler.
// =========================================================================

/// `GET /admin/{table}/{id}/history` — audit timeline for one object.
async fn history_handler(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/history");
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let _ = &user; // actor known but not needed here
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model `{table}`")).into_response();
    };
    let object_id: i64 = match id.parse() {
        Ok(v) => v,
        Err(_) => return AdminError::BadInput(format!("invalid id: {id}")).into_response(),
    };
    let entries = match crate::models::audit_for_object(&table, object_id, 50).await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "admin: audit_for_object failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "audit error").into_response();
        }
    };

    let apps = sidebar_apps(&state, &user);
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

// =========================================================================
// Phase 4: dashboard API handlers.
// =========================================================================

/// `GET /admin/api/dashboard/catalog` — list widgets the user may add.
async fn dashboard_catalog(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if let Err(r) = require_staff(&headers, "/admin/api/dashboard/catalog").await {
        return r;
    }
    let entries: Vec<CatalogEntry> = state
        .widget_catalog
        .iter()
        .map(|w| CatalogEntry {
            key: w.key,
            title: w.title.clone(),
            kind: w.kind.as_str().to_string(),
            default_span: w.default_span.clone(),
        })
        .collect();
    Json(entries).into_response()
}

/// `GET /admin/api/dashboard/layout` — user's saved layout or default.
async fn dashboard_layout_get(headers: HeaderMap) -> Response {
    let user = match require_staff(&headers, "/admin/api/dashboard/layout").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let prefs = match crate::models::fetch_or_default(user.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "admin: dashboard_layout_get failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "layout error").into_response();
        }
    };
    // Return as raw JSON string (the layout is stored serialized).
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(prefs.dashboard_layout))
        .unwrap_or_else(|_| (StatusCode::OK, "[]").into_response())
}

/// `PUT /admin/api/dashboard/layout` — save user's layout.
async fn dashboard_layout_put(headers: HeaderMap, body: String) -> Response {
    let user = match require_staff(&headers, "/admin/api/dashboard/layout").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    // Validate that the body is valid JSON (an array of widget instances).
    if serde_json::from_str::<serde_json::Value>(&body).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid JSON layout").into_response();
    }
    let mut prefs = match crate::models::fetch_or_default(user.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "admin: dashboard_layout_put fetch failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "layout error").into_response();
        }
    };
    prefs.dashboard_layout = body;
    match crate::models::upsert(prefs).await {
        Ok(_) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "admin: dashboard_layout_put save failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "layout save error").into_response()
        }
    }
}

/// `GET /admin/api/dashboard/widgets/{key}/data` — compute + return one widget's payload.
///
/// Returns either JSON (API consumers) or an HTML fragment (HTMX swap).
async fn dashboard_widget_data(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Response {
    let user = match require_staff(&headers, "/admin/api/dashboard/widgets/.../data").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let widget = state.widget_catalog.iter().find(|w| w.key == key.as_str());
    let Some(widget) = widget else {
        return AdminError::NotFound(format!("no widget `{key}`")).into_response();
    };

    let data_fn = widget.data.0.clone();
    let payload = data_fn(user).await;

    // For HTMX requests render the HTML fragment; otherwise return JSON.
    if is_htmx(&headers) {
        let kind = widget.kind.as_str().to_string();
        let title = widget.title.clone();
        let payload_json = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
        match render(
            "admin/widget_data.html",
            context!(
                kind    => kind,
                title   => title,
                payload => payload_json,
            ),
        ) {
            Ok(html) => html.into_response(),
            Err(e) => e.into_response(),
        }
    } else {
        Json(serde_json::json!({
            "key": key,
            "kind": widget.kind.as_str(),
            "title": widget.title,
            "payload": serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null),
        }))
        .into_response()
    }
}

// =========================================================================
// Phase 4: command palette fragment.
// =========================================================================

/// `GET /admin/api/palette` — returns the command palette HTML fragment.
///
/// Jump targets = registered models from the sidebar. Fixed commands = toggle
/// theme + logout.
async fn palette_fragment(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let user = match require_staff(&headers, "/admin/api/palette").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let sidebar = sidebar_apps(&state, &user);

    // Flatten all model entries into a simple list for jump targets.
    let models: Vec<serde_json::Value> = sidebar
        .into_iter()
        .flat_map(|app| app.models)
        .map(|r| {
            serde_json::json!({
                "table": r.table,
                "label": r.label,
                "icon": r.icon,
            })
        })
        .collect();

    let commands = vec![
        serde_json::json!({ "key": "toggle_theme", "label": "Toggle theme", "icon": "sun-moon" }),
        serde_json::json!({ "key": "logout",       "label": "Logout",       "icon": "log-out" }),
    ];

    match render(
        "admin/palette.html",
        context!(
            models   => models,
            commands => commands,
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

// =========================================================================
// Phase 4: palette global record search.
// =========================================================================

/// `GET /admin/api/palette/search?q=<term>` — search across all registered
/// models that have `search_fields` configured and return up to 10 matching
/// rows as palette items (HTML fragment for HTMX swap into #umbra-palette-records).
async fn palette_search(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = require_staff(&headers, "/admin/api/palette/search").await {
        return r;
    }
    let q = params.get("q").map(|s| s.as_str()).unwrap_or("").trim();
    if q.len() < 2 {
        // Return empty fragment for short queries.
        return axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html")
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| StatusCode::OK.into_response());
    }

    let pool = umbra::db::pool();
    let mut html = String::new();
    let mut total_found = 0usize;
    const MAX_RESULTS: usize = 10;

    for (_, model) in discover_models() {
        if total_found >= MAX_RESULTS {
            break;
        }
        let cfg = state.config_for(&model.table);
        let search_fields: Vec<String> = cfg
            .filter(|c| !c.search_fields.is_empty())
            .map(|c| c.search_fields.clone())
            .unwrap_or_default();
        if search_fields.is_empty() {
            continue;
        }

        let valid_names: std::collections::HashSet<&str> =
            model.fields.iter().map(|c| c.name.as_str()).collect();
        let pk = match pk_column(&model) {
            Some(p) => p,
            None => continue,
        };

        // Pick a human-readable label column: first non-pk text column.
        let label_col = model
            .fields
            .iter()
            .find(|c| !c.primary_key && matches!(c.ty, umbra::orm::SqlType::Text))
            .map(|c| c.name.as_str())
            .unwrap_or(pk.name.as_str());

        let like_clauses: Vec<String> = search_fields
            .iter()
            .filter(|f| valid_names.contains(f.as_str()))
            .map(|f| format!("\"{}\" LIKE ?", crate::q(f)))
            .collect();
        if like_clauses.is_empty() {
            continue;
        }

        let where_sql = format!("WHERE ({})", like_clauses.join(" OR "));
        let sql = format!(
            "SELECT \"{pk_col}\", \"{label_col}\" FROM \"{table}\" {where_sql} LIMIT ?",
            pk_col = crate::q(&pk.name),
            label_col = crate::q(label_col),
            table = crate::q(&model.table),
        );
        let like_val = format!("%{q}%");
        let remaining = MAX_RESULTS - total_found;

        let mut qb = sqlx::query(&sql);
        for _ in &like_clauses {
            qb = qb.bind(like_val.clone());
        }
        qb = qb.bind(remaining as i64);

        if let Ok(rows) = qb.fetch_all(&pool).await {
            for row in rows {
                if total_found >= MAX_RESULTS {
                    break;
                }
                let id: String = row
                    .try_get::<i64, _>(0)
                    .map(|v| v.to_string())
                    .or_else(|_| row.try_get::<String, _>(0))
                    .unwrap_or_default();
                let label: String = row
                    .try_get::<String, _>(1)
                    .unwrap_or_else(|_| format!("#{id}"));
                let item_label = format!("{}: {}", model.name, label);
                let href = format!("/admin/{}/{}/sheet", model.table, id);
                html.push_str(&format!(
                    r#"<li role="option" data-palette-href="{href}" class="palette-item flex items-center gap-sm px-lg py-sm cursor-pointer hover:bg-surface-container-high transition-colors group" onclick="umbra._paletteGo(this)" tabindex="-1">
  <div class="w-8 h-8 rounded-xl bg-primary-container/10 border border-primary/20 flex items-center justify-center flex-shrink-0">
    <i data-lucide="file-search" class="w-4 h-4 text-primary"></i>
  </div>
  <span class="text-body-md text-on-surface">{label}</span>
  <span class="ml-auto text-label-sm text-outline opacity-0 group-hover:opacity-100 transition-opacity">Open</span>
</li>"#,
                    href = html_escape(&href),
                    label = html_escape(&item_label),
                ));
                total_found += 1;
            }
        }
    }

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(axum::body::Body::from(html))
        .unwrap_or_else(|_| StatusCode::OK.into_response())
}

// =========================================================================
// Unit tests (pure logic — no DB needed).
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_model_defaults() {
        let m = AdminModel::new("post");
        assert_eq!(m.get_list_per_page(), 25);
        assert!(m.inlines.is_empty());
        assert!(m.label.is_none());
        assert!(m.icon.is_none());
    }

    #[test]
    fn admin_config_alias_compiles() {
        // The type alias must be identical to AdminModel at the Rust level.
        let _: AdminConfig = AdminModel::new("test");
    }
}
