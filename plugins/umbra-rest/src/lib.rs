//! umbra-rest — auto-generated JSON REST API over umbra models.
//!
//! Register [`RestPlugin`] on `App::builder()` and every registered
//! model gets a standard REST surface at `/api/<table>/`:
//!
//! - `GET /api/<table>/`         — list (envelope shape is configurable via
//!   [`RestPlugin::paginate`]; defaults to `{"results": [...], "count": N}`)
//! - `POST /api/<table>/`        — create, returns 201 + the new row
//! - `GET /api/<table>/<id>`     — retrieve, 404 on miss
//! - `PUT /api/<table>/<id>`     — update (full replacement), returns 200 + row
//! - `PATCH /api/<table>/<id>`   — partial update, returns 200 + row
//! - `DELETE /api/<table>/<id>`  — destroy, returns 204
//!
//! Same data, plain JSON. Per-column dispatch on the M3 `SqlType`
//! catalogue: integers / floats / bool / text / date / time /
//! timestamptz / uuid, plus nullable forms.
//!
//! ## Exposure
//!
//! By default the plugin auto-exposes every registered model except
//! the three known-internal tables: `auth_user`, `session`, and
//! `umbra_migrations`. Letting `/api/auth_user/` exist would leak
//! password hashes; the default block-list is the safe shape.
//!
//! Tighten with `RestPlugin::new().include_only(["article"])` or
//! loosen with `.exclude(["sensitive_thing"])`. The builder is
//! chainable.
//!
//! ## Auth
//!
//! v1 ships no built-in auth gate — every exposed route is open.
//! Apps that need authenticated CRUD wrap the umbra-rest router
//! with a tower layer (or write their own handler that delegates
//! after the auth check). A future round adds optional
//! `RestPlugin::require_staff()` that mirrors umbra-admin's Basic
//! Auth gate.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde::Serialize;
use serde_json::{Map, Value};
use umbra::migrate::ModelMeta;
use umbra::prelude::*;
use umbra::web::{Json, Path, Query, Response, StatusCode};

pub mod filtering;
pub(crate) use filtering::{FilterClause, parse_filters, parse_search};

pub mod pagination;
pub use pagination::{
    LimitOffsetPagination, NoPagination, PageNumberPagination, PageRequest, Pagination,
};

pub mod resource;
pub use resource::{ActionContext, ActionError, ActionScope, ResourceConfig};

pub mod auth;
pub use auth::{
    Authentication, ChainAuthentication, FnAuthentication, Identity, NoAuthentication,
    parse_basic_credentials,
};

pub mod permission;
pub use permission::{
    Action, AllowAny, AndPermission, IsAuthenticated, IsStaff, OrPermission, Permission,
    PermissionError, ReadOnly,
};

/// The block-list every plugin starts with. Exposing these via REST
/// would leak password hashes (auth_user), session IDs (session), or
/// the migration tracking table itself.
const DEFAULT_BLOCKED_TABLES: &[&str] = &["auth_user", "session", "umbra_migrations"];

/// Closure that transforms one field's JSON value to another. Used
/// by [`RestPlugin::transform`]. The signature is `&Value -> Value`
/// — the field's current value goes in, the replacement comes out.
/// `pub(crate)` so [`crate::resource::ResourceConfig`] can store them
/// in its own per-table vec before they're folded into the plugin.
pub(crate) type TransformFn = std::sync::Arc<dyn Fn(&Value) -> Value + Send + Sync + 'static>;

/// Closure that computes a derived field from the whole row. Used by
/// [`RestPlugin::computed`]. The signature is `&Map -> Value` — the
/// closure sees every present field (including computed ones added
/// earlier in the chain) and returns the value for the new key.
pub(crate) type ComputedFn =
    std::sync::Arc<dyn Fn(&Map<String, Value>) -> Value + Send + Sync + 'static>;

/// The plugin. Mounts the REST routes at `/api`.
///
/// Field-level customisation is configured at builder time and applied
/// to every outgoing JSON response (the list / retrieve / create /
/// update payloads). See [`Self::hide`], [`Self::transform`], and
/// [`Self::computed`].
#[derive(Clone)]
pub struct RestPlugin {
    include_only: Option<Vec<String>>,
    extra_exclude: Vec<String>,
    /// Tables that override the `DEFAULT_BLOCKED_TABLES` security
    /// default. Populated via `RestPlugin::expose([...])`. A name
    /// here is served via the standard CRUD endpoints even though
    /// it's normally blocked (the framework's auth_user / session /
    /// migration tables). The user explicitly opts in per-table —
    /// no implicit "expose everything" mode.
    expose: std::collections::HashSet<String>,
    /// `(table, field)` pairs that are stripped from response bodies.
    hidden: Vec<(String, String)>,
    /// `(table, field, transform_fn)` — replaces a field's value.
    transforms: Vec<(String, String, TransformFn)>,
    /// `(table, name, compute_fn)` — adds a derived field per row.
    /// Applied AFTER hide + transform, so the computed closure sees
    /// the customised row.
    computed: Vec<(String, String, ComputedFn)>,
    /// The pagination shape applied to every list endpoint. Defaults
    /// to [`NoPagination`] so the v1 envelope (`{ results, count }`)
    /// stays unchanged for apps that don't opt in. Configure via
    /// [`Self::paginate`].
    pagination: Arc<dyn Pagination>,
    /// The authentication backend run on every request before the
    /// permission check. Defaults to [`NoAuthentication`] — every
    /// request looks anonymous. Configure via
    /// [`Self::authenticate`].
    authentication: Arc<dyn Authentication>,
    /// Per-table permission classes, keyed by table name. Populated
    /// when a [`ResourceConfig`] with `.permission(...)` is merged
    /// via [`Self::resource`]. Tables without an entry default to
    /// [`AllowAny`].
    permissions: HashMap<String, Arc<dyn Permission>>,
    /// Per-table opt-in view scope, keyed by table name. `None` (no
    /// entry) means "all actions exposed" — backward-compatible.
    /// `Some(set)` restricts the table to exactly that set of
    /// actions; everything else returns 404 from the handler.
    view_scope: HashMap<String, std::collections::HashSet<Action>>,
    /// Per-table `@action` definitions, keyed by table name. The
    /// `RestPlugin::routes` walk mounts one axum route per entry,
    /// and the dispatch handler looks the (table, action_name)
    /// lookup back out at request time.
    actions: HashMap<String, Vec<crate::resource::ActionDef>>,
    /// Tables that have opted OUT of query-string filtering via
    /// `ResourceConfig::disable_filters()` or
    /// `RestPlugin::disable_filters_for(&[...])`. Filtering is the
    /// default for every exposed table; this set is the
    /// per-resource opt-out list.
    filters_disabled: std::collections::HashSet<String>,
    /// Tables that have opted OUT of `?search=` free-text search.
    /// Search is ON by default; this set is the per-table opt-out.
    search_disabled: std::collections::HashSet<String>,
    /// Per-table allow-list of column names that participate in
    /// `?search=`. Tables not in the map default to "every
    /// searchable column" (see `filtering::parse_search`).
    search_fields: HashMap<String, Vec<String>>,
}

