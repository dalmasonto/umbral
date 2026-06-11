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
mod permcheck;
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
// The two builtin dashboard widgets — `Models by Plugin` (bar)
// and `Recent Signups` (feed). Used to be auto-prepended to the
// catalog; now exposed as public functions so the caller can
// register them at the position they want and resize via
// `.with_span(cols, rows)`. See `AdminPlugin::register_widget`
// for the wiring shape.
pub use handlers::dashboard::{builtin_recent_users_widget, builtin_total_models_widget};
pub use widgets::{
    BarPayload, CardPayload, CatalogEntry, ChartPoint, DonutPayload, DonutSlice, FeedItem,
    FeedPayload, KpiPayload, LinePayload, Series, Span, TableColumn, TablePayload, Widget,
    WidgetDataFn, WidgetInstance, WidgetKind, WidgetParams, WidgetPayload, WidgetSection,
    format_thousands, humanize_number,
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
/// How the dashboard renders its "Models" cards section.
///
/// Default: [`Self::All`] — every registered model gets a card.
/// This works for a 5-20 model app but turns into a wall of 200
/// cards on a real-world enterprise install. Use [`Self::Only`]
/// to pick a curated subset, or [`Self::Hidden`] to drop the
/// section entirely (e.g. when the operator's primary view is
/// purely widget-driven).
#[derive(Debug, Clone)]
pub enum DashboardModelsConfig {
    /// Default — show a card for every registered model.
    All,
    /// Hide the section entirely. The dashboard becomes:
    /// greeting → quick stats → widgets, no model grid.
    Hidden,
    /// Show only these tables, in the given order. Unknown
    /// table names are dropped silently (typo-safe; if a
    /// plugin you reference is unregistered the rest still
    /// render).
    Only(Vec<String>),
}

impl Default for DashboardModelsConfig {
    fn default() -> Self {
        Self::All
    }
}

#[derive(Debug, Clone)]
pub struct AdminPlugin {
    registry: AdminRegistry,
    widget_catalog: Vec<Widget>,
    /// Explicit named sections (each with title + subtitle + widget
    /// list). Empty by default — back-compat for apps that only use
    /// the legacy `register_widget` call. When non-empty, the
    /// dashboard renders these sections first; any widgets in
    /// `widget_catalog` get an implicit final "Widgets" section.
    dashboard_sections: Vec<WidgetSection>,
    branding: branding::AdminBranding,
    /// Gap 107: base URL prefix for every admin route. Default
    /// `/admin`. Override with `AdminPlugin::default().at("/myadmin")`.
    /// Always normalised to one leading slash, no trailing slash.
    base_path: String,
    /// Dashboard model-cards config. Defaults to `All` so the
    /// dashboard does something sensible on a fresh install.
    dashboard_models: DashboardModelsConfig,
    /// Heading shown above the model-cards section. Default
    /// "Models" — override with `.dashboard_models_title(...)`.
    dashboard_models_title: String,
    /// Optional one-line subtitle under the heading.
    dashboard_models_subtitle: Option<String>,
}

impl Default for AdminPlugin {
    fn default() -> Self {
        Self {
            registry: AdminRegistry::default(),
            widget_catalog: Vec::new(),
            dashboard_sections: Vec::new(),
            branding: branding::AdminBranding::default(),
            base_path: "/admin".to_string(),
            dashboard_models: DashboardModelsConfig::default(),
            dashboard_models_title: "Models".to_string(),
            dashboard_models_subtitle: None,
        }
    }
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

    /// Gap 107: mount the admin at a path other than the default
    /// `/admin`. Useful when a single domain hosts multiple umbra
    /// admins, or when the operations team enforces a different
    /// vanity URL. Accepts `"/myadmin"`, `"myadmin"`, or
    /// `"/myadmin/"` — all normalise to `"/myadmin"`.
    ///
    /// ```ignore
    /// AdminPlugin::default().at("/backoffice")
    /// // → routes mount at /backoffice/login, /backoffice/{table}/, ...
    /// ```
    ///
    /// Templates read the configured base via the `admin_base`
    /// Jinja global, so cross-page links resolve to the new path
    /// automatically. Handler-side redirects and `sanitise_next`
    /// also use the configured base.
    pub fn at(mut self, path: impl Into<String>) -> Self {
        let raw = path.into();
        let trimmed = raw.trim_matches('/');
        self.base_path = if trimmed.is_empty() {
            String::new()
        } else {
            format!("/{trimmed}")
        };
        self
    }

    /// The normalised admin base path. Public so plugin authors and
    /// the OpenAPI plugin can reference it.
    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    /// Hide the dashboard's "Models" cards section entirely. Use
    /// when the operator's primary view is widget-driven and a
    /// long model grid would be noise (200-model enterprise
    /// installs, single-purpose admins, etc.).
    ///
    /// ```ignore
    /// AdminPlugin::default().dashboard_models_hidden()
    /// ```
    pub fn dashboard_models_hidden(mut self) -> Self {
        self.dashboard_models = DashboardModelsConfig::Hidden;
        self
    }

    /// Show only a curated subset of models on the dashboard, in
    /// the given order. Unknown table names are dropped silently
    /// (typo-safe — if one plugin is unregistered the rest still
    /// render).
    ///
    /// ```ignore
    /// AdminPlugin::default().dashboard_models_only(&[
    ///     "product", "order", "customer",
    /// ])
    /// ```
    ///
    /// Type-safe alternative coming in a follow-up: a
    /// `models![Product, Order, Customer]` macro that resolves
    /// each type to its `Model::TABLE` so a rename in the
    /// struct doesn't require updating string references here.
    pub fn dashboard_models_only<S: Into<String> + Clone>(mut self, tables: &[S]) -> Self {
        self.dashboard_models =
            DashboardModelsConfig::Only(tables.iter().cloned().map(Into::into).collect());
        self
    }

    /// Explicit reset to the default — show every registered
    /// model. Useful when a wrapper builder has previously
    /// configured a subset / hidden and you want the full grid
    /// back.
    pub fn dashboard_models_all(mut self) -> Self {
        self.dashboard_models = DashboardModelsConfig::All;
        self
    }

    /// Append a named widget section to the dashboard. Sections
    /// render in registration order, each with its own heading
    /// + (optional) subtitle + widget grid:
    ///
    /// ```ignore
    /// AdminPlugin::default()
    ///   .dashboard_section(
    ///       WidgetSection::new("Sales overview")
    ///           .subtitle("Daily KPIs across the storefront")
    ///           .widget(shop_total_sales_widget())
    ///           .widget(shop_orders_widget()))
    ///   .dashboard_section(
    ///       WidgetSection::new("Engagement")
    ///           .widget(umbra_admin::builtin_recent_users_widget()))
    /// ```
    ///
    /// Widgets registered via the legacy `register_widget(...)`
    /// land in an implicit final section titled "Widgets" so
    /// pre-existing apps keep working without refactor.
    pub fn dashboard_section(mut self, section: WidgetSection) -> Self {
        self.dashboard_sections.push(section);
        self
    }

    /// Insert a section at a specific position in the dashboard.
    /// Useful when a wrapper builder appended sections earlier
    /// and you want a new one above them. `index` is clamped at
    /// the current section count, so `usize::MAX` is equivalent
    /// to [`Self::dashboard_section`].
    ///
    /// ```ignore
    /// AdminPlugin::default()
    ///   .dashboard_section(sales_section)
    ///   .dashboard_section(system_section)
    ///   // Slot a new section between the two:
    ///   .dashboard_section_at(1, alerts_section)
    /// ```
    pub fn dashboard_section_at(mut self, index: usize, section: WidgetSection) -> Self {
        let i = index.min(self.dashboard_sections.len());
        self.dashboard_sections.insert(i, section);
        self
    }

    /// Override the heading shown above the model-cards section.
    /// Default "Models". Pair with `dashboard_models_subtitle`
    /// for a one-line explainer.
    pub fn dashboard_models_title(mut self, title: impl Into<String>) -> Self {
        self.dashboard_models_title = title.into();
        self
    }

    /// Optional one-line caption under the model-cards heading.
    pub fn dashboard_models_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.dashboard_models_subtitle = Some(subtitle.into());
        self
    }
}

