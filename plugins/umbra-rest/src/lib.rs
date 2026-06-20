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

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use serde::Serialize;
use serde_json::{Map, Value};
use umbra::migrate::ModelMeta;
use umbra::prelude::*;
use umbra::web::{IntoResponse, Json, Path, Query, Response, StatusCode};

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
/// would leak password hashes (auth_user), session IDs (session), the
/// migration tracking table, the authorization model itself (the
/// `permissions_*` tables), the background-job queue (`task_row`, whose
/// payloads can carry secrets and where "enqueue" is close to code
/// execution), or the admin audit trail. A consumer who genuinely wants
/// one served opts back in explicitly with `RestPlugin::expose(...)`.
/// WEB-1: keeping framework-internal security/infra tables off the
/// default surface limits the blast radius of the open-by-default API.
/// Hard ceiling on rows a single list request can return, even under
/// `NoPagination` (the default). PERF-1: stops `GET /api/<table>/` from
/// buffering an unbounded table into memory. A resource that genuinely
/// needs more configures a paginator with a higher page size; this is the
/// floor that protects the default config.
const MAX_LIST_ROWS: u64 = 1000;

const DEFAULT_BLOCKED_TABLES: &[&str] = &[
    "auth_user",
    "session",
    "umbra_migrations",
    "permissions_permission",
    "permissions_contenttype",
    "permissions_group",
    "permissions_usergroup",
    "permissions_userpermission",
    "task_row",
    "admin_audit_log",
];

/// Field names that are ALWAYS stripped from every serialized response,
/// on every table, regardless of `.expose(...)` / `.hide(...)` / any
/// `ResourceConfig` override. This is the hard security denylist.
///
/// The threat: a developer calls `.expose(["auth_user"])` to serve that
/// table over REST but forgets to pair it with `.hide("password_hash")`.
/// Without this list the argon2 hash leaks to every API consumer.
/// With it, `password_hash` is stripped *after* all configurable logic
/// runs, so no combination of builder calls can re-expose it.
///
/// Gap: gaps2 #75.
const HARD_DENIED_FIELDS: &[&str] = &["password_hash"];

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

/// Accept-either argument for the `hide` builders. Lets a caller pass
/// a single field (`"a"`) or many (`["a", "b"]`, `vec!["a"]`,
/// `&["a"][..]`) to [`RestPlugin::hide`] / [`ResourceConfig::hide`]
/// without two separate method names.
///
/// Implemented for `&str`, `String`, fixed-size arrays of either, and
/// the slice / `Vec` forms. Adding the trait is non-breaking: every
/// existing `.hide("field")` call keeps compiling because `&str:
/// HideFields`.
pub trait HideFields {
    /// Flatten the argument into the list of field names to hide.
    fn into_field_list(self) -> Vec<String>;
}

impl HideFields for &str {
    fn into_field_list(self) -> Vec<String> {
        vec![self.to_string()]
    }
}

impl HideFields for String {
    fn into_field_list(self) -> Vec<String> {
        vec![self]
    }
}

impl<const N: usize> HideFields for [&str; N] {
    fn into_field_list(self) -> Vec<String> {
        self.iter().map(|s| s.to_string()).collect()
    }
}

impl<const N: usize> HideFields for [String; N] {
    fn into_field_list(self) -> Vec<String> {
        self.into_iter().collect()
    }
}

impl HideFields for &[&str] {
    fn into_field_list(self) -> Vec<String> {
        self.iter().map(|s| s.to_string()).collect()
    }
}

impl HideFields for &[String] {
    fn into_field_list(self) -> Vec<String> {
        self.to_vec()
    }
}

impl HideFields for Vec<&str> {
    fn into_field_list(self) -> Vec<String> {
        self.iter().map(|s| s.to_string()).collect()
    }
}

