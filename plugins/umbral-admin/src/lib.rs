//! umbral-admin — auto-generated CRUD admin for umbral models.
//!
//! Drop-in admin interface for any umbral project. Register the
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
//! credentials via [`umbral_auth::authenticate`], creates a session via
//! [`umbral_sessions::login`], then redirects to `next`.
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
mod views;
pub mod widgets;

mod auth;
pub mod branding;
mod discovery;
mod engine;
mod error;
mod handlers;
mod inlines;
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
    AdminModel, InlineKind, InlineModel, ToastLevel,
};
pub use registry::{AdminRegistration, AdminRegistry, App as AdminApp};
// The two builtin dashboard widgets — `Models by Plugin` (bar)
// and `Recent Signups` (feed). Used to be auto-prepended to the
// catalog; now exposed as public functions so the caller can
// register them at the position they want and resize via
// `.with_span(cols, rows)`. See `AdminPlugin::register_widget`
// for the wiring shape.
pub use handlers::dashboard::{builtin_recent_users_widget, builtin_total_models_widget};
pub use views::AdminView;
pub use widgets::{
    BarPayload, CardPayload, CatalogEntry, ChartPoint, DonutPayload, DonutSlice, FeedItem,
    FeedPayload, FilterOption, HeatmapCell, HeatmapPayload, HeatmapRow, KpiPayload, LinePayload,
    ProgressItem, ProgressPayload, RadialPayload, RadialTrack, Series, Span, TableColumn,
    TablePayload, Widget, WidgetDataFn, WidgetFilter, WidgetFilterKind, WidgetInstance, WidgetKind,
    WidgetParams, WidgetPayload, WidgetSection, format_thousands, humanize_number,
};

use std::sync::Arc;

use umbral::prelude::*;
use umbral::web::post;

// =========================================================================
// Plugin struct
// =========================================================================

/// The plugin. Mounts every admin route under `/admin`.
///
/// Use [`AdminPlugin::register`] to attach an [`AdminModel`] before
/// passing the plugin to `App::builder().plugin(...)`.
///
/// ```ignore
/// use umbral_admin::{AdminPlugin, AdminModel, Action};
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
    /// gaps2 #33 — "restore where I left off" feature flag. Default
    /// `true` (on by default; opt out to disable).
    /// When `true`: `/admin/` 302-redirects to `last_path` if one is
    /// stored; the changelist handler writes `last_path` on every visit;
    /// the "Home" breadcrumb carries `?dashboard=1` as an escape hatch.
    /// When `false`: `/admin/` always renders the dashboard; the
    /// changelist handler skips the `last_path` write (no dead data).
    restore_last_path: bool,
    /// Developer-registered custom views (widget pages at arbitrary paths).
    custom_views: Vec<AdminView>,
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
            restore_last_path: true,
            custom_views: Vec::new(),
        }
    }
}

impl AdminPlugin {
    /// Register an [`AdminModel`] for one model. Chainable.
    ///
    /// If two configs are registered for the same table the last one wins
    /// (a duplicate registration overwrites the earlier one).
    ///
    /// The plugin name defaults to `"admin"` for models registered before
    /// the plugin is installed into the app. From M7+ plugins will pass
    /// their own name via `Plugin::admin_register` on the registry.
    pub fn register(mut self, model: AdminModel) -> Self {
        self.registry.register("admin", model);
        self
    }