impl std::fmt::Debug for RestPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Closures + trait objects aren't Debug; render placeholders
        // for the dynamic fields so the Debug impl still works for
        // tests / logs.
        f.debug_struct("RestPlugin")
            .field("include_only", &self.include_only)
            .field("extra_exclude", &self.extra_exclude)
            .field("hidden", &self.hidden)
            .field("transforms_count", &self.transforms.len())
            .field("computed_count", &self.computed.len())
            .field("pagination", &"<dyn Pagination>")
            .field("filters_disabled", &self.filters_disabled)
            .finish()
    }
}

impl Default for RestPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl RestPlugin {
    /// Resolve the permission class for a table, defaulting to
    /// [`AllowAny`] if no `ResourceConfig::permission(...)` was set.
    fn permission_for(&self, table: &str) -> Arc<dyn Permission> {
        self.permissions
            .get(table)
            .cloned()
            .unwrap_or_else(|| Arc::new(AllowAny))
    }

    /// True when this action is mounted for this table. Tables
    /// without an explicit `.views(...)` scope expose every action
    /// (backward-compatible default). Tables with a scope expose
    /// exactly the actions in the set.
    ///
    /// Custom actions are NOT subject to `view_scope` — they're
    /// opt-in by being registered at all, and the scope only filters
    /// the five built-in CRUD actions.
    fn view_exposed(&self, table: &str, action: &Action) -> bool {
        if matches!(action, Action::Custom(_)) {
            return true;
        }
        match self.view_scope.get(table) {
            Some(scope) => scope.contains(action),
            None => true,
        }
    }

    /// Authenticate + permission-check for one (table, action). The
    /// caller passes the resolved identity (already pulled from the
    /// auth backend at request entry). Returns the right `ApiError`
    /// variant for the failure mode so the handler's `?` operator
    /// surfaces 401 / 403 / 404 with the right shape.
    fn gate(
        &self,
        table: &str,
        action: &Action,
        identity: Option<&Identity>,
    ) -> Result<(), ApiError> {
        if !self.view_exposed(table, action) {
            return Err(ApiError::NotFound(format!(
                "action `{action:?}` is not exposed on `/api/{table}/`"
            )));
        }
        match self.permission_for(table).check(action, identity) {
            Ok(()) => Ok(()),
            Err(PermissionError::Unauthenticated) => Err(ApiError::Unauthenticated),
            Err(PermissionError::Forbidden) => Err(ApiError::Forbidden),
        }
    }
}

impl RestPlugin {
    pub fn new() -> Self {
        Self {
            include_only: None,
            extra_exclude: Vec::new(),
            expose: std::collections::HashSet::new(),
            hidden: Vec::new(),
            transforms: Vec::new(),
            computed: Vec::new(),
            pagination: Arc::new(NoPagination),
            authentication: Arc::new(NoAuthentication),
            permissions: HashMap::new(),
            view_scope: HashMap::new(),
            actions: HashMap::new(),
            filters_disabled: std::collections::HashSet::new(),
            search_disabled: std::collections::HashSet::new(),
            search_fields: HashMap::new(),
        }
    }

    /// Set the authentication backend run on every request. Default
    /// is [`NoAuthentication`]; opt in with one of the built-ins or
    /// supply a [`FnAuthentication`] / [`ChainAuthentication`].
    ///
    /// Resource-level permissions ([`ResourceConfig::permission`])
    /// see the `Option<Identity>` this produces.
    ///
    /// ```ignore
    /// RestPlugin::default()
    ///     .authenticate(FnAuthentication::new(|headers| async move {
    ///         let user = umbra_sessions::current_user(&headers).await.ok().flatten()?;
    ///         Some(Identity::user(user.id).with_staff(user.is_staff))
    ///     }))
    /// ```
    pub fn authenticate<A: Authentication>(mut self, auth: A) -> Self {
        self.authentication = Arc::new(auth);
        self
    }