impl HideFields for Vec<String> {
    fn into_field_list(self) -> Vec<String> {
        self
    }
}

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
    /// via [`Self::resource`]. Tables without an entry fall back to
    /// [`Self::default_permission`].
    permissions: HashMap<String, Arc<dyn Permission>>,
    /// Fallback permission for tables with no explicit `.permission(...)`.
    /// Defaults to [`ReadOnly`] (WEB-1: safe by default — anonymous reads,
    /// no anonymous writes). Override the blanket default with
    /// [`Self::default_permission`] — e.g. `AllowAny` to restore the old
    /// fully-open behaviour, or `IsAuthenticated` for an app behind auth.
    default_permission: Arc<dyn Permission>,
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
    /// Writable nested resources per table: `table -> [(json_field,
    /// child_table)]`. Merged from `ResourceConfig::nested(...)`; read by
    /// the create handler to insert children alongside the parent.
    nested: HashMap<String, Vec<(String, String)>>,
    /// Gap 107: base URL prefix for all REST endpoints. Default
    /// `/api`. Set via `RestPlugin::at("/v1")`. Always normalised
    /// to one leading slash, no trailing slash.
    base_path: String,
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
    /// Resolve the permission class for a table.
    ///
    /// WEB-1: the default is now [`ReadOnly`], **not** `AllowAny`. A
    /// resource with no explicit `.permission(...)` serves anonymous
    /// reads (List/Retrieve) but rejects every write (Create/Update/
    /// Delete) with 403. Open writes are now an explicit opt-in:
    /// `.permission("table", AllowAny)` (or a real permission /
    /// authentication backend). This closes "add RestPlugin, get a read
    /// API, and silently expose anonymous full CRUD on every model".
    fn permission_for(&self, table: &str) -> Arc<dyn Permission> {
        self.permissions
            .get(table)
            .cloned()
            .unwrap_or_else(|| self.default_permission.clone())
    }

    /// Set the blanket fallback permission for every table that has no
    /// explicit `ResourceConfig::permission(...)`.
    ///
    /// The default is [`ReadOnly`] (WEB-1). Pass [`AllowAny`] to restore
    /// the old fully-open behaviour (anonymous CRUD on every model — only
    /// do this for a trusted/internal deployment), or [`IsAuthenticated`]
    /// for an app where every endpoint sits behind login. A per-resource
    /// `.permission(...)` always wins over this default.
    pub fn default_permission<P: Permission>(mut self, perm: P) -> Self {
        self.default_permission = Arc::new(perm);
        self
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
            default_permission: Arc::new(ReadOnly),
            view_scope: HashMap::new(),
            actions: HashMap::new(),
            filters_disabled: std::collections::HashSet::new(),
            search_disabled: std::collections::HashSet::new(),
            search_fields: HashMap::new(),
            nested: HashMap::new(),
            base_path: "/api".to_string(),
        }
    }

    /// Gap 107: override the URL prefix for all REST endpoints.
    /// Default is `/api`. Use `RestPlugin::default().at("/v1")` to
    /// version your API, or `.at("/internal/api")` to nest it under a
    /// deeper segment. The path is normalised to one leading slash
    /// and no trailing slash, so `"api"`, `"/api"`, and `"/api/"`
    /// all produce the same routes. Empty string mounts at the root
    /// (rare but supported).
    ///
    /// ```ignore
    /// RestPlugin::default().at("/v1")  // → /v1/post/, /v1/post/{id}, ...
    /// ```
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

    /// The normalised base path for this plugin. Public for the
    /// OpenAPI plugin to read so the spec mirrors the live routes.
    pub fn base_path(&self) -> &str {
        &self.base_path
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
    ///         let user = umbra_auth::current_user(&headers).await.ok().flatten()?;
    ///         Some(Identity::user(user.id).with_staff(user.is_staff))
    ///     }))
    /// ```
    pub fn authenticate<A: Authentication>(mut self, auth: A) -> Self {
        self.authentication = Arc::new(auth);
        self
    }

    /// Walk the configured `Authentication` and return every
    /// `securitySchemes` entry it contributes — used by the
    /// OpenAPI plugin to publish a complete schemes block. For a
    /// `ChainAuthentication([Session, Bearer])` this returns both
    /// entries. For a single backend it returns at most one.
    /// `NoAuthentication` returns an empty Vec. Closes
    /// playground-openapi-gaps item 4.
    pub fn security_schemes(&self) -> Vec<(String, serde_json::Value)> {
        self.authentication.security_schemes_all()
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

    /// Register many [`ResourceConfig`]s at once — the batch form of
    /// [`resource`](Self::resource). Lets each plugin export a
    /// `Vec<ResourceConfig>` (its REST surface, declared next to its
    /// models, the way DRF keeps serializers per app) and the app register
    /// them in one call instead of a `.resource(...)` per model in `main.rs`.
    ///
    /// ```ignore
    /// // plugins/blog/src/lib.rs
    /// pub fn rest_resources() -> Vec<umbra_rest::ResourceConfig> {
    ///     vec![
    ///         umbra_rest::ResourceConfig::new("post").hide("draft_notes"),
    ///         umbra_rest::ResourceConfig::new("comment"),
    ///     ]
    /// }
    ///
    /// // main.rs
    /// RestPlugin::default()
    ///     .resources(blog::rest_resources())
    ///     .resources(shop::rest_resources())
    /// ```
    pub fn resources(mut self, configs: impl IntoIterator<Item = ResourceConfig>) -> Self {
        for config in configs {
            self = self.resource(config);
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
            nested,
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
        if !nested.is_empty() {
            self.nested.entry(table.clone()).or_default().extend(nested);
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

    /// Strip one or more fields from every REST response for the given
    /// table. The columns stay readable through the ORM and writable
    /// via POST/PUT/PATCH — this only changes the outgoing JSON shape.
    ///
    /// `fields` accepts a single name or many via [`HideFields`]:
    ///
    /// ```ignore
    /// RestPlugin::new()
    ///     .hide("user", "password_hash")          // single
    ///     .hide("user", ["password_hash", "ssn"]) // many
    /// ```
    pub fn hide(mut self, table: &str, fields: impl HideFields) -> Self {
        for field in fields.into_field_list() {
            self.hidden.push((table.to_string(), field));
        }
        self
    }

    /// Like [`Self::hide`] but the table is taken from the model's
    /// [`Model::TABLE`](umbra::orm::Model) const, so a typo in the
    /// table name is a compile error rather than a silent no-op.
    ///
    /// ```ignore
    /// RestPlugin::new().hide_model::<AuthUser>(["password_hash", "email"])
    /// ```
    pub fn hide_model<M: umbra::orm::Model>(mut self, fields: impl HideFields) -> Self {
        for field in fields.into_field_list() {
            self.hidden.push((M::TABLE.to_string(), field));
        }
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
    /// Recurses into `?include=`'d nested relations: when a column is a
    /// foreign key and its value in `row` has been hydrated into a JSON
    /// object (rather than left as the raw integer FK), the same
    /// overrides — keyed off the FK's *target* table — are applied to
    /// that nested object. This is the difference between a top-level
    /// `hide("auth_user", "password_hash")` that only scrubs the root
    /// row and one that ALSO scrubs `auth_user` when it appears nested
    /// under e.g. `?include=created_by` — without the recursion, a
    /// hidden column leaks through the nested relation (a data leak).
    ///
    /// Public-by-virtue-of-being-pub-crate so the handlers in this
    /// crate can reach it. Not exposed in the umbra facade.
    pub(crate) fn apply_overrides(&self, table: &str, row: &mut Map<String, Value>) {
        // Cap recursion so a self-referential FK that got `?include=`'d
        // (or a pathological hydration) can't loop forever. 5 hops is
        // comfortably past `?include=`'s own MAX_DEPTH of 3.
        self.apply_overrides_depth(table, row, 0);
    }

    fn apply_overrides_depth(&self, table: &str, row: &mut Map<String, Value>, depth: usize) {
        const MAX_DEPTH: usize = 5;

        // --- Recurse into hydrated nested relations FIRST, so the
        // nested objects are scrubbed by their own table's overrides
        // before the parent's hide/transform/computed run on the
        // (now-clean) parent row. Only FK columns whose value is a JSON
        // object were `?include=`-hydrated; everything else (raw integer
        // FKs, scalar columns) is left untouched. ---
        if depth < MAX_DEPTH
            && let Some(meta) = umbra::migrate::model_meta_for_table(table)
        {
            for col in &meta.fields {
                // File/image columns store a bare storage KEY in a TEXT
                // column. REST consumers want the resolved public URL, not
                // the opaque key, so swap a non-empty string value for
                // `storage().url(key)`. A nullable field with no upload is
                // `Value::Null` and stays null; an empty string stays empty
                // (never turned into a bare `/media/`). Resolved through the
                // ambient Storage backend, falling back to the raw key when
                // no backend is wired.
                if matches!(col.widget.as_deref(), Some("file") | Some("image")) {
                    // Compute the owned resolved URL while only borrowing
                    // `row` immutably (via `row.get`); let that borrow end
                    // before the `row.insert` below (borrow-checker dance).
                    let resolved: Option<String> = match row.get(&col.name) {
                        Some(Value::String(key)) if !key.is_empty() => Some(
                            umbra::storage::storage_opt()
                                .map(|s| s.url(key))
                                .unwrap_or_else(|| key.clone()),
                        ),
                        _ => None,
                    };
                    if let Some(url) = resolved {
                        row.insert(col.name.clone(), Value::String(url));
                    }
                }
                let Some(fk_target) = col.fk_target.as_deref() else {
                    continue;
                };
                if let Some(Value::Object(nested)) = row.get_mut(&col.name) {
                    self.apply_overrides_depth(fk_target, nested, depth + 1);
                }
            }
        }

        // Reuse `is_field_hidden` as the single source of truth so the
        // runtime strip here and the public `is_hidden` (which the
        // OpenAPI plugin reads to scrub the spec) can never disagree on
        // which fields are hidden. `self.hidden` carries both the
        // plugin-level hides and the resource-level ones (merged in at
        // `RestPlugin::resource`), so iterating its keys covers both.
        let to_remove: Vec<String> = self
            .hidden
            .iter()
            .filter(|(t, _)| t == table)
            .map(|(_, f)| f.clone())
            .filter(|f| self.is_field_hidden(table, f))
            .collect();
        for f in to_remove {
            row.remove(&f);
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

        // Hard security denylist — applied LAST, after all configurable
        // hide / transform / computed logic, so no `.expose()` or missing
        // `.hide()` call can re-expose these fields. gaps2 #75.
        for field in HARD_DENIED_FIELDS {
            row.remove(*field);
        }
    }

    /// Sparse fieldset (gap #81 + nested-projection extension). Prune
    /// the response row down to a caller-named subset of keys, walking
    /// into `?include=`'d nested objects to N hops.
    ///
    /// Token shapes, with `.` and `__` interchangeable as the hop
    /// separator (`created_by__name` ≡ `created_by.name`, matching
    /// `?include=`'s gap2 #18 normalisation):
    ///
    /// - **Plain** (`id`, `phone`, `user`) — keeps the named key. If
    ///   the key holds a nested object (because it was `?include=`'d),
    ///   the full nested shape survives untouched.
    ///
    /// - **Dotted / `__`** (`user.id`, `created_by__name`,
    ///   `a__b__c`) — keeps the named key, then recurses into the
    ///   nested object pruning it to the requested child path. The
    ///   parent is auto-kept so the nested object survives the retain
    ///   step. ANY nested token under a parent triggers pruning on
    ///   that nested object, so `?fields=user,user.id` collapses to
    ///   "keep user, but only keep `id` inside it" (most-specific
    ///   wins — the presence of a deeper path overrides the bare
    ///   "keep whole subtree").
    ///
    /// Examples (with `?include=user` / `?include=created_by.team`):
    ///
    /// | `?fields=` | Resulting row |
    /// |---|---|
    /// | `id,phone` | `{id, phone}` — user dropped |
    /// | `id,user` | `{id, user: {full user obj}}` |
    /// | `id,user.id,user.username` | `{id, user: {id, username}}` |
    /// | `user.id` | `{user: {id}}` — root id NOT pulled |
    /// | `created_by__team__name` | `{created_by: {team: {name}}}` |
    ///
    /// Applied *after* `apply_overrides` so users can still combine
    /// hide / transform / computed with sparse selection. Unknown
    /// names are silently ignored at every level — gives clients
    /// latitude to ask for new fields without coordinating a server
    /// change first. A nested path against a key that's still an
    /// integer FK (the relation wasn't `?include=`'d) leaves the
    /// integer untouched rather than crashing.
    ///
    /// Field-path depth is capped at the same `?include=` 3-hop norm
    /// (gap2 #18): hops past the cap are dropped from the token, so a
    /// pathological `a__b__c__d` prunes to `a.b.c` and ignores the
    /// rest rather than fanning out.
    pub(crate) fn apply_sparse_fields(row: &mut Map<String, Value>, fields_param: Option<&str>) {
        /// Mirrors `parse_include`'s MAX_DEPTH (gap2 #18) so field-path
        /// projection can't out-reach what `?include=` could hydrate.
        const MAX_FIELD_DEPTH: usize = 3;

        let Some(raw) = fields_param else { return };

        // Build an allowed-paths tree from every token. A node's
        // `children` map names the keys to keep one level down; an
        // EMPTY children map is a leaf meaning "keep this whole
        // subtree". A later, deeper token under the same key adds
        // children, which turns a leaf into a pruning node (so a
        // deeper path always wins over a bare plain token).
        #[derive(Default)]
        struct Node {
            children: std::collections::HashMap<String, Node>,
        }

        let mut root = Node::default();
        let mut any = false;
        for token in raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            // Normalise `__` → `.` then split, capping the hop count.
            let canonical = token.replace("__", ".");
            let hops: Vec<&str> = canonical
                .split('.')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take(MAX_FIELD_DEPTH)
                .collect();
            if hops.is_empty() {
                continue;
            }
            any = true;
            let mut cur = &mut root;
            for hop in hops {
                cur = cur.children.entry(hop.to_string()).or_default();
            }
        }

        if !any {
            return;
        }

        // Recursively prune `obj` to the keys named in `node`. Keys
        // not in the node are dropped. A key whose node has children
        // is descended into when its value is an object; otherwise
        // (leaf, or value isn't an object — e.g. an un-included
        // integer FK) the value is kept verbatim.
        fn prune(obj: &mut Map<String, Value>, node: &Node) {
            obj.retain(|k, _| node.children.contains_key(k));
            for (key, child_node) in &node.children {
                if child_node.children.is_empty() {
                    continue; // leaf — keep the whole subtree
                }
                if let Some(Value::Object(child_obj)) = obj.get_mut(key) {
                    prune(child_obj, child_node);
                }
            }
        }

        prune(row, &root);
    }

    /// True when `field` on `table` is stripped from response bodies.
    /// The single membership check both [`Self::apply_overrides`]'s
    /// hide loop and the public [`crate::is_hidden`] read, so the
    /// runtime strip and the OpenAPI spec can't drift.
    ///
    /// Covers BOTH hide sources because they land in the same place:
    /// plugin-level `RestPlugin::hide` / `hide_model` push into
    /// `self.hidden`, and resource-level `ResourceConfig::hide` fields
    /// are merged into `self.hidden` at `RestPlugin::resource(...)`
    /// time. So checking `self.hidden` alone agrees 1:1 with what
    /// `apply_overrides` removes.
    pub(crate) fn is_field_hidden(&self, table: &str, field: &str) -> bool {
        // Hard-denied fields are always hidden, regardless of any
        // `.expose()` / `.hide()` configuration. gaps2 #75.
        if HARD_DENIED_FIELDS.contains(&field) {
            return true;
        }
        self.hidden.iter().any(|(t, f)| t == table && f == field)
    }

    /// Drop every REST-hidden field from an inbound write body.
    ///
    /// WEB-2: hiding a field (`ResourceConfig::hide` / `RestPlugin::hide`)
    /// removes it from responses (`apply_overrides`), but the column stayed
    /// *writable* — so `PATCH /api/x {"hidden_field": ...}` could still set
    /// it (mass assignment / privilege escalation when the hidden field is
    /// something like `is_admin`). Stripping it here makes `hide` symmetric:
    /// hidden in, hidden out. The ORM still strips `noform` columns on its
    /// own; this layers the REST `hide` list on top.
    pub(crate) fn strip_hidden_for_write(&self, table: &str, body: &mut Map<String, Value>) {
        body.retain(|k, _| !self.is_field_hidden(table, k));
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

/// Public read: would this REST plugin strip `field` from `table`'s
/// response bodies? Returns the SAME answer `RestPlugin::apply_overrides`
/// uses at request time (both consult `RestPlugin::is_field_hidden`), so
/// spec consumers (umbra-openapi) advertise exactly the fields the API
/// actually returns — a `hide`d column like `password_hash` never leaks
/// into the generated schema, the `?fields=` picker, or Swagger UI.
///
/// Covers both hide sources: plugin-level `RestPlugin::hide` /
/// `hide_model` AND resource-level `ResourceConfig::hide` (which is
/// merged into the plugin's hide set at registration).
///
/// `false` when CONFIG isn't populated yet (no REST plugin booted —
/// spec-only smoke tests) so the spec describes the default "nothing
/// hidden" shape. Same defaulting ordering as `is_exposed`, which
/// assumes CONFIG is set before openapi runs.
pub fn is_hidden(table: &str, field: &str) -> bool {
    CONFIG
        .get()
        .map(|cfg| cfg.is_field_hidden(table, field))
        .unwrap_or(false)
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

/// Public read: every `securitySchemes` entry contributed by the
/// configured Authentication chain. Used by `umbra-openapi` at
/// spec-build time. Returns an empty Vec when CONFIG isn't
/// populated (no REST plugin booted) — same defaulting story as
/// `filters_enabled_for`. Closes playground-openapi-gaps item 4.
pub fn registered_security_schemes() -> Vec<(String, serde_json::Value)> {
    CONFIG
        .get()
        .map(|cfg| cfg.authentication.security_schemes_all())
        .unwrap_or_default()
}

/// One custom `@action`'s OpenAPI-facing schema info — read by
/// `umbra-openapi` to emit the action's path + request/response schemas
/// (feature #60).
#[derive(Debug, Clone)]
pub struct ActionSchema {
    pub table: String,
    pub name: String,
    /// HTTP method, e.g. `"POST"`.
    pub method: String,
    /// `true` for detail-scope (`/{id}/<name>/`), `false` for collection.
    pub detail: bool,
    /// The base path resources mount under (e.g. `"/api"`).
    pub base_path: String,
    pub input_schema: Option<serde_json::Value>,
    pub output_schema: Option<serde_json::Value>,
}

/// Public read: every custom `@action` that declared an input or output
/// schema (`ResourceConfig::action_input_schema` / `action_output_schema`).
/// Used by `umbra-openapi` at spec-build time. Empty when no REST plugin
/// has booted.
pub fn registered_action_schemas() -> Vec<ActionSchema> {
    let Some(cfg) = CONFIG.get() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (table, defs) in &cfg.actions {
        for d in defs {
            if d.input_schema.is_none() && d.output_schema.is_none() {
                continue;
            }
            out.push(ActionSchema {
                table: table.clone(),
                name: d.name.clone(),
                method: d.method.to_string(),
                detail: matches!(d.scope, ActionScope::Detail),
                base_path: cfg.base_path.clone(),
                input_schema: d.input_schema.clone(),
                output_schema: d.output_schema.clone(),
            });
        }
    }
    out
}

/// Validate `instance` against a subset of JSON Schema — the common
/// action-guard shapes: top-level `type`, `required`, and `properties`
/// (recursing into each, with per-property `type` + `enum`). Unsupported
/// keywords are ignored (permissive); the full schema still ships in the
/// OpenAPI spec. Returns human-readable errors (empty = valid).
fn validate_against_schema(schema: &Value, instance: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    validate_schema_node("", schema, instance, &mut errors);
    errors
}

fn validate_schema_node(path: &str, schema: &Value, instance: &Value, errors: &mut Vec<String>) {
    let Some(schema) = schema.as_object() else {
        return;
    };
    if let Some(ty) = schema.get("type").and_then(|v| v.as_str()) {
        if !json_type_matches(ty, instance) {
            errors.push(format!("{}: expected type `{ty}`", schema_label(path)));
            return; // type mismatch — deeper checks are moot
        }
    }
    if let Some(Value::Array(allowed)) = schema.get("enum") {
        if !allowed.iter().any(|a| a == instance) {
            errors.push(format!(
                "{}: value is not one of the allowed options",
                schema_label(path)
            ));
        }
    }
    if let Some(obj) = instance.as_object() {
        if let Some(Value::Array(required)) = schema.get("required") {
            for r in required.iter().filter_map(|v| v.as_str()) {
                if obj.get(r).map(|v| v.is_null()).unwrap_or(true) {
                    errors.push(format!("`{r}` is required"));
                }
            }
        }
        if let Some(Value::Object(props)) = schema.get("properties") {
            for (name, prop_schema) in props {
                if let Some(val) = obj.get(name) {
                    let child = if path.is_empty() {
                        name.clone()
                    } else {
                        format!("{path}.{name}")
                    };
                    validate_schema_node(&child, prop_schema, val, errors);
                }
            }
        }
    }
}

fn json_type_matches(expected: &str, v: &Value) -> bool {
    match expected {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        _ => true, // unknown type keyword — don't reject
    }
}

fn schema_label(path: &str) -> String {
    if path.is_empty() {
        "body".to_string()
    } else {
        format!("`{path}`")
    }
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

        // WEB-1: shout if any exposed resource is reachable with no
        // authentication AND an open (AllowAny) permission — that's
        // anonymous full CRUD from the internet, the highest-leverage
        // footgun in the API. We can't change the open-by-default
        // contract from under existing apps, but a developer who didn't
        // mean it sees exactly which tables are wide open at boot.
        if self.authentication.is_anonymous() {
            let open: Vec<String> = umbra::migrate::registered_models()
                .iter()
                .map(|m| m.table.clone())
                .filter(|t| self.allow(t) && self.permission_for(t).is_open())
                .collect();
            if !open.is_empty() {
                tracing::warn!(
                    tables = %open.join(", "),
                    "umbra-rest: {} resource(s) are exposed with NO authentication and an \
                     AllowAny permission — anonymous clients can read AND write them \
                     (POST/PUT/PATCH/DELETE). Set RestPlugin::authenticate(...) and/or a \
                     per-resource .permission(...) (ReadOnly / IsAuthenticated / IsStaff), \
                     or .exclude(...) the table if it shouldn't be served at all.",
                    open.len(),
                );
            }
        }

        let base = &self.base_path;
        let mut router = Router::new()
            .route(&format!("{base}/{{table}}/"), get(list).post(create))
            .route(&format!("{base}/{{table}}"), get(list).post(create))
            .route(
                &format!("{base}/{{table}}/{{id}}"),
                get(retrieve).put(update).patch(update).delete(destroy),
            );

        // API root index: lists the exposed resources + every plugin's
        // advertised endpoints (service discovery). Skipped when REST is
        // mounted at the bare root (empty base), where `/` would collide
        // with the app's own home route.
        if !base.is_empty() {
            router = router
                .route(&format!("{base}/"), get(api_root))
                .route(base, get(api_root));
        }

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
                        format!("{base}/{}/{}", q_seg(table), q_seg(&def.name))
                    }
                    ActionScope::Detail => {
                        format!("{base}/{}/{{id}}/{}", q_seg(table), q_seg(&def.name))
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
        let base = &self.base_path;
        let mut specs: Vec<RouteSpec> = Vec::new();
        for meta in umbra::migrate::registered_models() {
            specs.push(RouteSpec::new(
                format!("{base}/{}/", meta.table),
                vec!["GET", "POST"],
            ));
            specs.push(RouteSpec::new(
                format!("{base}/{}/{{id}}", meta.table),
                vec!["GET", "PUT", "PATCH", "DELETE"],
            ));
        }
        for (table, action_list) in &self.actions {
            for def in action_list {
                let path = match def.scope {
                    ActionScope::Collection => format!("{base}/{table}/{}", def.name),
                    ActionScope::Detail => format!("{base}/{table}/{{id}}/{}", def.name),
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
    /// Stable machine-readable error code. Always populated.
    code: &'static str,
    /// DRF-style field-level errors flattened to the top level
    /// (`{ "category": ["..."], "sku": ["..."] }`). Empty for
    /// non-validation errors.
    #[serde(flatten)]
    field_errors: BTreeMap<String, Vec<String>>,
    /// Validation errors not tied to a specific field. Mirrors
    /// DRF's `non_field_errors`. Empty for non-validation errors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    non_field_errors: Vec<String>,
    /// Operator-facing summary. Used for 404 / 401 / 403 / 500
    /// where there's no field-level shape. Empty on validation
    /// errors.
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
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
    /// 400 — DB constraint violation reshaped into DRF-style
    /// field-level errors. Lets clients render
    /// `{ category: ["Referenced row does not exist."] }` next to
    /// the offending input instead of guessing from an opaque
    /// 500. Sourced from FK / UNIQUE / NOT NULL / CHECK SQL
    /// errors on both SQLite and Postgres.
    Validation {
        code: &'static str,
        field_errors: BTreeMap<String, Vec<String>>,
        non_field_errors: Vec<String>,
    },
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
        // Plain sqlx errors land here only from the non-write
        // paths (filter / count / delete). Writes go through
        // `WriteError`, which has its own translator below.
        if matches!(e, sqlx::Error::Protocol(_)) {
            return Self::BadInput(e.to_string());
        }
        Self::Sqlx(e)
    }
}

impl From<umbra::orm::DynError> for ApiError {
    fn from(e: umbra::orm::DynError) -> Self {
        // gaps2 #12: `DynError` is now a real enum (was an alias
        // for `sqlx::Error`). Route each variant to the right
        // translator so the structured `WriteError` keeps its
        // per-field map all the way to the response body.
        match e {
            umbra::orm::DynError::Write(w) => Self::from(w),
            umbra::orm::DynError::Sqlx(s) => Self::from(s),
        }
    }
}

impl From<umbra::orm::write::WriteError> for ApiError {
    fn from(e: umbra::orm::write::WriteError) -> Self {
        use umbra::orm::write::WriteError;
        // True infrastructure / serialization failures (raw
        // sqlx::Error not classified as a constraint, JSON
        // serialization failure, NotAnObject) bubble out as 500
        // via the `Sqlx` path. Everything else is a 400 with the
        // structured WriteError shape rendered into the DRF-flat
        // body via `field_errors()` + `non_field_errors()`.
        if let WriteError::Sqlx(sqlx_err) = &e {
            return Self::Sqlx(sqlx_err_clone(sqlx_err));
        }
        if !e.is_validation() {
            return Self::Sqlx(sqlx::Error::Protocol(e.to_string()));
        }
        Self::Validation {
            code: e.code(),
            field_errors: e.field_errors(),
            non_field_errors: e.non_field_errors(),
        }
    }
}

/// `sqlx::Error` isn't `Clone`; we own the WriteError by value
/// from `?` so we need to recreate the inner sqlx::Error for the
/// `ApiError::Sqlx(...)` arm. Stringify via Display — we're
/// already on the 500 path; preserving the exact variant
/// matters less than getting a usable trace.
fn sqlx_err_clone(e: &sqlx::Error) -> sqlx::Error {
    sqlx::Error::Protocol(e.to_string())
}

impl umbra::web::IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Validation errors take the DRF-flat field shape; the
        // catch-all path below covers the single-message envelope.
        if let ApiError::Validation {
            code,
            field_errors,
            non_field_errors,
        } = self
        {
            let body = ApiErrorBody {
                code,
                field_errors,
                non_field_errors,
                error: String::new(),
                hint: None,
                available: Vec::new(),
            };
            return (StatusCode::BAD_REQUEST, Json(body)).into_response();
        }

        let (status, code, msg) = match self {
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m),
            ApiError::BadInput(m) => (StatusCode::BAD_REQUEST, "bad_input", m),
            ApiError::Validation { .. } => unreachable!("handled above"),
            ApiError::Sqlx(e) => {
                // WEB-5: never echo raw DB error text to the client — it
                // leaks table/column names, SQL fragments and constraint
                // internals that aid an attacker. Log the detail
                // server-side; hand the caller an opaque message.
                tracing::error!(error = %e, "REST handler hit an unhandled database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database_error",
                    "internal server error".to_string(),
                )
            }
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
                code,
                field_errors: BTreeMap::new(),
                non_field_errors: Vec::new(),
                error: msg,
                hint: None,
                available: Vec::new(),
            }
        };
        (status, Json(body)).into_response()
    }
}

/// Build the JSON body for a 404 from this plugin. Dev mode
/// emits a `hint` + `available` list of every `/api/<table>/`
/// URL the plugin would actually serve; prod stays minimal.
fn enrich_404_body(msg: String, code: &'static str) -> ApiErrorBody {
    let is_dev = umbra::settings::get_opt()
        .map(|s| matches!(s.environment, umbra::Environment::Dev))
        .unwrap_or(false);

    if !is_dev {
        return ApiErrorBody {
            code,
            field_errors: BTreeMap::new(),
            non_field_errors: Vec::new(),
            error: msg,
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
        code,
        field_errors: BTreeMap::new(),
        non_field_errors: Vec::new(),
        error: msg,
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

/// The API root index — a browsable map of what this API exposes.
///
/// `resources` lists every model the plugin serves (the allow/block
/// filter applies, so hidden models never appear), each with its
/// collection + detail path. `endpoints` is every plugin's advertised
/// `api_endpoints()` (OAuth login/connect, etc.), collected by the
/// framework at build time — REST reads the core registry without
/// depending on the contributing plugins' crates. Each endpoint gets an
/// absolute `url` joined from the incoming request's origin.
async fn api_root(headers: umbra::web::HeaderMap) -> Json<Value> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let base = &cfg.base_path;

    let mut resources = Map::new();
    for meta in umbra::migrate::registered_models() {
        if !cfg.allow(&meta.table) {
            continue;
        }
        resources.insert(
            meta.table.clone(),
            serde_json::json!({
                "path": format!("{base}/{}/", meta.table),
                "detail": format!("{base}/{}/{{id}}", meta.table),
            }),
        );
    }

    let origin = request_origin(&headers);
    let endpoints: Vec<Value> = umbra::migrate::registered_api_endpoints()
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "group": e.group,
                "name": e.name,
                "method": e.method,
                "path": e.path,
                "label": e.label,
                "url": origin.as_ref().map(|o| format!("{o}{}", e.path)),
            })
        })
        .collect();

    Json(serde_json::json!({ "resources": resources, "endpoints": endpoints }))
}

/// Best-effort absolute origin (`scheme://host`) from request headers,
/// honoring `X-Forwarded-Proto` behind a proxy. `None` when there's no
/// usable `Host` header (then the API root omits absolute `url`s and a
/// client falls back to the relative `path`).
fn request_origin(headers: &umbra::web::HeaderMap) -> Option<String> {
    let host = headers.get("host")?.to_str().ok()?;
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    Some(format!("{scheme}://{host}"))
}

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
        .pk_column()
        .ok_or_else(|| ApiError::BadInput(format!("`{}` has no primary key", model.table)))
}

// `noform` filtering used to live here. It moved into
// `DynQuerySet::insert_json` / `update_json` so every consumer of
// the dynamic-write path (REST, admin, custom handlers) gets it
// for free — no boundary-layer scrubbing required.

// =========================================================================
// Handlers.
// =========================================================================

async fn list(
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: umbra::web::HeaderMap,
) -> Result<Response, ApiError> {
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

    // `?include=fk1,fk2` — expand the named FK columns into their
    // full related-row objects via one batched IN(...) per FK. The
    // parser rejects unknown / non-FK names with a 400 so clients
    // get loud feedback on typos instead of a silently-unexpanded
    // response that looks fine until they check it.
    let include = parse_include(params.get("include").map(|s| s.as_str()), &model)?;
    let fields_param = params.get("fields").map(|s| s.as_str());

    // `?format=csv` — export the filtered set with the same hard ceiling
    // as JSON list responses. This endpoint buffers rows before writing
    // CSV, so it must never bypass MAX_LIST_ROWS.
    if params.get("format").map(String::as_str) == Some("csv") {
        let csv_page = PageRequest {
            limit: MAX_LIST_ROWS,
            offset: 0,
            page: None,
        };
        let mut rows = fetch_rows(&model, None, Some(csv_page), &filter, &include).await?;
        for row in &mut rows {
            cfg.apply_overrides(&table, row);
            RestPlugin::apply_sparse_fields(row, fields_param);
        }
        return Ok(csv_response(&table, &model, &rows));
    }

    let page_req = cfg.pagination.extract_request(&params);
    let mut rows = fetch_rows(&model, None, Some(page_req), &filter, &include).await?;
    for row in &mut rows {
        cfg.apply_overrides(&table, row);
        RestPlugin::apply_sparse_fields(row, fields_param);
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
    Ok(Json(envelope).into_response())
}

/// Build a CSV download response from the fetched rows.
fn csv_response(table: &str, model: &ModelMeta, rows: &[Map<String, Value>]) -> Response {
    let csv = rows_to_csv(model, rows);
    (
        StatusCode::OK,
        [
            (
                http::header::CONTENT_TYPE,
                "text/csv; charset=utf-8".to_string(),
            ),
            (
                http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{table}.csv\""),
            ),
        ],
        csv,
    )
        .into_response()
}

/// Serialize rows to CSV. Columns follow the model's field order (only
/// those present after hide / sparse-field filtering), with any extra keys
/// (computed fields) appended in first-seen order. Object / array cells
/// render as compact JSON. The `csv` writer handles quoting + escaping.
fn rows_to_csv(model: &ModelMeta, rows: &[Map<String, Value>]) -> String {
    let mut cols: Vec<String> = Vec::new();
    for f in &model.fields {
        if rows.iter().any(|r| r.contains_key(&f.name)) {
            cols.push(f.name.clone());
        }
    }
    for r in rows {
        for k in r.keys() {
            if !cols.iter().any(|c| c == k) {
                cols.push(k.clone());
            }
        }
    }
    let mut wtr = csv::Writer::from_writer(Vec::new());
    let _ = wtr.write_record(&cols);
    for r in rows {
        let record: Vec<String> = cols.iter().map(|c| csv_cell(r.get(c))).collect();
        let _ = wtr.write_record(&record);
    }
    wtr.into_inner()
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

/// One CSV cell from a JSON value: scalars verbatim, null → empty,
/// object/array → compact JSON.
fn csv_cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(other) => other.to_string(),
    }
}

async fn retrieve(
    Path((table, id)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    headers: umbra::web::HeaderMap,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let model = allowed_model(&table)?;
    cfg.gate(&table, &Action::Retrieve, identity.as_ref())?;
    let pk = pk_column(&model)?;
    let no_filter = FilterClause::default();
    // `?include=` works the same on the retrieve path — `GET
    // /api/customer/123/?include=user` returns the customer with
    // its `user` FK expanded to the full AuthUser object. Same
    // parser, same 400-on-bad-name semantics.
    let include = parse_include(params.get("include").map(|s| s.as_str()), &model)?;
    let mut rows = fetch_rows(&model, Some((&pk.name, &id)), None, &no_filter, &include).await?;
    let Some(mut row) = rows.pop() else {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    };
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    cfg.apply_overrides(&table, &mut row);
    RestPlugin::apply_sparse_fields(&mut row, params.get("fields").map(|s| s.as_str()));
    Ok(Json(row))
}

async fn create(
    Path(table): Path<String>,
    headers: umbra::web::HeaderMap,
    Json(mut body): Json<Map<String, Value>>,
) -> Result<(StatusCode, Json<Map<String, Value>>), ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let model = allowed_model(&table)?;
    cfg.gate(&table, &Action::Create, identity.as_ref())?;

    let nested_specs = cfg.nested.get(&table).cloned().unwrap_or_default();

    // WEB-2: a hidden field must not be writable (see strip_hidden_for_write).
    cfg.strip_hidden_for_write(&table, &mut body);

    // Flat path (the common case) — unchanged, zero overhead. The ORM owns
    // pre-validation + constraint classification + noform-stripping;
    // `insert_json` returns a structured `WriteError` that
    // `From<WriteError> for ApiError` translates into a 400 with
    // field-level errors.
    if nested_specs.is_empty() {
        let mut row = umbra::orm::DynQuerySet::for_meta(&model)
            .insert_json(&body)
            .await?;
        cfg.apply_overrides(&table, &mut row);
        return Ok((StatusCode::CREATED, Json(row)));
    }

    create_nested(cfg, &table, model, &mut body, &nested_specs).await
}

/// Writable nested create (feature #58): insert the parent, then each
/// declared child array with its FK to the parent set, returning the full
/// nested object.
///
/// **Atomicity (orm_fixes #2):** the whole nested write runs on ONE
/// `umbra::db::Transaction` via `DynQuerySet::insert_json_in_tx`. The
/// parent and every child insert on the same open tx and only become
/// durable when `tx.commit()` succeeds at the end. Any failure —
/// including a process crash mid-write — leaves zero rows because the
/// transaction is never committed; sqlx rolls it back on drop. This
/// replaces the old compensating-delete handler, which could orphan a
/// parent if the process died between the parent insert and a failing
/// child (each statement auto-committed independently).
async fn create_nested(
    cfg: &RestPlugin,
    table: &str,
    model: ModelMeta,
    body: &mut Map<String, Value>,
    specs: &[(String, String)],
) -> Result<(StatusCode, Json<Map<String, Value>>), ApiError> {
    struct Pending {
        field: String,
        child: ModelMeta,
        fk: String,
        items: Vec<Value>,
    }

    // Split the nested arrays out of the parent body; resolve each child
    // model + the FK column that points back at the parent.
    let mut pending: Vec<Pending> = Vec::new();
    for (field, child_table) in specs {
        let items = match body.remove(field) {
            Some(Value::Array(a)) => a,
            None | Some(Value::Null) => Vec::new(),
            Some(_) => {
                return Err(ApiError::BadInput(format!(
                    "nested field `{field}` must be an array"
                )));
            }
        };
        if items.is_empty() {
            continue;
        }
        let child = meta_for_table(child_table)?;
        let fk = child_fk_to(&child, table)?.to_string();
        pending.push(Pending {
            field: field.clone(),
            child,
            fk,
            items,
        });
    }

    // One transaction for the parent + all children. Dropping `tx`
    // without committing rolls everything back — that's the rollback
    // safety net for any early return below.
    let mut tx = umbra::db::begin().await?;

    // Insert the parent on the transaction.
    let mut parent = umbra::orm::DynQuerySet::for_meta(&model)
        .insert_json_in_tx(body, &mut tx)
        .await?;
    let parent_pk = pk_column(&model)?.name.clone();
    let pk_value = parent.get(&parent_pk).cloned().ok_or_else(|| {
        ApiError::BadInput("nested: parent row has no primary key after insert".into())
    })?;

    // Insert each child on the same transaction. A failure returns Err,
    // `tx` drops un-committed, and the parent + earlier children roll
    // back at the DB level — no compensating deletes needed.
    let mut results: Vec<(String, Vec<Value>)> = Vec::new();
    for p in pending {
        let mut created = Vec::with_capacity(p.items.len());
        for item in p.items {
            let Value::Object(mut child_body) = item else {
                return Err(ApiError::BadInput(format!(
                    "items in nested `{}` must be objects",
                    p.field
                )));
            };
            // Set the child's FK to the just-created (uncommitted)
            // parent — `insert_json_in_tx` validates the FK against the
            // open tx, so it resolves even though the parent isn't
            // committed yet.
            child_body.insert(p.fk.clone(), pk_value.clone());
            let mut crow = umbra::orm::DynQuerySet::for_meta(&p.child)
                .insert_json_in_tx(&child_body, &mut tx)
                .await?;
            cfg.apply_overrides(&p.child.table, &mut crow);
            created.push(Value::Object(crow));
        }
        results.push((p.field, created));
    }

    // Commit only after every insert succeeded.
    tx.commit().await?;

    cfg.apply_overrides(table, &mut parent);
    for (field, children) in results {
        parent.insert(field, Value::Array(children));
    }
    Ok((StatusCode::CREATED, Json(parent)))
}

/// Resolve a child model's `ModelMeta` by table (no allow-gate — nested
/// children are declared by the developer, not exposed as a top-level
/// resource).
fn meta_for_table(table: &str) -> Result<ModelMeta, ApiError> {
    for plugin in umbra::migrate::registered_plugins() {
        for m in umbra::migrate::models_for_plugin(&plugin) {
            if m.table == table {
                return Ok(m);
            }
        }
    }
    Err(ApiError::BadInput(format!(
        "nested: unknown child table `{table}`"
    )))
}

/// The child column whose foreign key targets `parent_table`. Errors when
/// there are zero or multiple such columns (the latter is ambiguous).
fn child_fk_to<'a>(child: &'a ModelMeta, parent_table: &str) -> Result<&'a str, ApiError> {
    let mut found: Option<&str> = None;
    for c in &child.fields {
        if c.fk_target.as_deref() == Some(parent_table) {
            if found.is_some() {
                return Err(ApiError::BadInput(format!(
                    "nested: `{}` has multiple FKs to `{}` — ambiguous",
                    child.table, parent_table
                )));
            }
            found = Some(c.name.as_str());
        }
    }
    found.ok_or_else(|| {
        ApiError::BadInput(format!(
            "nested: `{}` has no foreign key to `{}`",
            child.table, parent_table
        ))
    })
}

