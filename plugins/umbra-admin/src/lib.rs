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
mod handlers;
mod pagination;
mod rows;
mod static_assets;
mod util;
mod view;

pub mod files;

pub(crate) use auth::{login_get, login_post, logout_handler, require_staff};
pub(crate) use discovery::{find_model, pk_column, user_theme};
pub(crate) use engine::render;
pub(crate) use error::AdminError;
pub use files::{file_descriptor, resolve_preview_kind};
pub(crate) use pagination::{Pagination, build_order_clause_phase2, parse_list_params};
pub(crate) use rows::{
    count_rows_filtered, fetch_rows_filtered, fetch_rows_paged, insert_row, update_row,
};
pub(crate) use static_assets::serve_admin_css;
pub(crate) use util::{is_htmx, q, sanitise_form_error};
pub(crate) use view::{
    form_fields_for, model_for_template, model_for_template_cols, sidebar_apps,
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
use minijinja::context;
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use umbra::orm::SqlType;
use umbra::prelude::*;
use umbra::web::{HeaderMap, IntoResponse, Path, Redirect, Response, StatusCode, post};

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
        let mut catalog = vec![
            handlers::dashboard::builtin_total_models_widget(),
            handlers::dashboard::builtin_recent_users_widget(),
        ];
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
            .route("/admin/{table}/action", post(handlers::actions::run_action))
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
                axum::routing::post(handlers::actions::dispatch_action),
            )
            // Phase 3: FK/M2M async picker endpoints
            .route(
                "/admin/api/{table}/{field}/options/resolve",
                axum::routing::get(handlers::fk_picker::fk_options_resolve),
            )
            .route(
                "/admin/api/{table}/{field}/options",
                axum::routing::get(handlers::fk_picker::fk_options),
            )
            // Phase 3: inline cell edit
            .route(
                "/admin/{table}/{id}/cell/{field}/edit",
                axum::routing::get(handlers::inline_edit::cell_edit_get),
            )
            .route(
                "/admin/{table}/{id}/cell/{field}",
                axum::routing::post(handlers::inline_edit::cell_edit_post),
            )
            // Password change for models with password_field set
            .route(
                "/admin/{table}/{id}/change-password",
                axum::routing::post(change_password_handler),
            )
            // Phase 4: user prefs
            .route(
                "/admin/api/prefs",
                axum::routing::get(handlers::prefs::get_prefs_handler)
                    .put(handlers::prefs::put_prefs_handler),
            )
            // Phase 4: audit history
            .route(
                "/admin/{table}/{id}/history",
                axum::routing::get(handlers::history::history_handler),
            )
            // Phase 4: dashboard
            .route(
                "/admin/api/dashboard/catalog",
                axum::routing::get(handlers::dashboard::dashboard_catalog),
            )
            .route(
                "/admin/api/dashboard/layout",
                axum::routing::get(handlers::dashboard::dashboard_layout_get)
                    .put(handlers::dashboard::dashboard_layout_put),
            )
            .route(
                "/admin/api/dashboard/widgets/{key}/data",
                axum::routing::get(handlers::dashboard::dashboard_widget_data),
            )
            // Phase 4: command palette fragment + global record search
            .route(
                "/admin/api/palette",
                axum::routing::get(handlers::palette::palette_fragment),
            )
            .route(
                "/admin/api/palette/search",
                axum::routing::get(handlers::palette::palette_search),
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

    let action_names: Vec<serde_json::Value> = cfg.map(handlers::action_descriptors_json).unwrap_or_default();

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

    let action_names: Vec<serde_json::Value> = cfg.map(handlers::action_descriptors_json).unwrap_or_default();

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

// =========================================================================
// Row marshalling.
// =========================================================================

#[allow(clippy::too_many_arguments)]

// =========================================================================
// Pagination helpers.
// =========================================================================


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