    /// Set the pagination shape applied to every list endpoint.
    ///
    /// Three built-ins ship:
    /// - [`NoPagination`] (default) — `{ results, count }` envelope,
    ///   no LIMIT applied, no extra COUNT query.
    /// - [`PageNumberPagination::new(page_size)`] — Django default.
    ///   `?page=N&page_size=M`.
    /// - [`LimitOffsetPagination::new(default_limit)`] — REST classic.
    ///   `?limit=N&offset=M`.
    ///
    /// Custom envelopes: implement [`Pagination`] on a unit struct or
    /// configured type and pass it here. See the trait docs for the
    /// extract + paginate contract.
    ///
    /// ```ignore
    /// RestPlugin::default()
    ///     .paginate(PageNumberPagination::new(20).with_max_page_size(100))
    /// ```
    pub fn paginate<P: Pagination>(mut self, p: P) -> Self {
        self.pagination = Arc::new(p);
        self
    }

    /// Restrict exposure to exactly this set of tables. Every other
    /// model registered with the framework is hidden, including any
    /// not on the default block-list.
    pub fn include_only<I, S>(mut self, tables: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.include_only = Some(tables.into_iter().map(Into::into).collect());
        self
    }

    /// Add tables to the block-list. Defaults still apply.
    pub fn exclude<I, S>(mut self, tables: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for t in tables {
            self.extra_exclude.push(t.into());
        }
        self
    }

    /// Opt INTO exposing tables that are normally blocked for
    /// security reasons (`auth_user`, `session`, `umbra_migrations`).
    ///
    /// ```ignore
    /// // I want the admin's user list and session table reachable
    /// // through the REST API too, knowing what I'm signing up for.
    /// RestPlugin::default()
    ///     .expose(["auth_user", "session"])
    ///     .resource(
    ///         // ...and hide password_hash from the wire.
    ///         ResourceConfig::new("auth_user").hide("password_hash"),
    ///     )
    /// ```
    ///
    /// ## Security note
    ///
    /// `auth_user` and `session` are blocked by default because they
    /// carry credentials/secrets the framework doesn't want a careless
    /// `RestPlugin::default()` to leak — `password_hash` over the wire
    /// to anyone hitting `GET /api/auth_user/`, session tokens over
    /// `GET /api/session/`. When you expose them:
    ///
    /// - Pair with `ResourceConfig::hide("password_hash")` so that
    ///   column never appears in list/retrieve responses.
    /// - Pair with `ResourceConfig::permission(...)` so the endpoints
    ///   are gated behind staff-only authorisation.
    ///
    /// Composes with `include_only` (an `include_only` allow-list
    /// takes precedence — if `auth_user` isn't on it, expose is
    /// moot) and with `exclude` (an exposed table that's also in
    /// `extra_exclude` stays blocked — explicit user "no" beats
    /// explicit user "yes" for the same table).
    pub fn expose<I, S>(mut self, tables: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for t in tables {
            self.expose.insert(t.into());
        }
        self
    }

    /// Register a [`ResourceConfig`] — every `hide` / `transform` /
    /// `computed` it carries is folded into the plugin under the
    /// resource's table name.
    ///
    /// Lets per-model REST customization live next to the model
    /// (in a plugin crate, a module, a free function) rather than
    /// at the `RestPlugin::default()` site in `main.rs`. The
    /// per-call builders (`.hide` / `.transform` / `.computed`) still
    /// work for one-off cases.
    ///
    /// ```ignore
    /// // plugins/users/src/lib.rs
    /// pub fn rest_resource() -> umbra_rest::ResourceConfig {
    ///     umbra_rest::ResourceConfig::new("user")
    ///         .hide("password_hash")
    ///         .transform("email", mask_email)
    /// }
    ///
    /// // main.rs
    /// RestPlugin::default()
    ///     .resource(users::rest_resource())
    /// ```
    ///
    /// Calling `.resource(...)` multiple times for the SAME table is
    /// supported and additive — each call appends, doesn't replace.
    pub fn resource(mut self, config: ResourceConfig) -> Self {
        let ResourceConfig {
            table,
            hidden,
            transforms,
            computed,
            permission,
            view_scope,
            actions,
            filters_disabled,
            search_disabled,
            search_fields,
        } = config;
        for field in hidden {
            self.hidden.push((table.clone(), field));
        }
        for (field, func) in transforms {
            self.transforms.push((table.clone(), field, func));
        }
        for (name, func) in computed {
            self.computed.push((table.clone(), name, func));
        }
        if let Some(perm) = permission {
            // Repeated `.resource(...)` calls for the same table
            // overwrite — last one wins. The alternative (storing a
            // Vec and AND-ing) would mean a Vec<Arc<dyn Permission>>
            // per table, which the AndPermission combinator already
            // covers explicitly on the user side.
            self.permissions.insert(table.clone(), perm);
        }
        if let Some(scope) = view_scope {
            self.view_scope.insert(table.clone(), scope);
        }
        if !actions.is_empty() {
            self.actions
                .entry(table.clone())
                .or_default()
                .extend(actions);
        }
        if filters_disabled {
            self.filters_disabled.insert(table.clone());
        }
        if search_disabled {
            self.search_disabled.insert(table.clone());
        }
        if let Some(fields) = search_fields {
            self.search_fields.insert(table.clone(), fields);
        }
        self
    }

    /// Disable query-string filtering for one or more tables.
    ///
    /// Filters are ON by default — every exposed list endpoint accepts
    /// the `<field>` / `<field>__<lookup>` grammar described on
    /// [`crate::ResourceConfig::disable_filters`]. This is the
    /// plugin-level shorthand for opting a batch of tables out
    /// without building a `ResourceConfig` for each one.
    ///
    /// ```ignore
    /// RestPlugin::default()
    ///     .disable_filters_for(["audit_log", "metric_sample"])
    /// ```
    pub fn disable_filters_for<I, S>(mut self, tables: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for table in tables {
            self.filters_disabled.insert(table.into());
        }
        self
    }