async fn update(
    Path((table, id)): Path<(String, String)>,
    headers: umbra::web::HeaderMap,
    Json(mut body): Json<Map<String, Value>>,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let model = allowed_model(&table)?;
    cfg.gate(&table, &Action::Update, identity.as_ref())?;
    let pk = pk_column(&model)?;

    // WEB-2: a hidden field must not be writable (see strip_hidden_for_write).
    cfg.strip_hidden_for_write(&table, &mut body);

    // 404 if the target row doesn't exist before we attempt the UPDATE.
    let no_filter = FilterClause::default();
    let existing = fetch_rows(&model, Some((&pk.name, &id)), None, &no_filter, &[]).await?;
    if existing.is_empty() {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    }

    // PATCH-style update: only the columns supplied in the body are
    // written, primary key never. The ORM's `update_json` owns
    // validation + constraint classification; `From<WriteError>
    // for ApiError` handles the 400 translation.
    umbra::orm::DynQuerySet::for_meta(&model)
        .filter_eq_string(&pk.name, &id)
        .update_json(&body)
        .await?;
    let mut rows = fetch_rows(&model, Some((&pk.name, &id)), None, &no_filter, &[]).await?;
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

    // Validate the request body against the action's declared input schema
    // (feature #60), before the handler runs. A mismatch is a 400.
    if let Some(schema) = &def.input_schema {
        let errors = validate_against_schema(schema, &ctx.body);
        if !errors.is_empty() {
            return Err(ApiError::BadInput(format!(
                "request body does not match the action schema: {}",
                errors.join("; ")
            )));
        }
    }

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
    include: &[String],
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
        if let Some(req) = page {
            // PERF-1: a list request must never issue an unbounded
            // `SELECT * FROM table` that buffers a whole (possibly
            // million-row) table into RAM — a DoS surface, and worse now
            // that the endpoint is anonymously readable by default. When
            // the paginator asks for "everything" (`NoPagination` →
            // `limit u64::MAX`) we still clamp to a hard safety ceiling.
            // A real bounded paginator passes a concrete limit, used as-is.
            let effective_limit = req.limit.min(MAX_LIST_ROWS);
            qs = qs.limit(effective_limit).offset(req.offset);
        }
    }

    // `?include=fk1,fk2` → expand those FK columns into their full
    // related-row objects via one batched IN(...) per FK. Names
    // here have already been validated against the model's FK
    // columns by `parse_include` upstream; passing an empty slice
    // is a no-op.
    if !include.is_empty() {
        qs = qs.select_related_dyn(include);
    }

    let rows = qs.fetch_as_json().await?;
    Ok(rows)
}