/// Shared state injected into every route via [`axum::extract::State`].
///
/// `Arc` makes the clone cheap; the registry is immutable after `build()`.
#[derive(Clone, Debug)]
struct AdminState {
    registry: Arc<AdminRegistry>,
    /// Flat widget catalog — every widget across all sections.
    /// Used by `GET /admin/api/dashboard/widgets/<key>/data` to
    /// look up by key without knowing which section owns it.
    widget_catalog: Arc<Vec<Widget>>,
    /// Dashboard sections in render order. Each carries its own
    /// title + subtitle + widgets. The implicit "Widgets" section
    /// (from legacy `register_widget(...)` calls) lives at the end.
    dashboard_sections: Arc<Vec<WidgetSection>>,
    /// Dashboard model-cards section config. Read by the
    /// dashboard handler to filter (or skip) the model grid.
    dashboard_models: DashboardModelsConfig,
    /// Heading + optional subtitle for the model-cards section.
    dashboard_models_title: String,
    dashboard_models_subtitle: Option<String>,
}

impl AdminState {
    fn config_for(&self, table: &str) -> Option<&AdminConfig> {
        self.registry.get(table).map(|r| &r.model)
    }
}

/// Gap 107 — join an admin sub-path with the configured base.
///
/// `route("/login", "/admin")` → `"/admin/login"`. Used at routes()
/// construction so every `.route(...)` call honours the
/// `AdminPlugin::at()` override without hardcoding `/admin` anywhere.
/// An empty `sub` (the index page) returns the base path itself, so
/// `route("", "/admin")` → `"/admin"` and not `"/admin/"`.
fn route(sub: &str, base: &str) -> String {
    if sub.is_empty() {
        return base.to_string();
    }
    format!("{base}{sub}")
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

    fn static_dirs(&self) -> Vec<umbra::plugin::StaticDir> {
        // The admin ships its assets EMBEDDED (see `static_files()`), so it
        // works with zero config. This `static_dirs()` entry additionally
        // exposes the on-disk source so `collect_static` can gather the
        // admin's `admin.css` / `admin.js` into `<static_root>/admin/` for
        // CDN / disk serving. Both modes coexist: the embedded specific
        // route wins in-binary, the collected files serve when a deployment
        // customises `static_url` or fronts assets with a CDN.
        //
        // Because the embedded specific route shadows the pipeline in-binary,
        // live-editing admin.css won't hot-reload — acceptable for these
        // framework-internal assets.
        let source_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("assets");
        vec![umbra::plugin::StaticDir::new("admin", source_dir)]
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
        //
        // Gap 107: the configured `base_path` rides along with the
        // branding so templates and handlers read it from one place.
        let mut sealed_branding = self.branding.clone();
        sealed_branding.base_path = self.base_path.clone();
        let _ = branding::BRANDING.set(sealed_branding);

        // Final section list: developer-declared sections first
        // (preserving registration order), then an implicit
        // "Widgets" section at the end containing any legacy
        // `register_widget(...)` calls. Apps that exclusively
        // use the new `.dashboard_section(...)` API end up with
        // a clean sectioned dashboard; apps that only use the
        // legacy call see one un-sectioned grid like before;
        // mixed-mode apps see explicit sections first and a
        // catch-all at the bottom.
        let mut sections: Vec<WidgetSection> = self.dashboard_sections.clone();
        if !self.widget_catalog.is_empty() {
            sections
                .push(WidgetSection::new("Widgets").widgets(self.widget_catalog.iter().cloned()));
        }
        // Flat catalog — feeds the per-widget data API. Built by
        // flattening every section so a single lookup-by-key
        // works regardless of which section a widget lives in.
        let catalog: Vec<Widget> = sections
            .iter()
            .flat_map(|s| s.widgets.iter().cloned())
            .collect();

        let state = AdminState {
            registry: Arc::new(self.registry.clone()),
            widget_catalog: Arc::new(catalog),
            dashboard_sections: Arc::new(sections),
            dashboard_models: self.dashboard_models.clone(),
            dashboard_models_title: self.dashboard_models_title.clone(),
            dashboard_models_subtitle: self.dashboard_models_subtitle.clone(),
        };
        Router::new()
            // Login / logout (no auth required)
            .route(
                &route("/login", &self.base_path),
                axum::routing::get(login_get).post(login_post),
            )
            .route(
                &route("/logout", &self.base_path),
                axum::routing::get(logout_handler),
            )
            // Index + CRUD routes (all require staff session)
            .route(
                &route("", &self.base_path),
                axum::routing::get(handlers::list::index),
            )
            .route(
                &route("/", &self.base_path),
                axum::routing::get(handlers::list::index),
            )
            .route(
                &route("/{table}/", &self.base_path),
                axum::routing::get(handlers::list::list),
            )
            .route(
                &route("/{table}/new", &self.base_path),
                axum::routing::get(handlers::crud::new_form).post(handlers::crud::create),
            )
            .route(
                &route("/{table}/action", &self.base_path),
                post(handlers::actions::run_action),
            )
            // Phase 2: fragment-only rows endpoint (search/sort/filter/paginate)
            .route(
                &route("/{table}/rows", &self.base_path),
                axum::routing::get(handlers::list::rows_fragment),
            )
            // gaps2 #11 round 2: toggle a column's visibility on
            // the persisted per-table prefs.
            .route(
                &route("/{table}/columns/{column}/toggle", &self.base_path),
                post(handlers::list::toggle_column_visibility),
            )
            // Filter dialog fragment
            .route(
                &route("/{table}/filter-dialog", &self.base_path),
                axum::routing::get(handlers::list::filter_dialog_handler),
            )
            // Phase 2: new-record sheet (create mode)
            .route(
                &route("/{table}/new-sheet", &self.base_path),
                axum::routing::get(handlers::sheet::new_sheet),
            )
            // Phase 2: delete confirm dialog fragment
            .route(
                &route("/{table}/{id}/_confirm-delete", &self.base_path),
                axum::routing::get(handlers::sheet::confirm_delete_dialog),
            )
            // Phase 2: sheet fragments (preview + edit)
            .route(
                &route("/{table}/{id}/sheet", &self.base_path),
                axum::routing::get(handlers::sheet::preview_sheet),
            )
            .route(
                &route("/{table}/{id}/edit-sheet", &self.base_path),
                axum::routing::get(handlers::sheet::edit_sheet_handler),
            )
            .route(
                &route("/{table}/{id}", &self.base_path),
                axum::routing::get(handlers::crud::detail),
            )
            .route(
                &route("/{table}/{id}/edit", &self.base_path),
                axum::routing::get(handlers::crud::edit_form).post(handlers::crud::update),
            )
            // Phase 2: create via sheet (POST)
            .route(
                &route("/{table}/create", &self.base_path),
                axum::routing::post(handlers::sheet::sheet_create),
            )
            // Phase 2: DELETE method for HTMX delete button
            .route(
                &route("/{table}/{id}", &self.base_path),
                axum::routing::delete(handlers::crud::htmx_delete),
            )
            .route(
                &route("/{table}/{id}/delete", &self.base_path),
                post(handlers::crud::delete),
            )
            // Phase 3: per-key action dispatch
            .route(
                &route("/{table}/actions/{key}", &self.base_path),
                axum::routing::post(handlers::actions::dispatch_action),
            )
            // Phase 3: FK/M2M async picker endpoints
            .route(
                &route("/api/{table}/{field}/options/resolve", &self.base_path),
                axum::routing::get(handlers::fk_picker::fk_options_resolve),
            )
            .route(
                &route("/api/{table}/{field}/options", &self.base_path),
                axum::routing::get(handlers::fk_picker::fk_options),
            )
            // Phase 3: inline cell edit
            .route(
                &route("/{table}/{id}/cell/{field}/edit", &self.base_path),
                axum::routing::get(handlers::inline_edit::cell_edit_get),
            )
            .route(
                &route("/{table}/{id}/cell/{field}", &self.base_path),
                axum::routing::post(handlers::inline_edit::cell_edit_post),
            )
            // Password change for models with password_field set
            .route(
                &route("/{table}/{id}/change-password", &self.base_path),
                axum::routing::post(handlers::sheet::change_password_handler),
            )
            // Phase 4: user prefs
            .route(
                &route("/api/prefs", &self.base_path),
                axum::routing::get(handlers::prefs::get_prefs_handler)
                    .put(handlers::prefs::put_prefs_handler),
            )
            // Phase 4: audit history
            .route(
                &route("/{table}/{id}/history", &self.base_path),
                axum::routing::get(handlers::history::history_handler),
            )
            // Phase 4: dashboard
            .route(
                &route("/api/dashboard/catalog", &self.base_path),
                axum::routing::get(handlers::dashboard::dashboard_catalog),
            )
            .route(
                &route("/api/dashboard/layout", &self.base_path),
                axum::routing::get(handlers::dashboard::dashboard_layout_get)
                    .put(handlers::dashboard::dashboard_layout_put),
            )
            .route(
                &route("/api/dashboard/widgets/{key}/data", &self.base_path),
                axum::routing::get(handlers::dashboard::dashboard_widget_data),
            )
            // Phase 4: command palette fragment + global record search
            .route(
                &route("/api/palette", &self.base_path),
                axum::routing::get(handlers::palette::palette_fragment),
            )
            .route(
                &route("/api/palette/search", &self.base_path),
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
            RouteSpec::new(&route("", &self.base_path), g()),
            RouteSpec::new(&route("/", &self.base_path), g()),
            RouteSpec::new(&route("/login", &self.base_path), gp()),
            RouteSpec::new(&route("/logout", &self.base_path), g()),
            RouteSpec::new(&route("/{table}/", &self.base_path), g()),
            RouteSpec::new(&route("/{table}/new", &self.base_path), gp()),
            RouteSpec::new(&route("/{table}/action", &self.base_path), p()),
            RouteSpec::new(&route("/{table}/rows", &self.base_path), g()),
            RouteSpec::new(&route("/{table}/filter-dialog", &self.base_path), g()),
            RouteSpec::new(&route("/{table}/new-sheet", &self.base_path), g()),
            RouteSpec::new(&route("/{table}/create", &self.base_path), p()),
            RouteSpec::new(&route("/{table}/{id}", &self.base_path), gpd()),
            RouteSpec::new(&route("/{table}/{id}/edit", &self.base_path), gp()),
            RouteSpec::new(&route("/{table}/{id}/edit-sheet", &self.base_path), g()),
            RouteSpec::new(&route("/{table}/{id}/sheet", &self.base_path), g()),
            RouteSpec::new(&route("/{table}/{id}/delete", &self.base_path), p()),
            RouteSpec::new(
                &route("/{table}/{id}/_confirm-delete", &self.base_path),
                g(),
            ),
            RouteSpec::new(&route("/{table}/{id}/history", &self.base_path), g()),
            RouteSpec::new(
                &route("/{table}/{id}/change-password", &self.base_path),
                p(),
            ),
            RouteSpec::new(&route("/{table}/{id}/cell/{field}", &self.base_path), p()),
            RouteSpec::new(
                &route("/{table}/{id}/cell/{field}/edit", &self.base_path),
                g(),
            ),
            RouteSpec::new(&route("/{table}/actions/{key}", &self.base_path), p()),
            RouteSpec::new(&route("/api/{table}/{field}/options", &self.base_path), g()),
            RouteSpec::new(
                &route("/api/{table}/{field}/options/resolve", &self.base_path),
                g(),
            ),
            RouteSpec::new(&route("/api/prefs", &self.base_path), gput()),
            RouteSpec::new(&route("/api/palette", &self.base_path), g()),
            RouteSpec::new(&route("/api/palette/search", &self.base_path), g()),
            RouteSpec::new(&route("/api/dashboard/catalog", &self.base_path), g()),
            RouteSpec::new(&route("/api/dashboard/layout", &self.base_path), gput()),
            RouteSpec::new(
                &route("/api/dashboard/widgets/{key}/data", &self.base_path),
                g(),
            ),
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

    #[test]
    fn static_files_use_unified_static_url() {
        // The embedded admin assets now mount on the unified `/static/admin/…`
        // pipeline URL (default `static_url`), not the legacy `/admin/static/…`.
        let files = AdminPlugin::default().static_files();
        let paths: Vec<&str> = files.iter().map(|f| f.url_path).collect();
        assert!(
            paths.contains(&"/static/admin/admin.css"),
            "admin.css should mount at /static/admin/admin.css, got {paths:?}"
        );
        assert!(
            paths.contains(&"/static/admin/admin.js"),
            "admin.js should mount at /static/admin/admin.js, got {paths:?}"
        );
        // Both still ship non-trivial embedded bytes (zero-config preserved).
        for f in &files {
            assert!(
                f.body.len() > 100,
                "{} should ship embedded bytes, got {} bytes",
                f.url_path,
                f.body.len()
            );
        }
    }

    #[test]
    fn static_dirs_maps_admin_namespace_to_existing_assets_dir() {
        let dirs = AdminPlugin::default().static_dirs();
        assert_eq!(dirs.len(), 1, "admin contributes exactly one static dir");
        let dir = &dirs[0];
        assert_eq!(dir.namespace, "admin");
        // The source dir actually exists on disk and holds the css/js the
        // embedded route serves — so `collect_static` has real files to gather.
        assert!(
            dir.source_dir.join("admin.css").is_file(),
            "{} should contain admin.css",
            dir.source_dir.display()
        );
        assert!(
            dir.source_dir.join("admin.js").is_file(),
            "{} should contain admin.js",
            dir.source_dir.display()
        );
    }
}