    /// Disable `?search=` free-text search for one or more tables.
    /// See [`crate::ResourceConfig::disable_search`] for the rationale.
    pub fn disable_search_for<I, S>(mut self, tables: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for table in tables {
            self.search_disabled.insert(table.into());
        }
        self
    }

    /// Strip a field from every REST response for the given table.
    /// The column is still readable through the ORM and writable via
    /// POST/PUT/PATCH — this only changes the outgoing JSON shape.
    ///
    /// Common case: hiding `password_hash` from the `user` table so
    /// it never reaches an API consumer.
    ///
    /// ```ignore
    /// RestPlugin::new().hide("user", "password_hash")
    /// ```
    pub fn hide(mut self, table: &str, field: &str) -> Self {
        self.hidden.push((table.to_string(), field.to_string()));
        self
    }

    /// Replace a field's value in every REST response. The closure
    /// receives the raw value and returns the replacement. The field
    /// stays at the same JSON key.
    ///
    /// Common case: masking sensitive data (`email` → `***@domain`)
    /// without removing the field entirely.
    ///
    /// ```ignore
    /// RestPlugin::new()
    ///     .transform("user", "email", |v| {
    ///         let s = v.as_str().unwrap_or("");
    ///         match s.split_once('@') {
    ///             Some((_, d)) => json!(format!("***@{d}")),
    ///             None => v.clone(),
    ///         }
    ///     })
    /// ```
    pub fn transform<F>(mut self, table: &str, field: &str, f: F) -> Self
    where
        F: Fn(&Value) -> Value + Send + Sync + 'static,
    {
        self.transforms
            .push((table.to_string(), field.to_string(), std::sync::Arc::new(f)));
        self
    }

    /// Add a derived field to every REST response. The closure
    /// receives the (already hide+transform-processed) row map and
    /// returns the value for the new key. The key has no underlying
    /// column — it exists only in the API surface.
    ///
    /// Common case: synthesising a `display_name` from `first_name` +
    /// `last_name` columns.
    ///
    /// ```ignore
    /// RestPlugin::new()
    ///     .computed("user", "display_name", |row| {
    ///         let f = row.get("first_name").and_then(|v| v.as_str()).unwrap_or("");
    ///         let l = row.get("last_name").and_then(|v| v.as_str()).unwrap_or("");
    ///         json!(format!("{f} {l}").trim())
    ///     })
    /// ```
    pub fn computed<F>(mut self, table: &str, name: &str, f: F) -> Self
    where
        F: Fn(&Map<String, Value>) -> Value + Send + Sync + 'static,
    {
        self.computed
            .push((table.to_string(), name.to_string(), std::sync::Arc::new(f)));
        self
    }

    /// Apply every configured override to a single row, in order:
    /// hide → transform → computed. Run after the handlers build the
    /// raw row map from the database; mutates in place.
    ///
    /// Public-by-virtue-of-being-pub-crate so the handlers in this
    /// crate can reach it. Not exposed in the umbra facade.
    pub(crate) fn apply_overrides(&self, table: &str, row: &mut Map<String, Value>) {
        for (t, f) in &self.hidden {
            if t == table {
                row.remove(f);
            }
        }
        for (t, f, func) in &self.transforms {
            if t == table {
                if let Some(v) = row.get(f) {
                    let new_v = func(v);
                    row.insert(f.clone(), new_v);
                }
            }
        }
        for (t, name, func) in &self.computed {
            if t == table {
                let v = func(row);
                row.insert(name.clone(), v);
            }
        }
    }

    fn allow(&self, table: &str) -> bool {
        if let Some(allow) = &self.include_only {
            return allow.iter().any(|t| t == table);
        }
        // Explicit per-table override of the DEFAULT_BLOCKED_TABLES
        // security default. Lets a user say "yes, please serve
        // auth_user / session over REST, I know what I'm doing."
        if self.expose.contains(table) {
            return !self.extra_exclude.iter().any(|t| t == table);
        }
        if DEFAULT_BLOCKED_TABLES.contains(&table) {
            return false;
        }
        if self.extra_exclude.iter().any(|t| t == table) {
            return false;
        }
        true
    }
}

/// The configured plugin instance, captured at `App::build` time so
/// the route handlers (which can't capture state through axum's
/// handler trait without a State<T>) can consult the allow/block
/// rules per request.
static CONFIG: OnceLock<RestPlugin> = OnceLock::new();

/// Public read of "is filtering enabled for this table?" — used by
/// `umbra-openapi` to decide whether to advertise filter query
/// parameters on a list endpoint's OpenAPI operation. Returns
/// `false` when `RestPlugin::routes()` hasn't run yet (the OnceLock
/// is empty) so calls from spec-only smoke tests don't panic.
/// Public read: would this REST plugin instance serve the given
/// table? Returns the same answer the internal allow/block check
/// uses for the actual list/retrieve/create handlers, so spec
/// consumers (umbra-openapi, the playground sidebar, etc.) stay
/// in lockstep with what the API will actually accept.
///
/// `true` when the table is on the `include_only` list (when one
/// is set) AND not in the default block-list AND not in the
/// plugin's `extra_exclude`. Default behaviour when CONFIG isn't
/// populated yet is `true` — the spec-build path takes that
/// branch before the REST plugin's `routes()` runs in tests.
pub fn is_exposed(table: &str) -> bool {
    CONFIG.get().map(|cfg| cfg.allow(table)).unwrap_or(true)
}

