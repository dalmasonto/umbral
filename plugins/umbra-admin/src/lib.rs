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
mod branding;
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

pub(crate) use auth::{login_get, login_post, logout_handler};
pub(crate) use error::AdminError;
pub use files::{file_descriptor, resolve_preview_kind};
pub(crate) use static_assets::admin_static_files;
pub(crate) use util::q;

pub use config::{
    Action, ActionInvocation, ActionResult, ActionScope, ActionVariant, AdminConfig, AdminContext,
    AdminModel, InlineModel, ToastLevel,
};
pub use registry::{AdminRegistration, AdminRegistry, App as AdminApp};
pub use widgets::{
    BarPayload, CatalogEntry, FeedItem, FeedPayload, KpiPayload, LinePayload, Series, Span,
    TableColumn, TablePayload, Widget, WidgetDataFn, WidgetInstance, WidgetKind, WidgetPayload,
};

use std::sync::Arc;

use umbra::prelude::*;
use umbra::web::post;

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
    branding: branding::AdminBranding,
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

    /// Override the admin site title — shown in the browser tab,
    /// the sidebar header, and the login page.
    ///
    /// ```ignore
    /// AdminPlugin::default().site_title("Acme Backoffice")
    /// ```
    pub fn site_title(mut self, title: impl Into<String>) -> Self {
        self.branding.site_title = title.into();
        self
    }

    /// One-line description shown on the dashboard / login page
    /// underneath the site title.
    pub fn site_description(mut self, description: impl Into<String>) -> Self {
        self.branding.site_description = description.into();
        self
    }

    /// Override the brand primary color. Accepts any valid CSS color
    /// (`#5b5bd6`, `rgb(91 91 214)`, `hsl(240 60% 60%)`). The wrapper
    /// template emits a `<style>` that re-assigns `--primary` and
    /// `--primary-container` so every "primary"-tinted element across
    /// the admin picks it up automatically.
    pub fn brand_color(mut self, color: impl Into<String>) -> Self {
        self.branding.brand_color = color.into();
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

    fn static_files(&self) -> Vec<umbra::plugin::StaticFile> {
        admin_static_files()
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![
            umbra::migrate::ModelMeta::for_::<crate::models::AdminUserPref>(),
            umbra::migrate::ModelMeta::for_::<crate::models::AdminAuditLog>(),
        ]
    }

    fn routes(&self) -> Router {
        // Seal the developer-configured branding into the global so
        // the template engine picks it up on first init. Subsequent
        // attempts to set it are silent no-ops; the typical flow is
        // exactly one Plugin::routes() call per process.
        let _ = branding::BRANDING.set(self.branding.clone());

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
            .route("/admin", axum::routing::get(handlers::list::index))
            .route("/admin/", axum::routing::get(handlers::list::index))
            .route("/admin/{table}/", axum::routing::get(handlers::list::list))
            .route(
                "/admin/{table}/new",
                axum::routing::get(handlers::crud::new_form).post(handlers::crud::create),
            )
            .route("/admin/{table}/action", post(handlers::actions::run_action))
            // Phase 2: fragment-only rows endpoint (search/sort/filter/paginate)
            .route(
                "/admin/{table}/rows",
                axum::routing::get(handlers::list::rows_fragment),
            )
            // Filter dialog fragment
            .route(
                "/admin/{table}/filter-dialog",
                axum::routing::get(handlers::list::filter_dialog_handler),
            )
            // Phase 2: new-record sheet (create mode)
            .route(
                "/admin/{table}/new-sheet",
                axum::routing::get(handlers::sheet::new_sheet),
            )
            // Phase 2: delete confirm dialog fragment
            .route(
                "/admin/{table}/{id}/_confirm-delete",
                axum::routing::get(handlers::sheet::confirm_delete_dialog),
            )
            // Phase 2: sheet fragments (preview + edit)
            .route(
                "/admin/{table}/{id}/sheet",
                axum::routing::get(handlers::sheet::preview_sheet),
            )
            .route(
                "/admin/{table}/{id}/edit-sheet",
                axum::routing::get(handlers::sheet::edit_sheet_handler),
            )
            .route(
                "/admin/{table}/{id}",
                axum::routing::get(handlers::crud::detail),
            )
            .route(
                "/admin/{table}/{id}/edit",
                axum::routing::get(handlers::crud::edit_form).post(handlers::crud::update),
            )
            // Phase 2: create via sheet (POST)
            .route(
                "/admin/{table}/create",
                axum::routing::post(handlers::sheet::sheet_create),
            )
            // Phase 2: DELETE method for HTMX delete button
            .route(
                "/admin/{table}/{id}",
                axum::routing::delete(handlers::crud::htmx_delete),
            )
            .route("/admin/{table}/{id}/delete", post(handlers::crud::delete))
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
                axum::routing::post(handlers::sheet::change_password_handler),
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
            // Static admin.css is mounted by the framework via
            // `static_files()` — no manual route needed here.
            .with_state(state)
    }

    fn route_paths(&self) -> Vec<umbra::routes::RouteSpec> {
        // Companion list to `routes()` — surfaced by the dev-mode
        // default 404 page. Each entry pairs a path pattern with the
        // HTTP methods it accepts; keep in sync with the `.route(...)`
        // calls above. Mismatch is "stale route list," not a routing
        // bug.
        use umbra::routes::RouteSpec;
        // Method shorthands — each constructed once and `clone()`d per
        // entry so the source list stays one-line-per-route.
        let g = || vec!["GET"];
        let p = || vec!["POST"];
        let gp = || vec!["GET", "POST"];
        let gpd = || vec!["GET", "POST", "DELETE"];
        let gput = || vec!["GET", "PUT"];
        vec![
            RouteSpec::new("/admin", g()),
            RouteSpec::new("/admin/", g()),
            RouteSpec::new("/admin/login", gp()),
            RouteSpec::new("/admin/logout", g()),
            RouteSpec::new("/admin/{table}/", g()),
            RouteSpec::new("/admin/{table}/new", gp()),
            RouteSpec::new("/admin/{table}/action", p()),
            RouteSpec::new("/admin/{table}/rows", g()),
            RouteSpec::new("/admin/{table}/filter-dialog", g()),
            RouteSpec::new("/admin/{table}/new-sheet", g()),
            RouteSpec::new("/admin/{table}/create", p()),
            RouteSpec::new("/admin/{table}/{id}", gpd()),
            RouteSpec::new("/admin/{table}/{id}/edit", gp()),
            RouteSpec::new("/admin/{table}/{id}/edit-sheet", g()),
            RouteSpec::new("/admin/{table}/{id}/sheet", g()),
            RouteSpec::new("/admin/{table}/{id}/delete", p()),
            RouteSpec::new("/admin/{table}/{id}/_confirm-delete", g()),
            RouteSpec::new("/admin/{table}/{id}/history", g()),
            RouteSpec::new("/admin/{table}/{id}/change-password", p()),
            RouteSpec::new("/admin/{table}/{id}/cell/{field}", p()),
            RouteSpec::new("/admin/{table}/{id}/cell/{field}/edit", g()),
            RouteSpec::new("/admin/{table}/actions/{key}", p()),
            RouteSpec::new("/admin/api/{table}/{field}/options", g()),
            RouteSpec::new("/admin/api/{table}/{field}/options/resolve", g()),
            RouteSpec::new("/admin/api/prefs", gput()),
            RouteSpec::new("/admin/api/palette", g()),
            RouteSpec::new("/admin/api/palette/search", g()),
            RouteSpec::new("/admin/api/dashboard/catalog", g()),
            RouteSpec::new("/admin/api/dashboard/layout", gput()),
            RouteSpec::new("/admin/api/dashboard/widgets/{key}/data", g()),
        ]
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