    /// Register many [`AdminModel`]s at once — the batch form of
    /// [`register`](Self::register). Lets each plugin export a
    /// `Vec<AdminModel>` (its admin surface, declared next to its models)
    /// and the app register them in one call instead of a `.register(...)`
    /// per model in `main.rs`.
    ///
    /// ```ignore
    /// // plugins/blog/src/lib.rs
    /// pub fn admin_models() -> Vec<umbral_admin::AdminModel> {
    ///     vec![post_admin(), comment_admin(), tag_admin()]
    /// }
    ///
    /// // main.rs
    /// AdminPlugin::default().register_many(blog::admin_models())
    /// ```
    pub fn register_many(mut self, models: impl IntoIterator<Item = AdminModel>) -> Self {
        for model in models {
            self = self.register(model);
        }
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

    /// Batch form of [`register_for`](Self::register_for) — register many
    /// models under one plugin name (the `Plugin`-pathway batch entry).
    pub fn register_for_many(
        mut self,
        plugin_name: &str,
        models: impl IntoIterator<Item = AdminModel>,
    ) -> Self {
        for model in models {
            self = self.register_for(plugin_name, model);
        }
        self
    }

    /// Register a dashboard widget. Chainable.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use umbral_admin::{AdminPlugin, Widget, WidgetKind, WidgetDataFn, WidgetPayload, KpiPayload, Span};
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

    /// Show or hide the version string in the admin sidebar and on the login page
    /// (gaps3 #67).
    ///
    /// ```ignore
    /// AdminPlugin::default().show_version(false)
    /// ```
    ///
    /// On by default, showing umbral's own version. Turning it off is reasonable: an
    /// admin is a private surface, and telling every visitor which framework version you
    /// run is free reconnaissance for anyone matching it against a CVE list.
    /// The resolved branding, for tests. Not part of the stable surface.
    #[doc(hidden)]
    pub fn branding_for_tests(&self) -> &branding::AdminBranding {
        &self.branding
    }

    pub fn show_version(mut self, show: bool) -> Self {
        self.branding.version_label = if show {
            Some(
                self.branding
                    .version_label
                    .unwrap_or_else(crate::branding::umbral_version_label),
            )
        } else {
            None
        };
        self
    }

    /// Show YOUR version instead of umbral's (gaps3 #67).
    ///
    /// ```ignore
    /// AdminPlugin::default().version(concat!("MyShop v", env!("CARGO_PKG_VERSION")))
    /// ```
    ///
    /// The default advertises the framework — which is what the operator of a shop
    /// almost certainly does NOT want on their staff login page. Whose version an admin
    /// shows is a product decision, so it is yours to make. Implies `show_version(true)`.
    ///
    /// Prefer `env!("CARGO_PKG_VERSION")` over a literal: a hardcoded version is a lie
    /// waiting for the next release, which is exactly how the admin came to claim
    /// `v0.0.1` five releases after it stopped being true.
    pub fn version(mut self, label: impl Into<String>) -> Self {
        self.branding.version_label = Some(label.into());
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
    /// `/admin`. Useful when a single domain hosts multiple umbral
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
    ///           .widget(umbral_admin::builtin_recent_users_widget()))
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

    /// Control whether the admin "restore where I left off" feature is
    /// active (default: **`true`** — on by default, opt out to disable).
    ///
    /// When enabled (`true`, the default):
    /// - `/admin/` 302-redirects to the last-visited changelist URL
    ///   stored in `admin_user_pref.preferences.last_path`.
    /// - The changelist handler writes `last_path` on every page visit.
    /// - The "Home" breadcrumb carries `?dashboard=1` so the dashboard
    ///   is reachable in one click (the escape hatch becomes a UI affordance).
    ///
    /// When disabled (`false`):
    /// - `/admin/` always renders the dashboard directly.
    /// - The changelist handler skips the `last_path` write — no dead
    ///   data accumulates in `admin_user_pref.preferences`.
    ///
    /// ```ignore
    /// AdminPlugin::default().restore_last_path(false)
    /// ```
    pub fn restore_last_path(mut self, enabled: bool) -> Self {
        self.restore_last_path = enabled;
        self
    }

    /// Register a custom admin view — a widget page mounted at
    /// `{admin_base}/{view.path}`. Chainable.
    ///
    /// ```ignore
    /// AdminPlugin::default().view(
    ///     AdminView::new("reports/sales", "Sales report")
    ///         .with_icon("bar-chart")
    ///         .section(WidgetSection::new("This month").widget(revenue_kpi())),
    /// )
    /// ```
    pub fn view(mut self, view: AdminView) -> Self {
        self.custom_views.push(view);
        self
    }

    /// Batch form of [`view`](Self::view).
    pub fn views(mut self, views: impl IntoIterator<Item = AdminView>) -> Self {
        self.custom_views.extend(views);
        self
    }

    /// gaps3 #7 — custom views whose path is safe to mount, with the rest
    /// dropped (and logged). Two views registered at the same path would
    /// make axum's router `panic!` on a route conflict at boot; rejecting
    /// the duplicate here turns that into a clear `tracing::error!` and
    /// keeps the rest of the admin serving (the rejected view is absent
    /// from the router AND the sidebar, both of which read this list).
    ///
    /// Views mount under the dedicated `/custom-views/` URL namespace (see
    /// `Plugin::routes`), which is hyphenated and therefore can never be a
    /// model table name (tables are snake_case) — so a view can NOT collide
    /// with a built-in admin route or shadow a changelist. That's why the
    /// only checks here are empty-path and duplicate-path.
    fn resolved_custom_views(&self) -> Vec<AdminView> {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(self.custom_views.len());
        for v in &self.custom_views {
            let path = v.path();
            if path.is_empty() {
                tracing::error!(title = v.title(), "admin custom view rejected: empty path");
                continue;
            }
            if !seen.insert(path) {
                tracing::error!(path, "admin custom view rejected: duplicate path");
                continue;
            }
            out.push(v.clone());
        }
        out
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
    /// gaps2 #33 — mirrors `AdminPlugin::restore_last_path`. The index
    /// handler reads this to decide whether to redirect; the list handler
    /// reads it to decide whether to write `last_path`.
    restore_last_path: bool,
    /// Developer-registered custom views, for the page handler + sidebar.
    custom_views: Arc<Vec<AdminView>>,
    /// Gate map: widget key → view permission codename, for every widget
    /// that belongs to a `.with_permission()`-gated custom view.
    ///
    /// Built at `routes()` time from the custom-view registration list and
    /// checked by `dashboard_widget_data` after `require_staff` — if the
    /// widget's key is present, the requesting user must also hold the mapped
    /// codename, or the endpoint returns 403.
    ///
    /// Dashboard widgets and widgets in ungated views are NOT in this map,
    /// so the gate only applies to views that explicitly opt in via
    /// `.with_permission(...)`.
    widget_gates: Arc<std::collections::HashMap<String, String>>,
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
        // Auth is required: login verifies credentials via umbral-auth.
        // Sessions is required: login creates sessions.
        &["auth", "sessions"]
    }

    fn static_files(&self) -> Vec<umbral::plugin::StaticFile> {
        admin_static_files()
    }

    fn static_dirs(&self) -> Vec<umbral::plugin::StaticDir> {
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
        vec![umbral::plugin::StaticDir::new("admin", source_dir)]
    }

    fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
        vec![
            umbral::migrate::ModelMeta::for_::<crate::models::AdminUserPref>(),
            umbral::migrate::ModelMeta::for_::<crate::models::AdminAuditLog>(),
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
        // gaps2 #33: `restore_last_path` joins the branding cell so
        // templates can query the flag (e.g. to emit `?dashboard=1`
        // on the "Home" breadcrumb link) without a handler pass-through.
        let mut sealed_branding = self.branding.clone();
        sealed_branding.base_path = self.base_path.clone();
        sealed_branding.restore_last_path = self.restore_last_path;
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
        let mut catalog: Vec<Widget> = sections
            .iter()
            .flat_map(|s| s.widgets.iter().cloned())
            .collect();

        // Custom-view widgets join the same flat catalog so the per-key
        // data endpoint resolves them unchanged. Keys are global → warn on dups.
        // The gate map is built alongside the catalog: for every widget in a
        // permission-gated view (`.with_permission(codename)`), record
        // `widget_key → codename` so `dashboard_widget_data` can enforce the
        // same codename check on the API call, not just on the page load.
        // gaps3 #7: validate custom-view paths ONCE, up front. Everything
        // downstream (widget flatten, gate map, sidebar state, route mount)
        // reads this resolved list so a rejected view is absent everywhere.
        let resolved_views = self.resolved_custom_views();
        let mut seen_keys: std::collections::HashSet<&str> =
            catalog.iter().map(|w| w.key).collect();
        let mut widget_gates: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for v in &resolved_views {
            for w in v.sections().iter().flat_map(|s| s.widgets.iter()) {
                if !seen_keys.insert(w.key) {
                    tracing::warn!(
                        widget_key = w.key,
                        view = v.path(),
                        "duplicate widget key across dashboard/custom views; \
                         the data endpoint resolves the first match"
                    );
                }
                catalog.push(w.clone());
                // Only gated views contribute to the gate map.
                if let Some(perm) = v.permission() {
                    widget_gates.insert(w.key.to_string(), perm.to_string());
                }
            }
        }

        let state = AdminState {
            registry: Arc::new(self.registry.clone()),
            widget_catalog: Arc::new(catalog),
            dashboard_sections: Arc::new(sections),
            dashboard_models: self.dashboard_models.clone(),
            dashboard_models_title: self.dashboard_models_title.clone(),
            dashboard_models_subtitle: self.dashboard_models_subtitle.clone(),
            restore_last_path: self.restore_last_path,
            custom_views: Arc::new(resolved_views.clone()),
            widget_gates: Arc::new(widget_gates),
        };
        let mut router = Router::new()
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
            // CSV export of a widget's own payload, computed from the SAME
            // resolved filters the dashboard is showing — so the file matches
            // the chart you exported it from. Shares `gate_widget` with the data
            // endpoint, so it cannot become a way to read numbers you are not
            // allowed to see.
            .route(
                &route("/api/dashboard/widgets/{key}/export.csv", &self.base_path),
                axum::routing::get(handlers::dashboard::dashboard_widget_export),
            )
            // gaps2 #36: EasyMDE markdown-editor image upload. Staff-gated
            // (no `{table}` — a media upload isn't scoped to one model), and
            // stores through the ambient `umbral::storage` seam. Returns
            // `{ "url": ... }` for the editor's `imageUploadFunction`.
            .route(
                &route("/upload-image", &self.base_path),
                post(handlers::upload::upload_image),
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
            ;
        // Mount one GET route per registered custom view at
        // `{base}/{view.path}`. The per-invocation `slug` clone keeps the
        // handler `Clone` (axum requires the handler future factory to be
        // cloneable across concurrent requests).
        for v in &resolved_views {
            let slug = v.path().to_string();
            let full = route(&format!("/custom-views/{}/", v.path()), &self.base_path);
            router = router.route(
                &full,
                axum::routing::get({
                    let slug = slug.clone();
                    move |state: axum::extract::State<AdminState>,
                          headers: axum::http::HeaderMap| {
                        let slug = slug.clone();
                        async move {
                            crate::handlers::custom_view::custom_view(state, headers, slug).await
                        }
                    }
                }),
            );
        }
        router.with_state(state)
    }

    fn route_paths(&self) -> Vec<umbral::routes::RouteSpec> {
        // Companion list to `routes()` — surfaced by the dev-mode
        // default 404 page. Each entry pairs a path pattern with the
        // HTTP methods it accepts; keep in sync with the `.route(...)`
        // calls above. Mismatch is "stale route list," not a routing
        // bug.
        use umbral::routes::RouteSpec;
        // Method shorthands — each constructed once and `clone()`d per
        // entry so the source list stays one-line-per-route.
        let g = || vec!["GET"];
        let p = || vec!["POST"];
        let gp = || vec!["GET", "POST"];
        let gpd = || vec!["GET", "POST", "DELETE"];
        let gput = || vec!["GET", "PUT"];
        let mut specs = vec![
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
            RouteSpec::new(&route("/upload-image", &self.base_path), p()),
            RouteSpec::new(&route("/api/palette", &self.base_path), g()),
            RouteSpec::new(&route("/api/palette/search", &self.base_path), g()),
            RouteSpec::new(&route("/api/dashboard/catalog", &self.base_path), g()),
            RouteSpec::new(&route("/api/dashboard/layout", &self.base_path), gput()),
            RouteSpec::new(
                &route("/api/dashboard/widgets/{key}/data", &self.base_path),
                g(),
            ),
        ];
        // Companion entries for the developer-registered custom views,
        // mounted in `routes()` as `GET {base}/{view.path}`.
        for v in &self.resolved_custom_views() {
            specs.push(RouteSpec::new(
                &format!("{}/custom-views/{}/", self.base_path, v.path()),
                g(),
            ));
        }
        specs
    }

    fn on_ready(
        &self,
        _ctx: &umbral::plugin::AppContext,
    ) -> Result<(), umbral::plugin::PluginError> {
        // Tables are produced by the migration engine off
        // `Self::models()` — same path as every other plugin's models.
        // No bootstrap DDL here.

        // CSRF posture (audit_2 admin #5). The admin's mutating handlers
        // (create / update / delete / bulk-action / inline-edit / upload /
        // prefs) do NOT self-verify a CSRF token — only `login_post` does — so
        // cross-site request forgery is defended by the session cookie's
        // `SameSite` attribute. `SameSite=Lax` (the default) already blocks the
        // forged cross-site POST/PUT/DELETE that would carry the session
        // cookie. If an operator sets `SameSite=None` (e.g. to serve a
        // cross-origin SPA) that defense is gone, so the admin's mutations are
        // CSRF-forgeable unless a CSRF middleware is mounted. `on_ready` runs in
        // topological order and the admin depends on `sessions`, so the sealed
        // value is readable here. Warn loudly rather than fail (a cross-origin
        // API with a properly-mounted CSRF layer is a legitimate setup).
        if umbral_sessions::configured_same_site() == umbral_sessions::SameSite::None {
            tracing::warn!(
                "umbral-admin: the session cookie is SameSite=None, which removes the \
                 cross-site-request CSRF defense the admin's mutating handlers rely on. \
                 Mount a CSRF middleware (umbral-security's SecurityPlugin) so admin \
                 create/update/delete/upload/prefs actions can't be forged cross-site, \
                 or keep the session cookie at SameSite=Lax/Strict for same-origin admin use."
            );
        }
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

#[cfg(test)]
mod custom_view_wiring_tests {
    use super::*;
    use crate::views::AdminView;
    use crate::widgets::{
        KpiPayload, Widget, WidgetDataFn, WidgetKind, WidgetPayload, WidgetSection,
    };

    fn tiny_kpi(key: &'static str) -> Widget {
        Widget {
            key,
            title: "T".into(),
            kind: WidgetKind::Kpi,
            default_span: Default::default(),
            permission: None,
            data: WidgetDataFn::new(|_user| async {
                WidgetPayload::Kpi(KpiPayload {
                    value: "0".into(),
                    unit: None,
                    delta: None,
                    sparkline: None,
                })
            }),
            default_period: None,
            filters: Vec::new(),
        }
    }

    // gaps3 #7 — a DUPLICATE view path is dropped (logged), not panicked.
    // Views mount under the /custom-views/ namespace, so a path that looks
    // like a built-in route ("login") or a table can't collide — only an
    // exact duplicate path would make axum's router panic at boot.
    #[test]
    fn resolved_custom_views_drops_duplicate_paths() {
        let plugin = AdminPlugin::default()
            .view(AdminView::new("reports/sales", "A"))
            .view(AdminView::new("reports/sales", "dup B")) // duplicate → dropped
            .view(AdminView::new("login", "safe under the namespace")) // /custom-views/login/ — no collision
            .view(AdminView::new("reports/ok", "C")); // valid, distinct

        let resolved = plugin.resolved_custom_views();
        let paths: Vec<&str> = resolved.iter().map(|v| v.path()).collect();
        assert_eq!(
            paths,
            vec!["reports/sales", "login", "reports/ok"],
            "only the exact duplicate is dropped (first wins); a 'login' path is fine under /custom-views/"
        );

        // The real regression: routes() must not panic on the duplicate
        // registration now that the resolver drops it first.
        let _router = plugin.routes();
    }

    #[test]
    fn view_registers_and_flattens_widgets_into_catalog() {
        let plugin = AdminPlugin::default().view(
            AdminView::new("reports/sales", "Sales")
                .section(WidgetSection::new("S").widget(tiny_kpi("rpt_sales_total"))),
        );
        // The view is stored on the plugin.
        assert_eq!(plugin.custom_views.len(), 1);
        assert_eq!(plugin.custom_views[0].path(), "reports/sales");

        // The same flatten the `routes()` builder performs: a registered
        // view's widgets become reachable in the global key catalog so the
        // per-key data endpoint resolves them unchanged.
        let catalog_keys: Vec<&str> = plugin
            .custom_views
            .iter()
            .flat_map(|v| v.sections().iter())
            .flat_map(|s| s.widgets.iter())
            .map(|w| w.key)
            .collect();
        assert!(
            catalog_keys.contains(&"rpt_sales_total"),
            "the view's widget key should be flattenable into the catalog, got {catalog_keys:?}"
        );
    }
}