pub fn filters_enabled_for(table: &str) -> bool {
    // Filters are ON by default for every exposed table. The opt-out
    // set carries the tables that explicitly turned filtering off.
    // When CONFIG isn't populated (the REST plugin hasn't booted yet —
    // spec-only smoke tests, for example), we still return `true`
    // because the OpenAPI spec emitted in that context should
    // describe the default-on behaviour.
    CONFIG
        .get()
        .map(|cfg| !cfg.filters_disabled.contains(table))
        .unwrap_or(true)
}

/// Public read: is `?search=` enabled for `table`?
/// Same defaulting story as `filters_enabled_for`: ON by default,
/// opt-out via `ResourceConfig::disable_search()` or the plugin's
/// `disable_search_for([...])`.
pub fn search_enabled_for(table: &str) -> bool {
    CONFIG
        .get()
        .map(|cfg| !cfg.search_disabled.contains(table))
        .unwrap_or(true)
}

impl Plugin for RestPlugin {
    fn name(&self) -> &'static str {
        "rest"
    }

    fn routes(&self) -> Router {
        // The OnceLock-captured config is what the static handlers
        // read. `routes()` is called exactly once per App::build, so
        // setting it here is safe.
        let _ = CONFIG.set(self.clone());

        let mut router = Router::new()
            .route("/api/{table}/", get(list).post(create))
            .route("/api/{table}", get(list).post(create))
            .route(
                "/api/{table}/{id}",
                get(retrieve).put(update).patch(update).delete(destroy),
            );

        // Mount the `@action`-style custom endpoints. We register
        // each one with the table name and action name baked into
        // the path as LITERAL segments — axum's matchit router
        // prefers literal over `{param}` when both exist at the
        // same level, so collection actions on `/api/post/recent`
        // win over `/api/{table}/{id}` cleanly.
        //
        // The handler is a single dispatch fn shared by every
        // action; it pulls the `(table, name)` pair from the URL
        // segments and looks the closure back out of CONFIG.
        for (table, action_list) in &self.actions {
            for def in action_list {
                let path = match def.scope {
                    ActionScope::Collection => {
                        format!("/api/{}/{}", q_seg(table), q_seg(&def.name))
                    }
                    ActionScope::Detail => {
                        format!("/api/{}/{{id}}/{}", q_seg(table), q_seg(&def.name))
                    }
                };
                let method_router =
                    axum::routing::on(method_filter(&def.method), custom_action_dispatch);
                // axum panics on duplicate (path, method); we accept that —
                // a duplicate action registration is a programming
                // error, not a runtime case to recover from.
                router = router.route(&path, method_router);
                // Trailing-slash mirror so `/api/post/recent/` works too.
                router = router.route(
                    &format!("{path}/"),
                    axum::routing::on(method_filter(&def.method), custom_action_dispatch),
                );
            }
        }

        router
    }

    fn route_paths(&self) -> Vec<umbra::routes::RouteSpec> {
        // Concrete paths beat the `/api/{table}/` placeholder: the
        // dev-mode 404 lists them so a developer reading the page
        // can copy-paste an actual URL. We walk the model registry
        // (live by phase 3 of `App::build`) and emit the per-table
        // collection + detail routes, then append every registered
        // custom action.
        //
        // Each collection endpoint accepts GET (list) + POST (create);
        // each detail endpoint accepts GET (retrieve), PUT/PATCH
        // (update), DELETE (destroy). Custom `@action` endpoints use
        // whatever method the closure registered with.
        use umbra::routes::RouteSpec;
        let mut specs: Vec<RouteSpec> = Vec::new();
        for meta in umbra::migrate::registered_models() {
            specs.push(RouteSpec::new(
                format!("/api/{}/", meta.table),
                vec!["GET", "POST"],
            ));
            specs.push(RouteSpec::new(
                format!("/api/{}/{{id}}", meta.table),
                vec!["GET", "PUT", "PATCH", "DELETE"],
            ));
        }
        for (table, action_list) in &self.actions {
            for def in action_list {
                let path = match def.scope {
                    ActionScope::Collection => format!("/api/{table}/{}", def.name),
                    ActionScope::Detail => format!("/api/{table}/{{id}}/{}", def.name),
                };
                // The action's registered method name is the only one
                // it accepts. `http::Method` stringifies as the
                // canonical uppercase verb; we widen its borrow to a
                // `&'static str` via a small match so the value
                // fits `RouteSpec`'s `Vec<&'static str>` shape.
                let method_static: &'static str = match def.method.as_str() {
                    "GET" => "GET",
                    "POST" => "POST",
                    "PUT" => "PUT",
                    "PATCH" => "PATCH",
                    "DELETE" => "DELETE",
                    "HEAD" => "HEAD",
                    "OPTIONS" => "OPTIONS",
                    _ => "ANY",
                };
                specs.push(RouteSpec::new(path, vec![method_static]));
            }
        }
        specs.sort_by(|a, b| a.path.cmp(&b.path));
        specs
    }
}

/// Validate the URL segment is safe to splice into a route path
/// literally — axum 0.8's matchit treats `{` `}` as syntax. We
/// already gated action names through [`is_action_name_char`] in
/// the builder, so this is defense-in-depth for the table name.
fn q_seg(s: &str) -> String {
    assert!(
        !s.contains(['{', '}', '/', '?', '#']),
        "URL segment {s:?} contains a reserved path character"
    );
    s.to_string()
}