/// Parse `?include=fk1,fk2,fk3` against the model's FK columns.
/// Returns `Ok(Vec)` of validated FK names on success, `Err(ApiError)`
/// with a 400 + per-name reason on the first unknown / non-FK name.
/// Unknown names fail loudly (unlike the silent-drop on `?fields=`)
/// because an unknown include is almost always a typo or a stale
/// client expectation — silently ignoring it would let the caller
/// think the field was expanded when it wasn't.
fn parse_include(raw: Option<&str>, model: &ModelMeta) -> Result<Vec<String>, ApiError> {
    /// Hard cap on `?include=` chain depth. Pathologically deep
    /// chains (`?include=a.b.c.d.e.f`) are almost always a typo —
    /// fail loud rather than fan out a 6-query chain on a typo'd
    /// param. Matches the gap2 #18 spec recommendation.
    const MAX_DEPTH: usize = 3;

    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let registered = umbra::migrate::registered_models();
    let mut out: Vec<String> = Vec::new();
    for token in raw.split(',') {
        let name = token.trim();
        if name.is_empty() {
            continue;
        }
        // Accept both `.` (URL-natural) and `__` (Django/DRF
        // muscle-memory) as hop separators (gap2 #18). Mixed
        // separators in one token flatten the same way; the
        // canonical internal form is dotted.
        let canonical = name.replace("__", ".");
        let hops: Vec<&str> = canonical.split('.').filter(|s| !s.is_empty()).collect();
        if hops.is_empty() {
            continue;
        }
        if hops.len() > MAX_DEPTH {
            return Err(ApiError::BadInput(format!(
                "?include=: chain `{name}` exceeds max depth of {MAX_DEPTH} hops"
            )));
        }
        // Walk the chain, validating each hop against the FK
        // graph. Reject the whole token on the first failure so
        // the client gets the exact resolved chain that broke (not
        // a silent drop, which hides typos).
        let mut current_table: String = model.table.clone();
        let mut hop_idx = 0;
        for hop in &hops {
            let meta_owned: Option<ModelMeta>;
            let meta_ref: &ModelMeta = if hop_idx == 0 {
                model
            } else {
                meta_owned = registered
                    .iter()
                    .find(|m| m.table == current_table)
                    .cloned();
                match meta_owned.as_ref() {
                    Some(m) => m,
                    None => {
                        return Err(ApiError::BadInput(format!(
                            "?include=: model for table `{current_table}` is not registered \
                             (resolving chain `{canonical}` at hop `{hop}`)"
                        )));
                    }
                }
            };
            let Some(col) = meta_ref.fields.iter().find(|c| &c.name == hop) else {
                return Err(ApiError::BadInput(format!(
                    "?include=: unknown field `{hop}` on `{}` (resolving chain `{canonical}`)",
                    meta_ref.table
                )));
            };
            let Some(target) = col.fk_target.clone() else {
                return Err(ApiError::BadInput(format!(
                    "?include=: field `{hop}` on `{}` is not a foreign key \
                     (resolving chain `{canonical}`)",
                    meta_ref.table
                )));
            };
            current_table = target;
            hop_idx += 1;
        }
        if !out.iter().any(|n| n == &canonical) {
            out.push(canonical);
        }
    }
    Ok(out)
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

// The `noform` filtering tests that used to live here moved
// into the ORM alongside the logic — see
// `crates/umbra-core/src/orm/dynamic.rs`'s test module. The
// REST plugin no longer scrubs the body before dispatch; the
// dynamic-write seam does.

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

    /// WEB-1: the default block-list also covers the authorization model
    /// (`permissions_*`), the background-job queue (`task_row`) and the
    /// admin audit trail — none should be served unless explicitly
    /// `expose`d. A normal business table stays served.
    #[test]
    fn default_plugin_blocks_permissions_tasks_and_audit_tables() {
        let p = RestPlugin::new();
        for blocked in [
            "permissions_permission",
            "permissions_contenttype",
            "permissions_group",
            "permissions_usergroup",
            "permissions_userpermission",
            "task_row",
            "admin_audit_log",
        ] {
            assert!(!p.allow(blocked), "{blocked} must be blocked by default");
        }
        assert!(p.allow("product"), "business tables stay served");
        // Opt-in still works for the new entries.
        assert!(RestPlugin::new().expose(["task_row"]).allow("task_row"));
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

    #[test]
    fn is_field_hidden_covers_plugin_and_resource_hides() {
        // Plugin-level `.hide(...)` and resource-level
        // `ResourceConfig::hide(...)` both land in `self.hidden`, so
        // `is_field_hidden` must report true for either source — and
        // false for a visible field / unknown table.
        let p = RestPlugin::new()
            .hide("account", "password_hash")
            .resource(crate::ResourceConfig::new("account").hide("api_token"));
        assert!(p.is_field_hidden("account", "password_hash"));
        assert!(p.is_field_hidden("account", "api_token"));
        assert!(!p.is_field_hidden("account", "label"));
        // password_hash is in HARD_DENIED_FIELDS, so is_field_hidden returns
        // true for it on ANY table — even one that never called .hide().
        // gaps2 #75: hard denylist is un-overridable.
        assert!(p.is_field_hidden("other", "password_hash"));
    }

    #[test]
    fn is_hidden_defaults_false_when_config_unset() {
        // The lib's own test binary never boots an App, so the CONFIG
        // OnceLock is empty here — `is_hidden` must default to false so
        // a spec built before the REST plugin's `routes()` runs
        // describes the "nothing hidden" shape rather than panicking.
        assert!(!crate::is_hidden("anything", "any_field"));
    }
}

#[cfg(test)]
mod sparse_fields_unit {
    use super::RestPlugin;
    use serde_json::{Map, Value, json};

    fn row() -> Map<String, Value> {
        // Customer-shaped row AFTER include=user has run — user is
        // an Object, not the integer FK it would otherwise be.
        let mut m = Map::new();
        m.insert("id".into(), json!(1));
        m.insert("phone".into(), json!("+15555550100"));
        m.insert("loyalty_points".into(), json!(50));
        m.insert(
            "user".into(),
            json!({
                "id": 7,
                "username": "alice",
                "email": "alice@example.com",
                "is_staff": false
            }),
        );
        m
    }

    #[test]
    fn plain_tokens_filter_top_level_only() {
        let mut r = row();
        RestPlugin::apply_sparse_fields(&mut r, Some("id,phone"));
        let mut keys: Vec<&str> = r.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["id", "phone"]);
    }

    #[test]
    fn plain_user_token_keeps_full_nested_object() {
        let mut r = row();
        RestPlugin::apply_sparse_fields(&mut r, Some("id,user"));
        let user = r.get("user").unwrap().as_object().unwrap();
        assert_eq!(user.len(), 4, "full nested user object preserved");
        assert!(user.contains_key("email"));
    }

    #[test]
    fn dotted_tokens_filter_the_nested_object() {
        let mut r = row();
        RestPlugin::apply_sparse_fields(&mut r, Some("id,user.id,user.username"));
        assert!(r.contains_key("id"));
        let user = r.get("user").unwrap().as_object().unwrap();
        let mut keys: Vec<&str> = user.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["id", "username"]);
        // root has not pulled extra columns
        assert!(!r.contains_key("phone"));
        assert!(!r.contains_key("loyalty_points"));
    }

    #[test]
    fn dotted_without_plain_implicitly_includes_parent() {
        let mut r = row();
        RestPlugin::apply_sparse_fields(&mut r, Some("user.id"));
        // root.id NOT pulled — per-relation projection without
        // polluting the root. This is the design goal of
        // dot-notation: ask for user's id without forcing the
        // root id along.
        assert!(!r.contains_key("id"));
        let user = r.get("user").unwrap().as_object().unwrap();
        assert_eq!(
            user.keys().cloned().collect::<Vec<_>>(),
            vec!["id".to_string()],
        );
    }

    #[test]
    fn mixed_plain_and_dotted_for_same_parent_dotted_wins() {
        // user appears as a plain token AND as user.id — most-
        // specific wins, the nested object gets filtered.
        let mut r = row();
        RestPlugin::apply_sparse_fields(&mut r, Some("user,user.id"));
        let user = r.get("user").unwrap().as_object().unwrap();
        assert_eq!(
            user.keys().cloned().collect::<Vec<_>>(),
            vec!["id".to_string()],
            "dotted token overrides plain — nested object filtered",
        );
    }

    #[test]
    fn dotted_against_integer_fk_silently_skips_nested_filter() {
        // Caller wrote user.id but did not ?include=user, so user
        // is still the integer FK. The nested filter step
        // tolerates that — the integer survives unchanged.
        let mut r = Map::new();
        r.insert("id".into(), json!(1));
        r.insert("user".into(), json!(7));
        RestPlugin::apply_sparse_fields(&mut r, Some("id,user.id"));
        assert_eq!(r.get("user"), Some(&json!(7)));
    }

    #[test]
    fn unknown_tokens_silently_dropped() {
        let mut r = row();
        RestPlugin::apply_sparse_fields(&mut r, Some("id,nonsense,user.also_not_real"));
        // id kept, user kept (parent of an unknown nested key
        // still survives the root retain — the nested filter
        // just removes every key on user since none matched).
        assert!(r.contains_key("id"));
        let user = r.get("user").unwrap().as_object().unwrap();
        assert!(user.is_empty(), "nested object filtered down to nothing");
    }

    #[test]
    fn double_underscore_separator_equals_dot() {
        // `user__id` must behave exactly like `user.id` (gap2 #18
        // normalisation carried over to ?fields=).
        let mut a = row();
        RestPlugin::apply_sparse_fields(&mut a, Some("user__id"));
        let mut b = row();
        RestPlugin::apply_sparse_fields(&mut b, Some("user.id"));
        assert_eq!(a, b, "__ and . forms produce identical projections");
        let user = a.get("user").unwrap().as_object().unwrap();
        assert_eq!(
            user.keys().cloned().collect::<Vec<_>>(),
            vec!["id".to_string()]
        );
    }

    fn deep_row() -> Map<String, Value> {
        // a.b.c shape: `?include=a.b` hydrated nested objects.
        let mut m = Map::new();
        m.insert("id".into(), json!(1));
        m.insert(
            "a".into(),
            json!({
                "id": 10,
                "label": "outer",
                "b": { "id": 20, "name": "inner", "extra": "drop-me" }
            }),
        );
        m
    }

    #[test]
    fn multi_hop_prunes_each_level() {
        let mut r = deep_row();
        RestPlugin::apply_sparse_fields(&mut r, Some("a__b__name"));
        // root: only `a` survives
        assert_eq!(r.keys().cloned().collect::<Vec<_>>(), vec!["a".to_string()]);
        let a = r.get("a").unwrap().as_object().unwrap();
        // a: only `b` survives (label + id dropped)
        assert_eq!(a.keys().cloned().collect::<Vec<_>>(), vec!["b".to_string()]);
        let b = a.get("b").unwrap().as_object().unwrap();
        // b: only `name` survives (id + extra dropped)
        assert_eq!(
            b.keys().cloned().collect::<Vec<_>>(),
            vec!["name".to_string()]
        );
        assert_eq!(b.get("name"), Some(&json!("inner")));
    }

    #[test]
    fn multi_hop_keeps_sibling_at_intermediate_level() {
        let mut r = deep_row();
        // Ask for a.label (leaf one level down) AND a.b.id (two down).
        RestPlugin::apply_sparse_fields(&mut r, Some("a.label,a.b.id"));
        let a = r.get("a").unwrap().as_object().unwrap();
        let mut keys: Vec<&str> = a.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["b", "label"]);
        let b = a.get("b").unwrap().as_object().unwrap();
        assert_eq!(
            b.keys().cloned().collect::<Vec<_>>(),
            vec!["id".to_string()]
        );
    }

    #[test]
    fn nested_path_against_integer_fk_leaves_int_untouched() {
        // a.b.c requested but `a` is still the raw integer FK
        // (no ?include=) — no crash, the integer survives.
        let mut r = Map::new();
        r.insert("id".into(), json!(1));
        r.insert("a".into(), json!(7));
        RestPlugin::apply_sparse_fields(&mut r, Some("id,a__b__c"));
        assert_eq!(r.get("a"), Some(&json!(7)));
        assert_eq!(r.get("id"), Some(&json!(1)));
    }

    #[test]
    fn depth_cap_truncates_pathological_paths() {
        // a.b.c.d → capped to a.b.c; the 4th hop is ignored, so
        // pruning stops at c and keeps c's whole subtree.
        let mut m = Map::new();
        m.insert(
            "a".into(),
            json!({ "b": { "c": { "d": 1, "e": 2 }, "other": 3 } }),
        );
        RestPlugin::apply_sparse_fields(&mut m, Some("a__b__c__d"));
        let c = m["a"]["b"]["c"].as_object().unwrap();
        // c kept whole (cap stopped descent before pruning into c)
        assert!(c.contains_key("d") && c.contains_key("e"));
        // `other` (sibling of c under b) was pruned away
        let b = m["a"]["b"].as_object().unwrap();
        assert_eq!(b.keys().cloned().collect::<Vec<_>>(), vec!["c".to_string()]);
    }
}