/// Translate `http::Method` to axum's `MethodFilter`. Panics on
/// `CONNECT` / `TRACE` (not supported on `@action` routes); other
/// uncommon methods (HEAD / OPTIONS) are wired through anyway.
fn method_filter(m: &http::Method) -> axum::routing::MethodFilter {
    use axum::routing::MethodFilter;
    match *m {
        http::Method::GET => MethodFilter::GET,
        http::Method::POST => MethodFilter::POST,
        http::Method::PUT => MethodFilter::PUT,
        http::Method::PATCH => MethodFilter::PATCH,
        http::Method::DELETE => MethodFilter::DELETE,
        http::Method::HEAD => MethodFilter::HEAD,
        http::Method::OPTIONS => MethodFilter::OPTIONS,
        ref other => {
            panic!("umbra-rest: method {other} isn't supported as an `@action` HTTP method")
        }
    }
}

// =========================================================================
// Errors. Mapped to a JSON envelope so clients get a consistent shape.
// =========================================================================

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: String,
    code: &'static str,
    /// One-sentence operator hint. Currently only populated for
    /// dev-mode 404s, where it explains why the response is richer
    /// than prod's bare envelope (so a Dev-set deploy doesn't leak
    /// the available-endpoints list as a normal-looking detail).
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
    /// Available collection URLs the caller could have hit instead.
    /// Populated only on 404 in `Environment::Dev`. Empty otherwise.
    /// Filtered through the same allow/block list the real handlers
    /// use, so this list never advertises tables the plugin would
    /// refuse to serve.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    available: Vec<String>,
}

#[derive(Debug)]
enum ApiError {
    NotFound(String),
    BadInput(String),
    Sqlx(sqlx::Error),
    Json(serde_json::Error),
    /// 401 — authentication required. Raised when a Permission
    /// returned `PermissionError::Unauthenticated` for an anonymous
    /// request. Includes `WWW-Authenticate: Basic realm="api"`
    /// when the auth chain wants Basic Auth, but the generic case
    /// just signals "you need to authenticate."
    Unauthenticated,
    /// 403 — authenticated, but the permission rule denied this
    /// action. Returned when a Permission produced
    /// `PermissionError::Forbidden` on an authenticated identity.
    Forbidden,
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        // The ORM layer (`DynQuerySet::insert_json`/`update_json`)
        // surfaces structured pre-validation failures —
        // "field X is required", "field X must be an integer" — as
        // `sqlx::Error::Protocol` because that's the only structured
        // string variant the trait surface provides. Mapping them
        // back to `BadInput` here is what makes those land as 400s
        // at the boundary instead of as opaque 500s.
        if matches!(e, sqlx::Error::Protocol(_)) {
            return Self::BadInput(e.to_string());
        }
        Self::Sqlx(e)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl umbra::web::IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, msg) = match self {
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m),
            ApiError::BadInput(m) => (StatusCode::BAD_REQUEST, "bad_input", m),
            ApiError::Sqlx(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "database_error",
                e.to_string(),
            ),
            ApiError::Json(e) => (StatusCode::BAD_REQUEST, "invalid_json", e.to_string()),
            ApiError::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "authentication required".to_string(),
            ),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "forbidden".to_string()),
        };

        let body = if status == StatusCode::NOT_FOUND {
            enrich_404_body(msg, code)
        } else {
            ApiErrorBody {
                error: msg,
                code,
                hint: None,
                available: Vec::new(),
            }
        };
        (status, Json(body)).into_response()
    }
}

/// Build the JSON body for a 404 from this plugin. In `Environment::Dev`
/// the body grows a `hint` and an `available` list of every
/// `/api/<table>/` URL the plugin would actually serve — walked
/// straight from the model registry and filtered through the same
/// `allow()` check the real handlers use. In `Prod` / `Test` the
/// body stays the minimal `{error, code}` envelope so production
/// 404s don't leak the table list to unauthenticated clients.
///
/// Mirrors the framework's HTML 404 dev-panel behaviour
/// (`crate::routes` + the default 404 template), kept in spirit for
/// the REST plugin: JSON-formatted because that's the right shape
/// for an API consumer, but informative when it can be.
fn enrich_404_body(msg: String, code: &'static str) -> ApiErrorBody {
    let is_dev = umbra::settings::get_opt()
        .map(|s| matches!(s.environment, umbra::Environment::Dev))
        .unwrap_or(false);

    if !is_dev {
        return ApiErrorBody {
            error: msg,
            code,
            hint: None,
            available: Vec::new(),
        };
    }

    let mut available: Vec<String> = Vec::new();
    if let Some(cfg) = CONFIG.get() {
        for plugin in umbra::migrate::registered_plugins() {
            for m in umbra::migrate::models_for_plugin(&plugin) {
                if cfg.allow(&m.table) {
                    available.push(format!("/api/{}/", m.table));
                }
            }
        }
        available.sort();
        available.dedup();
    }

    ApiErrorBody {
        error: msg,
        code,
        hint: Some(
            "dev-mode hint: this list of available endpoints is omitted in production. \
             set `environment = \"prod\"` in umbra.toml to drop it."
                .to_string(),
        ),
        available,
    }
}

// =========================================================================
// Model discovery + the allow/block check.
// =========================================================================

fn allowed_model(table: &str) -> Result<ModelMeta, ApiError> {
    let config = CONFIG.get().expect("RestPlugin::routes was called");
    if !config.allow(table) {
        return Err(ApiError::NotFound(format!("no resource at /api/{table}")));
    }
    for plugin in umbra::migrate::registered_plugins() {
        for m in umbra::migrate::models_for_plugin(&plugin) {
            if m.table == table {
                return Ok(m);
            }
        }
    }
    Err(ApiError::NotFound(format!("no resource at /api/{table}")))
}

fn pk_column(model: &ModelMeta) -> Result<&umbra::migrate::Column, ApiError> {
    model
        .fields
        .iter()
        .find(|c| c.primary_key)
        .ok_or_else(|| ApiError::BadInput(format!("`{}` has no primary key", model.table)))
}

// =========================================================================
// Handlers.
// =========================================================================

async fn list(
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: umbra::web::HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let model = allowed_model(&table)?;
    cfg.gate(&table, &Action::List, identity.as_ref())?;

    // Parse query-string filters when this resource has opted in.
    // Filters are ON by default; `filters_disabled` is the opt-out set.
    let filters_on = !cfg.filters_disabled.contains(&table);
    let mut filter = parse_filters(&params, &model.fields, filters_on)?;

    // Free-text search: `?search=<term>` ORs predicates across every
    // searchable column. Default-on, opt-out via
    // `ResourceConfig::disable_search()`. When restricted via
    // `search_fields`, only the named subset participates.
    if !cfg.search_disabled.contains(&table) {
        if let Some(term) = params.get("search") {
            let restrict = cfg.search_fields.get(&table).map(|v| v.as_slice());
            if let Some(search_cond) = parse_search(term, &model.fields, restrict) {
                filter = filter.and(search_cond);
            }
        }
    }

    let page_req = cfg.pagination.extract_request(&params);
    let mut rows = fetch_rows(&model, None, Some(page_req), &filter).await?;
    for row in &mut rows {
        cfg.apply_overrides(&table, row);
    }
    // Skip the extra COUNT round-trip for NoPagination — it would
    // throw away the result anyway. Other paginators read the total
    // for their envelope.
    let total = if cfg.pagination.needs_total() {
        count_rows_filtered(&model, &filter).await?
    } else {
        rows.len() as i64
    };
    let envelope = cfg.pagination.paginate(rows, total, &page_req);
    Ok(Json(envelope))
}

async fn retrieve(
    Path((table, id)): Path<(String, String)>,
    headers: umbra::web::HeaderMap,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let model = allowed_model(&table)?;
    cfg.gate(&table, &Action::Retrieve, identity.as_ref())?;
    let pk = pk_column(&model)?;
    let no_filter = FilterClause::default();
    let mut rows = fetch_rows(&model, Some((&pk.name, &id)), None, &no_filter).await?;
    let Some(mut row) = rows.pop() else {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    };
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    cfg.apply_overrides(&table, &mut row);
    Ok(Json(row))
}

async fn create(
    Path(table): Path<String>,
    headers: umbra::web::HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> Result<(StatusCode, Json<Map<String, Value>>), ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let model = allowed_model(&table)?;
    cfg.gate(&table, &Action::Create, identity.as_ref())?;
    let mut row = umbra::orm::DynQuerySet::for_meta(&model)
        .insert_json(&body)
        .await?;
    cfg.apply_overrides(&table, &mut row);
    Ok((StatusCode::CREATED, Json(row)))
}

async fn update(
    Path((table, id)): Path<(String, String)>,
    headers: umbra::web::HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let model = allowed_model(&table)?;
    cfg.gate(&table, &Action::Update, identity.as_ref())?;
    let pk = pk_column(&model)?;

    // 404 if the target row doesn't exist before we attempt the UPDATE.
    let no_filter = FilterClause::default();
    let existing = fetch_rows(&model, Some((&pk.name, &id)), None, &no_filter).await?;
    if existing.is_empty() {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    }

    // PATCH-style update: only the columns supplied in the body are
    // written, primary key never. Returns the row count which we
    // discard — we re-read the row below to send back the canonical
    // post-update shape (incl. any DB-side defaults or triggers).
    umbra::orm::DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .update_json(&body)
        .await?;
    let mut rows = fetch_rows(&model, Some((&pk.name, &id)), None, &no_filter).await?;
    let Some(mut row) = rows.pop() else {
        return Err(ApiError::BadInput(
            "row updated but disappeared on read-back".into(),
        ));
    };
    cfg.apply_overrides(&table, &mut row);
    Ok(Json(row))
}

async fn destroy(
    Path((table, id)): Path<(String, String)>,
    headers: umbra::web::HeaderMap,
) -> Result<StatusCode, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let model = allowed_model(&table)?;
    cfg.gate(&table, &Action::Delete, identity.as_ref())?;
    let pk = pk_column(&model)?;
    let affected = umbra::orm::DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .delete()
        .await?;
    if affected == 0 {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

// =========================================================================
// Custom-action dispatch.
//
// One generic handler that's mounted at every (table, action) path
// the user registered via `ResourceConfig::action(...)`. It reads
// the path's table + action segments (literal, baked-in at routes()
// time) and looks the closure back out of CONFIG. Detail-scope
// actions get the `{id}` path param too.
//
// Why a single handler instead of one per action: axum can mount
// closures, but they have to satisfy `Handler` — which means picking
// extractors at compile time. With a dynamic count of registered
// actions, we'd need either a per-action handler factory (ugly,
// would need to be macro-generated) or a single dispatch that pulls
// state from the static CONFIG. The latter is what every other
// handler already does, so it's the consistent choice.
// =========================================================================

async fn custom_action_dispatch(
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: umbra::web::HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, ApiError> {
    let (table, name, pk) = parse_action_route(uri.path())?;
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");

    // Locate the registered action by (table, name, method). The
    // request's HTTP method has to match the one the user passed at
    // registration time; a method mismatch falls through axum to a
    // 405, so we shouldn't see one here, but be defensive.
    let def = cfg
        .actions
        .get(&table)
        .and_then(|list| list.iter().find(|d| d.name == name && d.method == method))
        .ok_or_else(|| ApiError::NotFound(format!("no @action `{name}` on `{table}`")))?;

    // Permission gate runs with `Action::Custom(name)` so the
    // resource's permission can deny or allow per-action.
    let identity = cfg.authentication.authenticate(&headers).await;
    cfg.gate(&table, &Action::Custom(name.clone()), identity.as_ref())?;

    let query = parse_query_string(uri.query().unwrap_or(""));
    let ctx = ActionContext {
        table: table.clone(),
        name: name.clone(),
        pk,
        identity,
        body: body.map(|Json(v)| v).unwrap_or(Value::Null),
        query,
    };

    let result = (def.handler)(ctx).await;
    match result {
        Ok(v) => Ok(Json(v)),
        Err(ActionError::BadInput(m)) => Err(ApiError::BadInput(m)),
        Err(ActionError::NotFound(m)) => Err(ApiError::NotFound(m)),
        Err(ActionError::Unauthenticated) => Err(ApiError::Unauthenticated),
        Err(ActionError::Forbidden) => Err(ApiError::Forbidden),
        Err(ActionError::Internal(m)) => Err(ApiError::Sqlx(sqlx::Error::Protocol(m))),
    }
}

/// Decode a `key=value&key=value` query string into a HashMap.
/// Percent-decoding the values is left to consumers — the v1 contract
/// is that `ActionContext::query` carries raw URL-encoded bytes; if
/// real-world consumers want it decoded by default we can swap later.
fn parse_query_string(q: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        } else {
            out.insert(pair.to_string(), String::new());
        }
    }
    out
}

/// Parse `/api/<table>/<name>` and `/api/<table>/<id>/<name>` —
/// trailing slash tolerated. Returns `(table, action_name, pk)`
/// where `pk` is `Some(id)` for detail-scope.
fn parse_action_route(path: &str) -> Result<(String, String, Option<String>), ApiError> {
    let trimmed = path.trim_end_matches('/');
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        ["api", table, name] => Ok((table.to_string(), name.to_string(), None)),
        ["api", table, id, name] => Ok((table.to_string(), name.to_string(), Some(id.to_string()))),
        _ => Err(ApiError::NotFound(format!(
            "{path} is not a recognised @action route"
        ))),
    }
}

// =========================================================================
// Row helpers. Every row read / write routes through DynQuerySet, which
// emits the dialect-correct SQL via sea-query and binds values via
// sea-query-binder. Identifier escaping is the queryset's job, not ours.
// =========================================================================

async fn fetch_rows(
    model: &ModelMeta,
    where_clause: Option<(&str, &str)>,
    page: Option<PageRequest>,
    filter: &FilterClause,
) -> Result<Vec<Map<String, Value>>, ApiError> {
    let mut qs = umbra::orm::DynQuerySet::for_meta(model);

    if let Some((col, val)) = where_clause {
        // Single-row lookup (retrieve / update / delete read-back).
        // The WHERE col = val + LIMIT 1 shape is the same as before.
        qs = qs.filter_eq_string(col, val).limit(1);
    } else {
        // List path: pagination applies, plus any filter the resource
        // opted in to. `FilterClause` ANDs every parsed predicate.
        if !filter.is_empty()
            && let Some(cond) = filter.condition_clone()
        {
            qs = qs.filter_condition(cond);
        }
        if let Some(req) = page
            && req.limit != u64::MAX
        {
            qs = qs.limit(req.limit).offset(req.offset);
        }
    }

    let rows = qs.fetch_as_json().await?;
    Ok(rows)
}

/// `SELECT COUNT(*)` for the given model, respecting any active
/// filter predicates so the paginator's total reflects the filtered
/// result set rather than the whole table.
async fn count_rows_filtered(model: &ModelMeta, filter: &FilterClause) -> Result<i64, ApiError> {
    let mut qs = umbra::orm::DynQuerySet::for_meta(model);
    if !filter.is_empty()
        && let Some(cond) = filter.condition_clone()
    {
        qs = qs.filter_condition(cond);
    }
    Ok(qs.count().await?)
}

#[cfg(test)]
mod allow_block_unit {
    use super::*;

    #[test]
    fn default_plugin_blocks_auth_user_and_session_and_migrations() {
        let p = RestPlugin::new();
        assert!(!p.allow("auth_user"));
        assert!(!p.allow("session"));
        assert!(!p.allow("umbra_migrations"));
        assert!(p.allow("article"));
    }

    #[test]
    fn expose_overrides_default_block_for_named_tables() {
        let p = RestPlugin::new().expose(["auth_user"]);
        assert!(p.allow("auth_user"), "expose should let auth_user through");
        // Other blocked tables stay blocked unless individually exposed.
        assert!(!p.allow("session"));
        assert!(!p.allow("umbra_migrations"));
        // Regular tables unaffected.
        assert!(p.allow("article"));
    }

    #[test]
    fn extra_exclude_beats_expose_when_both_name_the_same_table() {
        // Explicit user "no" wins over explicit user "yes" for the
        // same table — least surprising answer when two configs
        // contradict.
        let p = RestPlugin::new()
            .expose(["auth_user"])
            .exclude(["auth_user"]);
        assert!(!p.allow("auth_user"));
    }

    #[test]
    fn include_only_short_circuits_expose() {
        // include_only is the strictest gate; tables not in it are
        // blocked regardless of expose.
        let p = RestPlugin::new()
            .include_only(["article"])
            .expose(["auth_user"]);
        assert!(p.allow("article"));
        assert!(
            !p.allow("auth_user"),
            "include_only's allow-list is exhaustive — expose can't punch through"
        );
    }
}
