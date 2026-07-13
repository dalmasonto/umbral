//! umbral-rest — auto-generated JSON REST API over umbral models.
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
//! `umbral_migrations`. Letting `/api/auth_user/` exist would leak
//! password hashes; the default block-list is the safe shape.
//!
//! Tighten with `RestPlugin::new().include_only(["article"])` or
//! loosen with `.exclude(["sensitive_thing"])`. The builder is
//! chainable.
//!
//! ## Auth
//!
//! v1 ships no built-in auth gate — every exposed route is open.
//! Apps that need authenticated CRUD wrap the umbral-rest router
//! with a tower layer (or write their own handler that delegates
//! after the auth check). A future round adds optional
//! `RestPlugin::require_staff()` that mirrors umbral-admin's Basic
//! Auth gate.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use serde::Serialize;
use serde_json::{Map, Value};
use umbral::migrate::ModelMeta;
use umbral::prelude::*;
use umbral::web::{IntoResponse, Json, Path, Query, Response, StatusCode};

pub mod filtering;
pub(crate) use filtering::{FilterClause, parse_filters, parse_ordering, parse_search};

pub mod pagination;
pub use pagination::{
    LimitOffsetPagination, NoPagination, PageNumberPagination, PageRequest, Pagination,
    PaginationField, PaginationScalar, PaginationSchema, PaginationStyle,
};

pub mod resource;
pub use resource::{
    ActionContext, ActionError, ActionScope, RequestContext, ResourceConfig, ScopeDecision,
};

pub mod versioning;
pub use versioning::{VersioningConfig, VersioningScheme, version_from_headers};

pub mod auth;
pub use auth::{
    Authentication, ChainAuthentication, FnAuthentication, Identity, NoAuthentication,
    parse_basic_credentials,
};

pub mod permission;
pub use permission::{
    Action, AllowAny, AndPermission, IsAuthenticated, IsAuthenticatedOrReadOnly, IsStaff,
    OrPermission, Permission, PermissionError, ReadOnly,
};

pub mod throttle;
pub use throttle::{
    AnonRateThrottle, ScopedRateThrottle, Throttle, ThrottleContext, ThrottleDenied,
    UserRateThrottle,
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
    "umbral_migrations",
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
/// The list itself now lives in **core** (`umbral::orm::HARD_DENIED_FIELDS`),
/// not here. It was a REST-owned constant, which made `password_hash` safe
/// only if you happened to mount REST — `umbral-graphql` exposed every column
/// of every model it was pointed at and inherited none of this. Secrecy is a
/// property of the data, not of the transport, so the rule belongs where every
/// plugin picks it up for free. REST keeps enforcing it; it just no longer
/// *owns* it.
///
/// Gap: gaps2 #75.
use umbral::orm::HARD_DENIED_FIELDS;

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
/// The applied form of a resource's object-level row scope (audit_2 H1/P2),
/// resolved per-request by [`RestPlugin::object_scope`].
enum ObjectScopeOutcome {
    /// No scoping — every row is reachable.
    Unconstrained,
    /// AND this condition into every CRUD query for the request.
    Filter(sea_query::Condition),
    /// No rows are in scope: list returns an empty page, and
    /// retrieve/update/destroy return `404`.
    DenyAll,
}

#[derive(Clone)]
pub struct RestPlugin {
    /// `Cache-Control` for every REST JSON response (gaps3 #36).
    ///
    /// Defaults to `no-store`. A `200 application/json` carrying NO cache
    /// directive is *heuristically cacheable* by browsers and shared proxies
    /// (RFC 9111 §4.2.2) — so a mutable API served with no header can be replayed
    /// stale: a refetch right after a mutation can return the pre-mutation
    /// snapshot. An opinionated framework should not ship that by default.
    cache_control: String,
    /// Per-resource `Cache-Control` overrides (gaps3 #36).
    cache_controls: HashMap<String, String>,
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
    /// `#[umbral(private)]` columns a resource can unlock, and for whom. `(table, field, fn)`.
    private_unlocks: Vec<(String, String, crate::resource::PrivateFn)>,
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
    /// Throttles that apply to EVERY resource, run after auth and before
    /// the handler. A Vec because throttles stack — all must pass, the
    /// first to deny wins (429). Empty by default: no limits unless the
    /// app opts in via [`Self::default_throttle`]. Per-table throttles
    /// ([`Self::throttles`]) run in addition to these.
    default_throttles: Vec<Arc<dyn Throttle>>,
    /// Per-table throttles, keyed by table name. Merged from
    /// [`ResourceConfig::throttle`]. A table's throttles run alongside
    /// (after) the `default_throttles` — both sets must pass.
    throttles: HashMap<String, Vec<Arc<dyn Throttle>>>,
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
    /// Tables that opted IN to bulk endpoints via `ResourceConfig::bulk()`
    /// (gaps2 #82). A table NOT in this set keeps the original behaviour:
    /// `POST` of a JSON array is rejected, and no collection-level
    /// `PATCH`/`DELETE` mounts. A table in the set enables transactional
    /// bulk create/update/delete, each gated by the same
    /// permission/throttle/denylist/blocked-table checks as the
    /// single-object handlers.
    bulk: std::collections::HashSet<String>,
    /// Object-level row-scoping hooks per table (audit_2 H1/P2). A table with
    /// a hook restricts every built-in CRUD action to the rows the caller may
    /// access. Merged from `ResourceConfig::scope(...)` / `.owned_by(...)`.
    object_scopes: HashMap<String, crate::resource::ObjectScopeFn>,
    /// gaps3 #16: per-table owner column filled from the identity on create.
    owner_fields: HashMap<String, String>,
    /// gaps3 #29 item 2 — child table -> (parent table, fk column). A table listed
    /// here is reachable ONLY at `/api/{parent}/{parent_id}/{table}`; its flat route
    /// 404s, because a nested resource that is also reachable flat is not scoped, it
    /// merely has a scoped-looking URL.
    unders: HashMap<String, (String, String)>,
    /// Gap 107: base URL prefix for all REST endpoints. Default
    /// `/api`. Set via `RestPlugin::at("/v1")`. Always normalised
    /// to one leading slash, no trailing slash.
    base_path: String,
    /// Opt-in API versioning (gaps2 #82). `None` (the default) means no
    /// versioning: routes mount at `{base_path}/<table>/` and
    /// `RequestContext::version` is always `None`. `Some(cfg)` selects a
    /// scheme (URL-path or accept-header) — see [`RestPlugin::versioning`].
    versioning: Option<VersioningConfig>,
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

    /// M-4 boot check: would an *anonymous* caller get read access to
    /// `table`? True when the table is served (`allow`) and its effective
    /// permission lets an anonymous caller `List` or `Retrieve`. Pure over
    /// the config (no DB, no request), so the boot-time security warning is
    /// unit-testable. Note the default `ReadOnly` returns `true` here — that
    /// is exactly the exposure M-4 wants surfaced.
    fn allows_anonymous_read(&self, table: &str) -> bool {
        if !self.allow(table) {
            return false;
        }
        let perm = self.permission_for(table);
        perm.check(&Action::List, None).is_ok() || perm.check(&Action::Retrieve, None).is_ok()
    }

    /// M-5 boot check: is a throttle configured *anywhere*? False only when
    /// neither a plugin-wide `default_throttle` nor any per-resource
    /// `.throttle(...)` is set. The write-without-throttle warning fires
    /// only in that state; any throttle at all suppresses it.
    fn has_no_throttle(&self) -> bool {
        self.default_throttles.is_empty() && self.throttles.values().all(|v| v.is_empty())
    }

    /// M-5 boot check: does `table` accept writes from *some* caller? A
    /// resource whose effective permission denies Create/Update/Delete to
    /// everyone (e.g. `ReadOnly`) is read-only and needs no write throttle;
    /// anything that lets an anonymous or staff caller write counts as a
    /// write endpoint. Pure over the config so the warning is testable.
    fn permits_writes(&self, table: &str) -> bool {
        if !self.allow(table) {
            return false;
        }
        let perm = self.permission_for(table);
        let staff = Identity::user("boot-check").staff();
        for action in [Action::Create, Action::Update, Action::Delete] {
            if perm.check(&action, None).is_ok() || perm.check(&action, Some(&staff)).is_ok() {
                return true;
            }
        }
        false
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

    /// Add a throttle that applies to EVERY resource. Run after auth
    /// resolves and before the handler; on the first denial
    /// the request returns **429 Too Many Requests** with a `Retry-After`
    /// header and a `{"detail":"Request was throttled.","retry_after":N}`
    /// body.
    ///
    /// Throttles **stack**: call this more than once (or pair with a
    /// per-resource [`ResourceConfig::throttle`]) and ALL of them must
    /// pass. Throttling is OFF by default — a `RestPlugin` with no
    /// throttle imposes no limits, so adding the plugin never surprises an
    /// existing API with a rate cap.
    ///
    /// ```ignore
    /// RestPlugin::default()
    ///     .default_throttle(AnonRateThrottle::new("100/hour"))
    ///     .default_throttle(UserRateThrottle::new("1000/day"))
    /// ```
    pub fn default_throttle<T: Throttle>(mut self, throttle: T) -> Self {
        self.default_throttles.push(Arc::new(throttle));
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

    /// HTTP method tokens currently mounted for `(table, kind)`, honoring
    /// the `.views(...)` scope and (for the collection) the `.bulk()`
    /// opt-in. OPTIONS is omitted — it's always available, so callers
    /// prepend it. This is the single source of truth for both the
    /// `OPTIONS` `Allow` header and the `Allow` header on a `405`.
    ///
    /// Note this reflects what's *mounted*, never the per-identity
    /// permission class: `Allow` is a property of the resource, not of
    /// who's asking. A resource that should advertise only `GET` says so
    /// with `.views([List, Retrieve])`, not with a `ReadOnly` permission.
    fn exposed_methods(&self, table: &str, kind: EndpointKind) -> Vec<&'static str> {
        let mut v = Vec::new();
        match kind {
            EndpointKind::Collection => {
                if self.view_exposed(table, &Action::List) {
                    v.push("GET");
                }
                if self.view_exposed(table, &Action::Create) {
                    v.push("POST");
                }
                // Collection PATCH/DELETE are the bulk update/delete
                // endpoints — only mounted when the resource opted in.
                if self.bulk.contains(table) {
                    if self.view_exposed(table, &Action::Update) {
                        v.push("PATCH");
                    }
                    if self.view_exposed(table, &Action::Delete) {
                        v.push("DELETE");
                    }
                }
            }
            EndpointKind::Detail => {
                if self.view_exposed(table, &Action::Retrieve) {
                    v.push("GET");
                }
                if self.view_exposed(table, &Action::Update) {
                    v.push("PUT");
                    v.push("PATCH");
                }
                if self.view_exposed(table, &Action::Delete) {
                    v.push("DELETE");
                }
            }
        }
        v
    }

    /// Authenticate + permission-check for one (table, action). The
    /// caller passes the resolved identity (already pulled from the
    /// auth backend at request entry) and the `kind` of endpoint the
    /// request hit (collection vs detail). Returns the right `ApiError`
    /// variant for the failure mode so the handler's `?` operator
    /// surfaces 401 / 403 / 404 / 405 with the right shape.
    ///
    /// `.views(...)` scope filters built-in CRUD actions. When the
    /// requested action is scoped out we distinguish two cases:
    /// - the endpoint still serves *some* verb → `405 Method Not
    ///   Allowed` with an `Allow` header (the URI exists, this method
    ///   doesn't), matching RFC 7231;
    /// - the endpoint serves *nothing* (e.g. `views([List])` makes the
    ///   detail URI serve no method) → `404` (the URI isn't served).
    fn gate(
        &self,
        table: &str,
        action: &Action,
        kind: EndpointKind,
        identity: Option<&Identity>,
    ) -> Result<(), ApiError> {
        if !self.view_exposed(table, action) {
            let methods = self.exposed_methods(table, kind);
            if methods.is_empty() {
                return Err(ApiError::NotFound(format!(
                    "action `{action:?}` is not exposed on `/api/{table}/`"
                )));
            }
            let allow = std::iter::once("OPTIONS")
                .chain(methods)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ApiError::MethodNotAllowed { allow });
        }
        match self.permission_for(table).check(action, identity) {
            Ok(()) => Ok(()),
            Err(PermissionError::Unauthenticated) => Err(ApiError::Unauthenticated),
            Err(PermissionError::Forbidden) => Err(ApiError::Forbidden),
        }
    }

    /// Resolve this request's object-level row scope (audit_2 H1/P2). Consults
    /// the resource's `scope`/`owned_by` hook with the caller's identity and
    /// turns the [`ScopeDecision`] into an [`ObjectScopeOutcome`] the CRUD
    /// handlers apply: `Unconstrained` (no hook / `All`), a `Filter` condition
    /// ANDed into every query, or `DenyAll` (list → empty, detail → 404).
    async fn object_scope(
        &self,
        table: &str,
        identity: Option<&Identity>,
        parent: Option<&(String, String)>,
    ) -> ObjectScopeOutcome {
        // gaps3 #29 item 2: the parent scope rides the SAME seam as the row-level
        // `scope`/`owned_by` hook, so the two AND together instead of racing. A
        // separate filter applied somewhere else in each handler is how one of the five
        // ends up missing it.
        let own = self.object_scope_hook(table, identity).await;
        let Some((fk_column, parent_id)) = parent else {
            return own;
        };
        let Some(meta) = model_meta(table) else {
            return own;
        };
        // An id that cannot be coerced to the FK's type (`/api/fixture/abc/selection`
        // where the pk is a BigInt) matches nothing — and must not fall through to
        // "unscoped", which would hand back every child row in the table.
        let Some(parent_cond) = umbral::orm::typed_eq_condition(&meta, fk_column, parent_id) else {
            return ObjectScopeOutcome::DenyAll;
        };
        match own {
            ObjectScopeOutcome::DenyAll => ObjectScopeOutcome::DenyAll,
            ObjectScopeOutcome::Unconstrained => ObjectScopeOutcome::Filter(parent_cond),
            ObjectScopeOutcome::Filter(cond) => {
                ObjectScopeOutcome::Filter(sea_query::Condition::all().add(cond).add(parent_cond))
            }
        }
    }

    /// The resource's own `scope` / `owned_by` decision, before parent scoping.
    async fn object_scope_hook(
        &self,
        table: &str,
        identity: Option<&Identity>,
    ) -> ObjectScopeOutcome {
        let Some(hook) = self.object_scopes.get(table) else {
            return ObjectScopeOutcome::Unconstrained;
        };
        match hook(identity.cloned()).await {
            crate::resource::ScopeDecision::All => ObjectScopeOutcome::Unconstrained,
            crate::resource::ScopeDecision::None => ObjectScopeOutcome::DenyAll,
            crate::resource::ScopeDecision::Restrict(pairs) => {
                if pairs.is_empty() {
                    return ObjectScopeOutcome::Unconstrained;
                }
                let mut cond = sea_query::Condition::all();
                for (col, val) in pairs {
                    cond = cond.add(sea_query::Expr::col(sea_query::Alias::new(col)).eq(val));
                }
                ObjectScopeOutcome::Filter(cond)
            }
            crate::resource::ScopeDecision::RestrictIn(col, values) => {
                // Empty membership → NO rows, never all rows. A caller who
                // belongs to no club must see nothing; defaulting to
                // `Unconstrained` here (as the empty-`Restrict` arm above does,
                // where an empty pair list means "the hook added no constraint")
                // would turn "you joined nothing" into "you see everything".
                if values.is_empty() {
                    return ObjectScopeOutcome::DenyAll;
                }
                let cond = sea_query::Condition::all()
                    .add(sea_query::Expr::col(sea_query::Alias::new(col)).is_in(values));
                ObjectScopeOutcome::Filter(cond)
            }
        }
    }

    /// Run every applicable throttle for `(table, action)` after auth has
    /// resolved, before the handler. Returns
    /// `Err(ApiError::Throttled { retry_after })` on the FIRST denial so
    /// the handler's `?` surfaces a 429 with a `Retry-After` header.
    ///
    /// Both the plugin-wide `default_throttles` and the per-table
    /// `throttles` run; all must pass. The scope handed to each throttle
    /// is `"<table>:<action>"` (e.g. `"post:list"`) — that's what
    /// [`ScopedRateThrottle`] matches against.
    fn gate_throttle(
        &self,
        table: &str,
        action: &Action,
        identity: Option<&Identity>,
        client_ip: Option<&str>,
    ) -> Result<(), ApiError> {
        // Fast path: no throttle configured anywhere for this table.
        let per_table = self.throttles.get(table);
        if self.default_throttles.is_empty() && per_table.is_none() {
            return Ok(());
        }
        let scope = format!("{table}:{}", action_label(action));
        let ctx = ThrottleContext {
            identity,
            client_ip,
            scope: &scope,
        };
        let all = self
            .default_throttles
            .iter()
            .chain(per_table.into_iter().flatten());
        for t in all {
            if let Err(ThrottleDenied { retry_after }) = t.check(&ctx) {
                return Err(ApiError::Throttled { retry_after });
            }
        }
        Ok(())
    }
}

/// The scope-label segment for an [`Action`] — `list` / `retrieve` /
/// `create` / `update` / `delete`, or the raw name for a custom action.
/// Used to build the `"<table>:<action>"` throttle scope.
fn action_label(action: &Action) -> String {
    match action {
        Action::List => "list".to_string(),
        Action::Retrieve => "retrieve".to_string(),
        Action::Create => "create".to_string(),
        Action::Update => "update".to_string(),
        Action::Delete => "delete".to_string(),
        Action::Custom(name) => name.clone(),
    }
}

/// Resolve the caller's IP from proxy headers for throttle keying.
/// Takes the first hop of `X-Forwarded-For`, else `X-Real-IP`. Returns
/// `None` when neither resolves (direct connection, no proxy) — the
/// throttles then fall back to a shared `"unknown"` bucket, which limits
/// rather than opening a hole. Mirrors `umbral-auth`'s `client_ip` and
/// `umbral-logs`'s `resolve_ip`.
fn throttle_client_ip(headers: &umbral::web::HeaderMap) -> Option<String> {
    // audit_2 H9: derive the client IP under the framework's trusted-proxy
    // policy (`settings.trusted_proxy_hops`). With no trusted proxy configured
    // (the default) this returns `None` — `X-Forwarded-For` is client-forgeable,
    // so keying a throttle on it would let an attacker rotate the header to dodge
    // every limit. A `None` IP makes the throttle fall back to a non-IP scope.
    umbral::settings::client_ip(headers)
}

impl RestPlugin {
    pub fn new() -> Self {
        Self {
            cache_control: "no-store".to_string(),
            cache_controls: HashMap::new(),
            include_only: None,
            extra_exclude: Vec::new(),
            expose: std::collections::HashSet::new(),
            hidden: Vec::new(),
            private_unlocks: Vec::new(),
            transforms: Vec::new(),
            computed: Vec::new(),
            pagination: Arc::new(NoPagination),
            authentication: Arc::new(NoAuthentication),
            permissions: HashMap::new(),
            default_throttles: Vec::new(),
            throttles: HashMap::new(),
            default_permission: Arc::new(ReadOnly),
            view_scope: HashMap::new(),
            actions: HashMap::new(),
            filters_disabled: std::collections::HashSet::new(),
            search_disabled: std::collections::HashSet::new(),
            search_fields: HashMap::new(),
            nested: HashMap::new(),
            bulk: std::collections::HashSet::new(),
            object_scopes: HashMap::new(),
            owner_fields: HashMap::new(),
            unders: HashMap::new(),
            base_path: "/api".to_string(),
            versioning: None,
        }
    }

    /// Opt into API versioning (gaps2 #82). **Off by default** —
    /// without this call the API is unversioned (`/api/<table>/`) and
    /// [`RequestContext::version`] is always `None`.
    ///
    /// Pass a [`VersioningConfig`] built from a [`VersioningScheme`]:
    ///
    /// - [`VersioningScheme::url_path()`] — the version is a path segment
    ///   after the base path. Routes mount under `{base}/{version}/...`
    ///   for **each** allowed version, so `/api/v1/post/` and
    ///   `/api/v2/post/` both resolve when both are allowed. An unknown
    ///   version matches no route → **404**. The version is required in the
    ///   path; there is no unversioned `/api/<table>/`
    ///   fallback once this scheme is on.
    /// - [`VersioningScheme::accept_header()`] — paths stay
    ///   `/api/<table>/`; the version comes from the `Accept` header
    ///   (`application/json; version=v2`). A configurable plain header is
    ///   supported via [`VersioningScheme::header("X-API-Version")`].
    ///   Absent → `default_version`; an unknown version → **406**.
    ///
    /// The resolved version lands on [`RequestContext::version`] so
    /// handlers / `transform` / `computed` can branch on it.
    ///
    /// ```ignore
    /// // URL-path: /api/v1/post/ and /api/v2/post/, default v1
    /// RestPlugin::default().versioning(
    ///     VersioningConfig::new(VersioningScheme::url_path())
    ///         .allowed_versions(["v1", "v2"])
    ///         .default_version("v1"),
    /// )
    ///
    /// // Accept header: Accept: application/json; version=v2
    /// RestPlugin::default().versioning(
    ///     VersioningConfig::new(VersioningScheme::accept_header())
    ///         .allowed_versions(["v1", "v2"])
    ///         .default_version("v1"),
    /// )
    /// ```
    pub fn versioning(mut self, cfg: VersioningConfig) -> Self {
        self.versioning = Some(cfg);
        self
    }

    /// The configured versioning, if any. Public so `umbral-openapi` can
    /// mirror the versioned paths in the spec.
    pub fn versioning_config(&self) -> Option<&VersioningConfig> {
        self.versioning.as_ref()
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

    /// The URL prefixes the resource route-set mounts under. Just
    /// `[base_path]` normally; `[{base}/{version}, ...]` (one per allowed
    /// version) when [`VersioningScheme::UrlPath`] is configured.
    fn mount_prefixes(&self) -> Vec<String> {
        match &self.versioning {
            Some(cfg) if matches!(cfg.scheme, VersioningScheme::UrlPath) => {
                if cfg.allowed_versions.is_empty() {
                    // No allow-list with URL-path versioning would mean no
                    // routable prefix at all; fall back to the bare base so
                    // the misconfiguration is visible (every path 404s on
                    // the version segment) rather than panicking at boot.
                    vec![self.base_path.clone()]
                } else {
                    cfg.allowed_versions
                        .iter()
                        .map(|v| format!("{}/{}", self.base_path, v))
                        .collect()
                }
            }
            // Accept-header versioning keeps unversioned paths; no version
            // in the URL. No versioning at all → the plain base path.
            _ => vec![self.base_path.clone()],
        }
    }

    /// Resolve the API version for a request, given its full URL path and
    /// headers, then validate it against `allowed_versions`.
    ///
    /// - No versioning configured → `Ok(None)`.
    /// - URL-path: the segment right after the base path. Routing already
    ///   guarantees it's an allowed version (an unknown one matched no
    ///   route), so this just reads it back onto the context.
    /// - Accept-header: read from the configured header. Absent →
    ///   `default_version`. A version outside `allowed_versions` → 406.
    fn resolve_version(
        &self,
        uri_path: &str,
        headers: &umbral::web::HeaderMap,
    ) -> Result<Option<String>, ApiError> {
        let Some(cfg) = &self.versioning else {
            return Ok(None);
        };
        match &cfg.scheme {
            VersioningScheme::UrlPath => {
                // The version is the first path segment after the base path.
                let base = self.base_path.trim_matches('/');
                let rest = uri_path
                    .trim_start_matches('/')
                    .strip_prefix(base)
                    .map(|r| r.trim_start_matches('/'))
                    .unwrap_or("");
                let seg = rest.split('/').next().unwrap_or("");
                if seg.is_empty() {
                    // No version segment — only reachable if a prefix-less
                    // route matched (shouldn't with URL-path versioning).
                    Ok(cfg.default_version.clone())
                } else if cfg.is_allowed(seg) {
                    Ok(Some(seg.to_string()))
                } else {
                    // Defense in depth: routing already 404s unknown
                    // versions, so this path is unreachable in practice.
                    Err(ApiError::NotFound(format!("unknown API version `{seg}`")))
                }
            }
            VersioningScheme::AcceptHeader { header } => {
                match version_from_headers(headers, header) {
                    Some(v) if cfg.is_allowed(&v) => Ok(Some(v)),
                    Some(v) => Err(ApiError::NotAcceptable(format!(
                        "requested API version `{v}` is not supported"
                    ))),
                    None => Ok(cfg.default_version.clone()),
                }
            }
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
    ///         let user = umbral_auth::current_user(&headers).await.ok().flatten()?;
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
    /// - [`PageNumberPagination::new(page_size)`] — page-number shape.
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
    /// Override the `Cache-Control` sent on every REST JSON response.
    ///
    /// The default is `no-store`, because a JSON `200` with no directive is
    /// heuristically cacheable (RFC 9111 §4.2.2) and a mutable API served stale
    /// is a data-loss bug: a refetch right after a write can return the
    /// pre-write snapshot and clobber fresh state. Soften it globally with
    /// `private, no-cache` if you want revalidation instead of no storage, or
    /// override per-resource with [`ResourceConfig::cache_control`] for a
    /// genuinely cacheable read endpoint.
    ///
    /// ```ignore
    /// RestPlugin::default().cache_control("private, no-cache")
    /// ```
    pub fn cache_control(mut self, value: impl Into<String>) -> Self {
        self.cache_control = value.into();
        self
    }

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
    /// security reasons (`auth_user`, `session`, `umbral_migrations`).
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
    /// models, keeping serializers per app/model) and the app register
    /// them in one call instead of a `.resource(...)` per model in `main.rs`.
    ///
    /// ```ignore
    /// // plugins/blog/src/lib.rs
    /// pub fn rest_resources() -> Vec<umbral_rest::ResourceConfig> {
    ///     vec![
    ///         umbral_rest::ResourceConfig::new("post").hide("draft_notes"),
    ///         umbral_rest::ResourceConfig::new("comment"),
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
    /// pub fn rest_resource() -> umbral_rest::ResourceConfig {
    ///     umbral_rest::ResourceConfig::new("user")
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
            throttles,
            view_scope,
            actions,
            filters_disabled,
            search_disabled,
            search_fields,
            nested,
            bulk,
            scope,
            owner_field,
            cache_control,
            under,
            private_unlocks,
        } = config;
        if let Some(cc) = cache_control {
            self.cache_controls.insert(table.clone(), cc);
        }
        for field in hidden {
            self.hidden.push((table.clone(), field));
        }
        for (field, func) in private_unlocks {
            self.private_unlocks.push((table.clone(), field, func));
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
        if !throttles.is_empty() {
            // Additive: repeated `.resource(...)` calls for the same table
            // stack their throttles (all must pass), matching the way
            // `.throttle(...)` itself stacks within one config.
            self.throttles
                .entry(table.clone())
                .or_default()
                .extend(throttles);
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
        if bulk {
            self.bulk.insert(table.clone());
        }
        if let Some(scope) = scope {
            // Last `.resource(...)` wins for the same table (as with permission).
            self.object_scopes.insert(table.clone(), scope);
        }
        if let Some(col) = owner_field {
            // gaps3 #16: last `.resource(...)` wins (as with permission/scope).
            self.owner_fields.insert(table.clone(), col);
        }
        if let Some(parent) = under {
            // gaps3 #29 item 2. Last `.resource(...)` wins, as everywhere else here.
            self.unders.insert(table.clone(), parent);
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
    /// [`Model::TABLE`](umbral::orm::Model) const, so a typo in the
    /// table name is a compile error rather than a silent no-op.
    ///
    /// ```ignore
    /// RestPlugin::new().hide_model::<AuthUser>(["password_hash", "email"])
    /// ```
    pub fn hide_model<M: umbral::orm::Model>(mut self, fields: impl HideFields) -> Self {
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
    /// crate can reach it. Not exposed in the umbral facade.
    pub(crate) fn apply_overrides(&self, table: &str, row: &mut Map<String, Value>) {
        // Cap recursion so a self-referential FK that got `?include=`'d
        // (or a pathological hydration) can't loop forever. 5 hops is
        // comfortably past `?include=`'s own MAX_DEPTH of 3.
        self.apply_overrides_depth(table, None, row, 0);
    }

    /// Batch-list variant: the caller resolves the `ModelMeta` for
    /// `table` ONCE before the loop and passes a reference in.  Eliminates
    /// the per-row `model_meta_for_table` clone that `apply_overrides`
    /// would otherwise issue on every iteration.  For nested FK depths
    /// (depth > 0) the recursion falls back to `model_meta_for_table` as
    /// usual — those paths are rare and not on the hot N-row critical path.
    pub(crate) fn apply_overrides_with_meta(
        &self,
        table: &str,
        meta: &ModelMeta,
        row: &mut Map<String, Value>,
    ) {
        self.apply_overrides_depth(table, Some(meta), row, 0);
    }

    fn apply_overrides_depth(
        &self,
        table: &str,
        meta_hint: Option<&ModelMeta>,
        row: &mut Map<String, Value>,
        depth: usize,
    ) {
        const MAX_DEPTH: usize = 5;

        // --- Recurse into hydrated nested relations FIRST, so the
        // nested objects are scrubbed by their own table's overrides
        // before the parent's hide/transform/computed run on the
        // (now-clean) parent row. Only FK columns whose value is a JSON
        // object were `?include=`-hydrated; everything else (raw integer
        // FKs, scalar columns) is left untouched. ---
        //
        // At depth 0, the caller may supply a pre-resolved `meta_hint`
        // so the list-row loop pays only one `model_meta_for_table`
        // clone across all N rows instead of N clones.  At depth > 0
        // (nested FK tables) we fall back to the cached lookup.
        if depth < MAX_DEPTH {
            let owned: Option<ModelMeta>;
            let meta_opt: Option<&ModelMeta> = if let Some(m) = meta_hint {
                Some(m)
            } else {
                owned = umbral::migrate::model_meta_for_table(table);
                owned.as_ref()
            };
            if let Some(meta) = meta_opt {
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
                                umbral::storage::storage_opt()
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
                        // Nested FK targets are looked up fresh — `None` hint
                        // means `apply_overrides_depth` falls back to
                        // `model_meta_for_table` for that FK's table.
                        self.apply_overrides_depth(fk_target, None, nested, depth + 1);
                    }
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
        // Declared on the MODEL.
        //
        // `secret` can never be served to anyone, so it is hidden, full stop.
        //
        // `private` is hidden only when NOTHING can unlock it. With an `allow_private_if` on
        // the resource the column is *conditionally* visible, and calling it "hidden" here
        // would strip it straight back out of the response we just went to the trouble of
        // fetching for an authorized caller. Those columns are decided per-request by
        // `unlocked_private`, and described to OpenAPI by `is_conditionally_visible`.
        //
        // `_opt`, not `registered_models()`: the latter PANICS when no `App::build()` has run,
        // and this is reachable from unit tests and spec-only tooling that never boot an app.
        // No registry => no model info => fall through to the configured hide list; the name
        // denylist above has already run, so the catastrophic case is covered either way.
        if let Some(col) = umbral::migrate::registered_models_opt()
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|m| m.table == table)
            .and_then(|m| m.fields.iter().find(|c| c.name == field))
        {
            if umbral::orm::is_secret_column(col) {
                return true;
            }
            if col.private && !self.has_private_unlock(table, field) {
                return true;
            }
        }
        self.hidden.iter().any(|(t, f)| t == table && f == field)
    }

    /// Is an `allow_private_if` configured for this column at all?
    pub(crate) fn has_private_unlock(&self, table: &str, field: &str) -> bool {
        self.private_unlocks
            .iter()
            .any(|(t, f, _)| t == table && f == field)
    }

    /// Which `#[umbral(private)]` columns THIS caller may see on this table.
    ///
    /// Evaluated per request, because the caller's identity does not exist anywhere else. The
    /// result is handed to `DynQuerySet::allow_private`, so an approved column is SELECTed and
    /// a denied one never leaves the database.
    pub(crate) fn unlocked_private(&self, table: &str, identity: Option<&Identity>) -> Vec<String> {
        self.private_unlocks
            .iter()
            .filter(|(t, _, check)| t == table && check(identity))
            .map(|(_, f, _)| f.clone())
            .collect()
    }

    /// Is this field denied on a WRITE?
    ///
    /// Deliberately NOT the same question as [`is_field_hidden`], which is the READ policy.
    ///
    /// `#[umbral(private)]` is a **read** policy: "do not show this to anyone who has not
    /// earned it". It says nothing about who may SET the column, and it must not, because the
    /// two questions have different answers. A storefront takes `cost` on the create form and
    /// then never shows it back; a support tool lets an agent file an `internal_note` it cannot
    /// read again. Conflating them meant marking a column `private` silently made it
    /// unwritable, and the client got `cost: ["This field is required."]` for a field it
    /// demonstrably sent (gaps3 #75) — the API lying about the cause.
    ///
    /// The write guards are separate attributes, and they still apply here:
    ///
    /// - `#[umbral(privileged)]` — the mass-assignment guard. Enforced in the ORM
    ///   (`is_unauthorized_privileged`); this is the attribute to reach for when a column must
    ///   not be settable from an untrusted body.
    /// - `#[umbral(secret)]` / a hard-denied name (`password_hash`, …) — never readable, never
    ///   writable through the API.
    /// - `#[umbral(noform)]` / `noedit` — the ORM strips these from write bodies.
    /// - `ResourceConfig::hide` / `RestPlugin::hide` — stays symmetric (hidden in, hidden out),
    ///   because `hide` exists to stop mass assignment of things like `is_admin` (WEB-2).
    ///
    /// So: hidden-by-config and secret still block a write. `private` alone no longer does.
    pub(crate) fn is_field_write_denied(&self, table: &str, field: &str) -> bool {
        if HARD_DENIED_FIELDS.contains(&field) {
            return true;
        }
        if let Some(col) = umbral::migrate::registered_models_opt()
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|m| m.table == table)
            .and_then(|m| m.fields.iter().find(|c| c.name == field))
        {
            if umbral::orm::is_secret_column(col) {
                return true;
            }
        }
        // The explicit hide list — but NOT `private`, which is read-only policy.
        self.hidden.iter().any(|(t, f)| t == table && f == field)
    }

    /// Drop every write-denied field from an inbound write body.
    ///
    /// WEB-2: hiding a field (`ResourceConfig::hide` / `RestPlugin::hide`)
    /// removes it from responses (`apply_overrides`), but the column stayed
    /// *writable* — so `PATCH /api/x {"hidden_field": ...}` could still set
    /// it (mass assignment / privilege escalation when the hidden field is
    /// something like `is_admin`). Stripping it here makes `hide` symmetric:
    /// hidden in, hidden out. The ORM still strips `noform` and unauthorized
    /// `privileged` columns on its own; this layers the REST `hide` list on top.
    ///
    /// It filters on [`is_field_write_denied`], NOT `is_field_hidden`: a
    /// `#[umbral(private)]` column is hidden from RESPONSES but remains settable.
    /// See `is_field_write_denied` for why the two questions have different answers.
    pub(crate) fn strip_hidden_for_write(
        &self,
        table: &str,
        _identity: Option<&Identity>,
        body: &mut Map<String, Value>,
    ) {
        body.retain(|k, _| !self.is_field_write_denied(table, k));
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
/// `umbral-openapi` to decide whether to advertise filter query
/// parameters on a list endpoint's OpenAPI operation. Returns
/// `false` when `RestPlugin::routes()` hasn't run yet (the OnceLock
/// is empty) so calls from spec-only smoke tests don't panic.
/// Public read: would this REST plugin instance serve the given
/// table? Returns the same answer the internal allow/block check
/// uses for the actual list/retrieve/create handlers, so spec
/// consumers (umbral-openapi, the playground sidebar, etc.) stay
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

/// Public read: is `action` mounted for `table`? Consults the same
/// `.views(...)` scope the request-time gate uses, so spec consumers
/// (umbral-openapi) advertise exactly the operations the API serves — a
/// `views([List, Retrieve])` resource never emits `post`/`put`/`patch`/
/// `delete` in the generated spec. Custom (`@action`) endpoints are
/// always exposed (the scope filters only built-in CRUD).
///
/// Returns `true` when CONFIG isn't populated yet (spec-only smoke
/// tests, no REST plugin booted) so the default shape exposes
/// everything — same defaulting as [`is_exposed`].
pub fn action_exposed(table: &str, action: &Action) -> bool {
    CONFIG
        .get()
        .map(|cfg| cfg.view_exposed(table, action))
        .unwrap_or(true)
}

/// Public read: would this REST plugin strip `field` from `table`'s
/// response bodies? Returns the SAME answer `RestPlugin::apply_overrides`
/// uses at request time (both consult `RestPlugin::is_field_hidden`), so
/// spec consumers (umbral-openapi) advertise exactly the fields the API
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
/// Is this column visible to SOME callers but not others?
///
/// True exactly when the model marks it `#[umbral(private)]` and a resource configured an
/// `allow_private_if` for it.
///
/// This exists for OpenAPI. One path cannot describe two response shapes, and with a
/// per-request unlock the same endpoint returns `cost` to staff and omits it for everyone
/// else. Advertising it as a required field lies to the anonymous caller; omitting it lies to
/// the staff one. The truth is **optional** — the field may or may not be present — and that
/// is what the spec says, so a generated TypeScript client emits `cost?: string` and makes
/// the consumer check. Which is correct, because they do have to.
pub fn is_conditionally_visible(table: &str, field: &str) -> bool {
    CONFIG
        .get()
        .map(|cfg| cfg.has_private_unlock(table, field))
        .unwrap_or(false)
}

pub fn is_hidden(table: &str, field: &str) -> bool {
    CONFIG
        .get()
        .map(|cfg| cfg.is_field_hidden(table, field))
        .unwrap_or(false)
}

/// Is this column **write-only** — settable, but never returned to anyone?
///
/// That is a `#[umbral(private)]` column with no `allow_private_if` to unlock it. It is
/// writable (`private` is a read policy, not a write guard) and permanently unreadable through
/// the API. OpenAPI has exactly this concept — `writeOnly: true` — so the spec can describe it
/// honestly in a single schema instead of pretending the field does not exist and leaving a
/// client unable to discover a field it is allowed to send.
///
/// False for a column that is genuinely unwritable (`secret`, hard-denied, `hide`-ed) — those
/// are absent from the API in both directions and belong in no schema at all.
pub fn is_write_only(table: &str, field: &str) -> bool {
    let Some(cfg) = CONFIG.get() else {
        return false;
    };
    if cfg.is_field_write_denied(table, field) {
        return false;
    }
    umbral::migrate::registered_models_opt()
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|m| m.table == table)
        .and_then(|m| m.fields.iter().find(|c| c.name == field))
        .is_some_and(|col| col.private && !cfg.has_private_unlock(table, field))
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
/// configured Authentication chain. Used by `umbral-openapi` at
/// spec-build time. Returns an empty Vec when CONFIG isn't
/// populated (no REST plugin booted) — same defaulting story as
/// `filters_enabled_for`. Closes playground-openapi-gaps item 4.
pub fn registered_security_schemes() -> Vec<(String, serde_json::Value)> {
    CONFIG
        .get()
        .map(|cfg| cfg.authentication.security_schemes_all())
        .unwrap_or_default()
}

/// Public read: the base path the REST plugin mounts all CRUD routes under.
/// Used by `umbral-openapi` to build the documented paths that match the
/// real mounted routes — e.g. `"/v2"` when the plugin was configured with
/// `.at("/v2")`. Returns `"/api"` (the default) when CONFIG isn't populated.
pub fn registered_base_path() -> &'static str {
    CONFIG
        .get()
        .map(|cfg| cfg.base_path.as_str())
        .unwrap_or("/api")
}

/// Public read: which pagination query-parameter style the configured
/// backend reads. Used by `umbral-openapi` to emit the correct `parameters`
/// entries on list endpoints — `page`/`page_size` for [`PageNumberPagination`],
/// `limit`/`offset` for [`LimitOffsetPagination`], nothing for
/// [`NoPagination`], and nothing for unknown custom backends. Returns
/// [`PaginationStyle::None`] when CONFIG isn't populated.
pub fn registered_pagination_style() -> PaginationStyle {
    CONFIG
        .get()
        .map(|cfg| cfg.pagination.style())
        .unwrap_or(PaginationStyle::None)
}

/// The configured paginator's declared wire shape, if it is a custom
/// paginator that overrides [`Pagination::schema`]. Read by `umbral-openapi`
/// to emit a *typed* envelope + query params for a custom paginator (in the
/// OpenAPI spec and the generated TypeScript client) instead of the opaque
/// fallback. `None` for the built-in styles (their shape is known from
/// [`registered_pagination_style`]) and when CONFIG isn't populated.
pub fn registered_pagination_schema() -> Option<PaginationSchema> {
    CONFIG.get().and_then(|cfg| cfg.pagination.schema())
}

/// One custom `@action`'s OpenAPI-facing schema info — read by
/// `umbral-openapi` to emit the action's path + request/response schemas
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

/// Public read: every custom `@action` registered on any resource. Used by
/// `umbral-openapi` at spec-build time to emit each action's path + method.
/// Request/response schemas are inlined when the action declared them via
/// `ResourceConfig::action_input_schema` / `action_output_schema`; an action
/// with no declared schema still appears (just without a typed body). Empty
/// when no REST plugin has booted.
pub fn registered_action_schemas() -> Vec<ActionSchema> {
    let Some(cfg) = CONFIG.get() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (table, defs) in &cfg.actions {
        for d in defs {
            // Every registered action is surfaced — not only the ones that
            // declared a request/response schema. A plain `.action(name,
            // method, scope, handler)` (e.g. `get_price_at`) still has a
            // path + method worth listing; umbral-openapi emits it with a
            // generic 200 and inlines schemas only when present.
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

    fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
        // Publish our base path before any plugin's routes() runs.
        // models() is collected in an earlier build phase than router
        // assembly, so umbral-auth can read umbral::web::api_base() in its
        // own routes() call without a Cargo dependency on this crate.
        // See umbral::web::set_api_base (first-call-wins OnceLock).
        umbral::web::set_api_base(self.base_path());
        Vec::new() // RestPlugin owns no models; it serves app-registered ones.
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
            let tables: Vec<String> = umbral::migrate::registered_models()
                .iter()
                .map(|m| m.table.clone())
                .collect();

            let open: Vec<String> = tables
                .iter()
                .filter(|t| self.allow(t) && self.permission_for(t).is_open())
                .cloned()
                .collect();
            if !open.is_empty() {
                tracing::warn!(
                    tables = %open.join(", "),
                    "umbral-rest: {} resource(s) are exposed with NO authentication and an \
                     AllowAny permission — anonymous clients can read AND write them \
                     (POST/PUT/PATCH/DELETE). Set RestPlugin::authenticate(...) and/or a \
                     per-resource .permission(...) (ReadOnly / IsAuthenticated / IsStaff), \
                     or .exclude(...) the table if it shouldn't be served at all.",
                    open.len(),
                );
            }

            // M-4: the `open` warning above only fires for AllowAny (read
            // AND write). The DEFAULT `ReadOnly` serves anonymous *reads* of
            // every non-blocked business model with no startup signal at
            // all. Surface those too — quieter than full CRUD, but the
            // operator should still know these rows are world-readable.
            // Exclude the `open` tables already reported above.
            let read_open: Vec<String> = tables
                .iter()
                .filter(|t| self.allows_anonymous_read(t) && !self.permission_for(t).is_open())
                .cloned()
                .collect();
            if !read_open.is_empty() {
                tracing::warn!(
                    tables = %read_open.join(", "),
                    "umbral-rest: {} resource(s) serve anonymous READS with NO authentication \
                     (GET list/detail) — every row is world-readable. This is the safe-by-default \
                     ReadOnly permission; if that's not intended, set RestPlugin::authenticate(...), \
                     a per-resource .permission(IsAuthenticated / IsStaff), or .exclude(...) the \
                     table.",
                    read_open.len(),
                );
            }
        }

        // M-5: throttling is entirely opt-in. If writes are enabled on any
        // resource yet NO throttle is configured anywhere, the write / bulk /
        // nested / CSV / search endpoints carry no rate limit. We do not
        // impose a default throttle (that would be a silent behaviour change);
        // we warn so the operator can add one deliberately.
        if self.has_no_throttle() {
            let writable: Vec<String> = umbral::migrate::registered_models()
                .iter()
                .map(|m| m.table.clone())
                .filter(|t| self.permits_writes(t))
                .collect();
            if !writable.is_empty() {
                tracing::warn!(
                    tables = %writable.join(", "),
                    "umbral-rest: {} writable resource(s) have NO throttle configured — create / \
                     update / delete / bulk / nested writes are unbounded by rate. Add \
                     RestPlugin::default_throttle(...) (e.g. AnonRateThrottle / UserRateThrottle) \
                     or a per-resource .throttle(...) to cap abuse.",
                    writable.len(),
                );
            }
        }

        // audit_2 H9: an IP-keyed throttle (AnonRateThrottle) can only isolate
        // callers when the framework can trust a client IP. Under the secure
        // default `trusted_proxy_hops == 0`, `X-Forwarded-For` is client-forgeable
        // and therefore ignored, so EVERY anonymous caller collapses into a single
        // shared bucket: a "2/min" limit then caps the entire anonymous userbase
        // collectively (a self-inflicted DoS), and an attacker can't be singled
        // out anyway. IP throttling is a reverse-proxy-deployment feature — the
        // operator must declare how many trusted proxies sit in front so the real
        // client IP can be recovered from `X-Forwarded-For`. Warn loudly rather
        // than silently degrade to the shared bucket.
        if !self.has_no_throttle() {
            let hops = umbral::settings::get_opt()
                .map(|s| s.trusted_proxy_hops)
                .unwrap_or(0);
            if hops == 0 {
                tracing::warn!(
                    "umbral-rest: an IP-keyed throttle is configured but \
                     settings.trusted_proxy_hops == 0, so X-Forwarded-For is not trusted and \
                     every anonymous caller shares ONE rate-limit bucket — the limit applies to \
                     all anonymous traffic collectively, not per client. Set trusted_proxy_hops \
                     to the number of trusted reverse proxies in front of the app (e.g. 1 behind \
                     a single nginx/LB) so the real client IP can be recovered and throttled \
                     per-IP.",
                );
            }
        }

        // Compute the URL prefixes the resource route-set mounts under.
        // Without versioning (or with accept-header versioning, where the
        // version travels in a header) that's just the base path. With
        // URL-path versioning it's `{base}/{version}` for EACH allowed
        // version, so `/api/v1/...` and `/api/v2/...` both resolve. An
        // unknown version matches none of these prefixes → axum 404,
        // which is exactly "unknown version is not routable".
        let prefixes = self.mount_prefixes();

        let mut router = Router::new();
        let mut root_mounted = false;
        for base in &prefixes {
            // Collection routes carry GET (list) + POST (create) always, and
            // PATCH (bulk_update) + DELETE (bulk_delete) for the bulk path
            // (gaps2 #82). The bulk handlers self-gate on the per-table
            // `.bulk()` opt-in — a collection PATCH/DELETE to a resource that
            // didn't opt in returns 404, so a non-bulk resource is unchanged.
            router = router
                .route(
                    &format!("{base}/{{table}}/"),
                    get(list)
                        .post(create)
                        .patch(bulk_update)
                        .delete(bulk_delete)
                        .options(collection_options),
                )
                .route(
                    &format!("{base}/{{table}}"),
                    get(list)
                        .post(create)
                        .patch(bulk_update)
                        .delete(bulk_delete)
                        .options(collection_options),
                )
                .route(
                    &format!("{base}/{{table}}/{{id}}"),
                    get(retrieve)
                        .put(update)
                        .patch(update)
                        .delete(destroy)
                        .options(detail_options),
                )
                // gaps3 #29 item 2 — parent-scoped sub-resources. Generic, exactly like
                // the flat routes above: the handlers dispatch on `{table}` and 404 unless
                // that resource declared `.under(parent, fk)` naming THIS parent.
                //
                // These do not collide with the registered custom actions
                // (`/api/order/{id}/approve`, same segment count): matchit ranks static
                // segments above parameters, so a concrete action route wins, and only
                // what it does not claim reaches here.
                .route(
                    &format!("{base}/{{parent}}/{{parent_id}}/{{table}}"),
                    get(nested_list).post(nested_create),
                )
                .route(
                    &format!("{base}/{{parent}}/{{parent_id}}/{{table}}/"),
                    get(nested_list).post(nested_create),
                )
                .route(
                    &format!("{base}/{{parent}}/{{parent_id}}/{{table}}/{{id}}"),
                    get(nested_retrieve)
                        .put(nested_update)
                        .patch(nested_update)
                        .delete(nested_destroy),
                );

            // API root index: lists the exposed resources + every plugin's
            // advertised endpoints (service discovery). Skipped when REST is
            // mounted at the bare root (empty base), where `/` would collide
            // with the app's own home route. With versioning the index lives
            // at each `{base}/{version}/` prefix.
            if !base.is_empty() && !root_mounted {
                router = router
                    .route(&format!("{base}/"), get(api_root))
                    .route(base.as_str(), get(api_root));
                // Only mount `{base}` (the bare, version-less root) once;
                // the per-version `{base}/{version}/` index is mounted below.
                root_mounted = true;
            } else if !base.is_empty() {
                router = router.route(&format!("{base}/"), get(api_root));
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
        }

        // gaps3 #36: one layer, so EVERY REST response is covered — list, detail,
        // the write verbs, and the custom `@action` handlers apps mount by hand.
        // Doing it per-handler would have missed the ones added later, which is
        // exactly how a header like this rots.
        router.layer(axum::middleware::from_fn(cache_control_layer))
    }

    fn route_paths(&self) -> Vec<umbral::routes::RouteSpec> {
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
        use umbral::routes::RouteSpec;
        let base = &self.base_path;
        let mut specs: Vec<RouteSpec> = Vec::new();
        for meta in umbral::migrate::registered_models() {
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
            panic!("umbral-rest: method {other} isn't supported as an `@action` HTTP method")
        }
    }
}

// =========================================================================
// OPTIONS (gaps2 #98). A resource endpoint answers `OPTIONS` with `204 No
// Content` + an `Allow` header listing its supported verbs, instead of the
// bare `405` axum returns for an unregistered method. CORS-preflight OPTIONS
// is still handled by the cors layer when present; this is the resource-level
// method-discovery answer (the HTTP-spec-correct response). A JSON metadata
// body — fields, writable columns — is a deferred follow-up.
// =========================================================================

/// Which of the two REST URI shapes a request hit. Drives both the
/// `OPTIONS` `Allow` header and the `Allow` header on a view-scoped
/// `405`: collection PATCH/DELETE are the bulk endpoints, detail
/// PUT/PATCH/DELETE act on a single row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointKind {
    /// `/api/<table>/` — list, create, bulk update/delete.
    Collection,
    /// `/api/<table>/<id>` — retrieve, update, destroy.
    Detail,
}

/// Run `fut` with the authenticated caller in the ambient `RouteContext`, so ORM
/// writes can stamp `#[umbral(auto_user_add)]` / `#[umbral(auto_user)]` columns
/// (gaps3 #55).
///
/// Scoped around the WRITE rather than resolved in a router-wide layer, because
/// the identity is already in hand here — re-authenticating in a layer would cost
/// a second token lookup on every request, including reads that never stamp.
async fn as_user<F: std::future::Future>(identity: Option<&Identity>, fut: F) -> F::Output {
    match identity {
        Some(id) => {
            let mut ctx = (*umbral::db::route_context()).clone();
            ctx.set_user(id.user_id.clone());
            umbral::db::route_context_scope(ctx, fut).await
        }
        None => fut.await,
    }
}

/// Build a `204 No Content` response carrying an `Allow` header.
fn options_response(allow: &str) -> Response {
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if let Ok(value) = http::HeaderValue::from_str(allow) {
        resp.headers_mut().insert(http::header::ALLOW, value);
    }
    resp
}

/// `Allow` header value for `(table, kind)`: `OPTIONS` plus every verb
/// the resource actually serves. Reflects `.views(...)` scope and (for
/// the collection) the `.bulk()` opt-in. Defaults to the full verb set
/// when CONFIG isn't populated (spec-only smoke tests, no plugin booted).
fn options_allow(table: &str, kind: EndpointKind) -> String {
    let methods = match CONFIG.get() {
        Some(cfg) => cfg.exposed_methods(table, kind),
        None => match kind {
            EndpointKind::Collection => vec!["GET", "POST"],
            EndpointKind::Detail => vec!["GET", "PUT", "PATCH", "DELETE"],
        },
    };
    std::iter::once("OPTIONS")
        .chain(methods)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `OPTIONS` on a collection endpoint (`/api/{table}` and `/api/{table}/`).
/// `GET` (list) + `POST` (create) when exposed; collection `PATCH`/`DELETE`
/// (bulk) only when the resource opted in via `.bulk()`. The `Allow` list
/// honors any `.views(...)` scope so a read-only resource advertises only
/// the verbs it serves.
async fn collection_options(Path(table): Path<String>) -> Response {
    options_response(&options_allow(&table, EndpointKind::Collection))
}

/// `OPTIONS` on a detail endpoint (`/api/{table}/{id}`): retrieve / update /
/// destroy, filtered by any `.views(...)` scope on the resource.
async fn detail_options(Path((table, _id)): Path<(String, String)>) -> Response {
    options_response(&options_allow(&table, EndpointKind::Detail))
}

// =========================================================================
// Errors. Mapped to a JSON envelope so clients get a consistent shape.
// =========================================================================

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    /// Stable machine-readable error code. Always populated.
    code: &'static str,
    /// Field-level errors flattened to the top level
    /// (`{ "category": ["..."], "sku": ["..."] }`). Empty for
    /// non-validation errors.
    #[serde(flatten)]
    field_errors: BTreeMap<String, Vec<String>>,
    /// Validation errors not tied to a specific field
    /// (`non_field_errors`). Empty for non-validation errors.
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
    /// 400 — DB constraint violation reshaped into
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
    /// 406 — the request asked (via the `Accept` / configured version
    /// header) for an API version that isn't in `allowed_versions`.
    /// Accept-header versioning rejects an unknown version this
    /// way; URL-path versioning 404s instead (no matching route).
    NotAcceptable(String),
    /// 429 — the caller is over their rate. Raised when a
    /// [`Throttle`] denied the request (after auth, before the
    /// handler). Carries the retry hint that becomes a `Retry-After`
    /// header (seconds, rounded up). The body is
    /// `{"detail":"Request was throttled.","retry_after":N}`.
    Throttled {
        retry_after: Option<std::time::Duration>,
    },
    /// 405 — the URI exists but doesn't serve the requested method.
    /// Raised when a built-in CRUD action is scoped out by `.views(...)`
    /// yet the endpoint still serves at least one other verb (e.g. a
    /// `POST` to a `views([List, Retrieve])` collection). Carries the
    /// `Allow` header value listing the verbs that *are* served, per
    /// RFC 7231. (A fully unserved URI 404s instead — see `gate`.)
    MethodNotAllowed {
        allow: String,
    },
    /// 500 — a non-database internal error (e.g. CSV serialization
    /// failure). The message is logged server-side; the client sees
    /// an opaque "internal server error" response.
    Internal(String),
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        // Plain sqlx errors land here only from the non-write
        // paths (filter / count / delete). Writes go through
        // `WriteError`, which has its own translator below.
        if matches!(e, sqlx::Error::Protocol(_)) {
            // WEB-5 (L-8): a Protocol error is a driver/wire-level fault
            // whose text ("unexpected message from server", column
            // metadata, etc.) is framework/DB internals. It maps to a 400
            // (the request shape drove it) but the client gets a generic
            // message; the detail stays in the server log only.
            tracing::warn!(error = %e, "REST: sqlx protocol error mapped to a generic 400");
            return Self::BadInput("malformed request".to_string());
        }
        Self::Sqlx(e)
    }
}

impl From<umbral::orm::DynError> for ApiError {
    fn from(e: umbral::orm::DynError) -> Self {
        // gaps2 #12: `DynError` is now a real enum (was an alias
        // for `sqlx::Error`). Route each variant to the right
        // translator so the structured `WriteError` keeps its
        // per-field map all the way to the response body.
        match e {
            umbral::orm::DynError::Write(w) => Self::from(w),
            umbral::orm::DynError::Sqlx(s) => Self::from(s),
        }
    }
}

impl From<umbral::orm::write::WriteError> for ApiError {
    fn from(e: umbral::orm::write::WriteError) -> Self {
        use umbral::orm::write::WriteError;
        // True infrastructure / serialization failures (raw
        // sqlx::Error not classified as a constraint, JSON
        // serialization failure, NotAnObject) bubble out as 500
        // via the `Sqlx` path. Everything else is a 400 with the
        // structured WriteError shape rendered into the flat
        // field-error body via `field_errors()` + `non_field_errors()`.
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

impl umbral::web::IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Validation errors take the flat field-error shape; the
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

        // 429 takes a body (`detail` + `retry_after`) plus a
        // `Retry-After` header, so it's built here rather than through the
        // single-message envelope below.
        if let ApiError::Throttled { retry_after } = self {
            // Round UP to whole seconds — never tell a client to retry
            // before a slot actually frees.
            let secs = retry_after
                .map(|d| {
                    let whole = d.as_secs();
                    if d.subsec_nanos() > 0 {
                        whole + 1
                    } else {
                        whole
                    }
                })
                .unwrap_or(0);
            let body = serde_json::json!({
                "detail": "Request was throttled.",
                "retry_after": secs,
            });
            let mut resp = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
            if let Ok(val) = http::HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut().insert(http::header::RETRY_AFTER, val);
            }
            return resp;
        }

        // 405 carries an `Allow` header (the verbs this URI does serve),
        // so it's built here rather than through the single-message
        // envelope below.
        if let ApiError::MethodNotAllowed { allow } = self {
            let body = ApiErrorBody {
                code: "method_not_allowed",
                field_errors: BTreeMap::new(),
                non_field_errors: Vec::new(),
                error: "method not allowed".to_string(),
                hint: None,
                available: Vec::new(),
            };
            let mut resp = (StatusCode::METHOD_NOT_ALLOWED, Json(body)).into_response();
            if let Ok(val) = http::HeaderValue::from_str(&allow) {
                resp.headers_mut().insert(http::header::ALLOW, val);
            }
            return resp;
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
            ApiError::Json(e) => {
                // WEB-5 (L-8): serde error text can echo internal type
                // names / struct shapes back to the client. Log the parse
                // detail server-side; hand the caller a generic message.
                tracing::warn!(error = %e, "REST: request-body JSON parse error");
                (
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    "request body is not valid JSON".to_string(),
                )
            }
            ApiError::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "authentication required".to_string(),
            ),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "forbidden".to_string()),
            ApiError::NotAcceptable(m) => (StatusCode::NOT_ACCEPTABLE, "not_acceptable", m),
            ApiError::Throttled { .. } => unreachable!("handled above"),
            ApiError::MethodNotAllowed { .. } => unreachable!("handled above"),
            ApiError::Internal(m) => {
                tracing::error!(error = %m, "REST handler hit an internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_string(),
                )
            }
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
    let is_dev = umbral::settings::get_opt()
        .map(|s| matches!(s.environment, umbral::Environment::Dev))
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
        for plugin in umbral::migrate::registered_plugins() {
            for m in umbral::migrate::models_for_plugin(&plugin) {
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
             set `environment = \"prod\"` in umbral.toml to drop it."
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
async fn api_root(headers: umbral::web::HeaderMap) -> Json<Value> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let base = &cfg.base_path;

    let mut resources = Map::new();
    for meta in umbral::migrate::registered_models() {
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
    let endpoints: Vec<Value> = umbral::migrate::registered_api_endpoints()
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
fn request_origin(headers: &umbral::web::HeaderMap) -> Option<String> {
    let host = headers.get("host")?.to_str().ok()?;
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    Some(format!("{scheme}://{host}"))
}

/// 404 unless a row with this primary key exists (gaps3 #29 item 2).
///
/// `ResourceConfig::under` does this for you on a nested route. This is the same check,
/// standalone, for the custom handler that still needs it — the audit found seven
/// hand-written copies of `.filter(pk.eq(id)).exists()` + a 404.
///
/// ```ignore
/// exists_or_404::<Fixture>(&fixture_id).await?;
/// ```
///
/// An `id` that cannot even be coerced to the model's PK type is a 404, not a query.
/// That matters: a filter whose value will not coerce is DROPPED by the query builder,
/// so the naive spelling of this check asks "does any row exist at all?" and cheerfully
/// answers yes. (See gaps3 #56.)
///
/// Returns the PUBLIC `umbral::web::ApiError` — the one a hand-written handler already
/// returns and the prelude already exports — not this crate's internal viewset error.
pub async fn exists_or_404<M: umbral::orm::Model>(id: &str) -> Result<(), umbral::web::ApiError> {
    use umbral::web::ApiError as PublicApiError;

    let meta = umbral::migrate::ModelMeta::for_::<M>();
    let pk = meta
        .pk_column()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "id".to_string());
    let not_found = || PublicApiError::not_found(format!("no {} with id `{id}`", M::NAME));

    let Some(cond) = umbral::orm::typed_eq_condition(&meta, &pk, id) else {
        return Err(not_found());
    };
    let found = umbral::orm::DynQuerySet::for_meta(&meta)
        .filter_condition(cond)
        .exists()
        .await
        .map_err(|e| PublicApiError::internal(e.to_string()))?;
    if found { Ok(()) } else { Err(not_found()) }
}

/// The parent segment of a nested URL: `/api/{parent.table}/{parent.id}/{child}`.
#[derive(Clone, Debug)]
struct ParentRef {
    table: String,
    id: String,
}

/// Resolve a request's parent scoping (gaps3 #29 item 2), or 404.
///
/// Returns `Some((fk_column, parent_id))` when the resource is nested — the caller ANDs
/// that into its query and injects it on create.
///
/// Every arm below that returns 404 is a URL that does not name a real thing, and the
/// distinction it protects is the one hand-written nested handlers get wrong: a child
/// collection under a parent that does not exist is a **wrong URL**, not an empty
/// result. Answering `200 []` tells the client it asked a valid question about a real
/// parent — so a typo'd id, a deleted fixture and a genuinely childless one all look
/// identical, and the bug hides in the one case you cannot see.
async fn resolve_parent(
    cfg: &RestPlugin,
    table: &str,
    parent: Option<&ParentRef>,
) -> Result<Option<(String, String)>, ApiError> {
    match (cfg.unders.get(table), parent) {
        (None, None) => Ok(None),

        // A nested URL for a resource that never declared a parent.
        (None, Some(p)) => Err(ApiError::NotFound(format!(
            "no resource at /api/{}/{}/{table}",
            p.table, p.id
        ))),

        // Declared nested, reached FLAT. The flat route must not work: a resource that
        // is reachable both nested and flat is not scoped, it merely has a
        // scoped-looking URL — which is worse than no scoping, because you would trust
        // it. This is the arm that makes `under()` a guarantee rather than a decoration.
        (Some((parent_table, _)), None) => Err(ApiError::NotFound(format!(
            "`{table}` is nested under `{parent_table}` — use \
             /api/{parent_table}/{{{parent_table}_id}}/{table}"
        ))),

        // Nested under the WRONG parent.
        (Some((parent_table, _)), Some(p)) if &p.table != parent_table => {
            Err(ApiError::NotFound(format!(
                "`{table}` is nested under `{parent_table}`, not `{}`",
                p.table
            )))
        }

        (Some((parent_table, fk_column)), Some(p)) => {
            // The parent row must EXIST. `exists()` stops at the first hit rather than
            // counting every match — this runs on every request to a nested endpoint.
            let parent_meta = model_meta(parent_table).ok_or_else(|| {
                ApiError::NotFound(format!("no model `{parent_table}` to nest `{table}` under"))
            })?;
            let parent_pk = parent_meta
                .pk_column()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "id".to_string());

            // Coerce the URL segment to the parent PK's real type FIRST, and 404 when it
            // cannot be. This is not a nicety. `filter_eq_string` DROPS a predicate whose
            // value will not coerce, so `/api/fixture/not-a-number/selection` would ask
            // the unfiltered question "does any fixture exist at all?" — get back `true`
            // — and sail on. An id that cannot name a row must not degrade into a query
            // that matches every row.
            let Some(pk_cond) = umbral::orm::typed_eq_condition(&parent_meta, &parent_pk, &p.id)
            else {
                return Err(ApiError::NotFound(format!(
                    "no `{parent_table}` with id `{}`",
                    p.id
                )));
            };
            let found = umbral::orm::DynQuerySet::for_meta(&parent_meta)
                .filter_condition(pk_cond)
                .exists()
                .await
                .map_err(ApiError::from)?;
            if !found {
                return Err(ApiError::NotFound(format!(
                    "no `{parent_table}` with id `{}`",
                    p.id
                )));
            }
            Ok(Some((fk_column.clone(), p.id.clone())))
        }
    }
}

/// The registered `ModelMeta` for a table, WITHOUT the REST exposure check.
///
/// `allowed_model` refuses a table that is not an exposed REST resource — right for a
/// resource being served, wrong for a parent being nested under, which is a perfectly
/// ordinary thing to want without also publishing a flat CRUD endpoint for it.
fn model_meta(table: &str) -> Option<ModelMeta> {
    for plugin in umbral::migrate::registered_plugins() {
        for m in umbral::migrate::models_for_plugin(&plugin) {
            if m.table == table {
                return Some(m);
            }
        }
    }
    None
}

fn allowed_model(table: &str) -> Result<ModelMeta, ApiError> {
    let config = CONFIG.get().expect("RestPlugin::routes was called");
    if !config.allow(table) {
        return Err(ApiError::NotFound(format!("no resource at /api/{table}")));
    }
    for plugin in umbral::migrate::registered_plugins() {
        for m in umbral::migrate::models_for_plugin(&plugin) {
            if m.table == table {
                return Ok(m);
            }
        }
    }
    Err(ApiError::NotFound(format!("no resource at /api/{table}")))
}

fn pk_column(model: &ModelMeta) -> Result<&umbral::migrate::Column, ApiError> {
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
    uri: axum::http::Uri,
    Query(params): Query<HashMap<String, String>>,
    headers: umbral::web::HeaderMap,
) -> Result<Response, ApiError> {
    list_impl(table, None, uri, params, headers).await
}

/// `GET /api/{parent}/{parent_id}/{table}` (gaps3 #29 item 2).
async fn nested_list(
    Path((parent_table, parent_id, table)): Path<(String, String, String)>,
    uri: axum::http::Uri,
    Query(params): Query<HashMap<String, String>>,
    headers: umbral::web::HeaderMap,
) -> Result<Response, ApiError> {
    let parent = ParentRef {
        table: parent_table,
        id: parent_id,
    };
    list_impl(table, Some(parent), uri, params, headers).await
}

async fn list_impl(
    table: String,
    parent: Option<ParentRef>,
    uri: axum::http::Uri,
    params: HashMap<String, String>,
    headers: umbral::web::HeaderMap,
) -> Result<Response, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    // Resolve + validate the API version (opt-in; `None` when off). For
    // accept-header versioning an unsupported version is a 406 here.
    let _ctx = RequestContext {
        table: table.clone(),
        identity: identity.clone(),
        version: cfg.resolve_version(uri.path(), &headers)?,
    };
    let model = allowed_model(&table)?;
    cfg.gate(
        &table,
        &Action::List,
        EndpointKind::Collection,
        identity.as_ref(),
    )?;
    cfg.gate_throttle(
        &table,
        &Action::List,
        identity.as_ref(),
        throttle_client_ip(&headers).as_deref(),
    )?;

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

    // audit_2 H1/P2: AND the object-level scope into the list filter so only
    // in-scope rows are returned. `DenyAll` becomes an always-false predicate,
    // so the normal pagination/response path yields an empty page (no special
    // early-return, no oracle).
    // gaps3 #29 item 2. Deliberately AFTER the permission gate: whether a parent row
    // exists is information, and a caller who may not read this resource must not
    // learn it from the difference between a 403 and a 404.
    let parent_scope = resolve_parent(cfg, &table, parent.as_ref()).await?;
    match cfg
        .object_scope(&table, identity.as_ref(), parent_scope.as_ref())
        .await
    {
        ObjectScopeOutcome::Unconstrained => {}
        ObjectScopeOutcome::Filter(cond) => filter = filter.and(cond),
        ObjectScopeOutcome::DenyAll => {
            filter = filter.and(sea_query::Condition::all().add(sea_query::Expr::val(1).eq(0)));
        }
    }

    // `?ordering=-created_at,name` — comma-separated
    // field names, leading `-` for DESC. Unknown fields are silently
    // dropped (same as DynQuerySet::order_by_col does internally).
    let ordering: Vec<(String, bool)> = params
        .get("ordering")
        .map(|s| parse_ordering(s, &model.fields))
        .unwrap_or_default();

    // `?include=fk1,fk2` — expand the named FK columns into their
    // full related-row objects via one batched IN(...) per FK. The
    // parser rejects unknown / non-FK names with a 400 so clients
    // get loud feedback on typos instead of a silently-unexpanded
    // response that looks fine until they check it.
    let include = parse_include(params.get("include").map(|s| s.as_str()), &model)?;
    let fields_param = params.get("fields").map(|s| s.as_str());

    // Resolve the ModelMeta once for the whole response — shared by both
    // the CSV and JSON paths below.  `apply_overrides_with_meta` accepts a
    // `&ModelMeta` reference so the per-row loop pays only one clone for
    // the entire list instead of one clone per row (gaps2 #72).
    let list_meta = umbral::migrate::model_meta_for_table(&table);

    // `?format=csv` — export the filtered set with the same hard ceiling
    // as JSON list responses. This endpoint buffers rows before writing
    // CSV, so it must never bypass MAX_LIST_ROWS.
    if params.get("format").map(String::as_str) == Some("csv") {
        let csv_page = PageRequest {
            limit: MAX_LIST_ROWS,
            offset: 0,
            page: None,
        };
        let mut rows = fetch_rows(
            &model,
            None,
            Some(csv_page),
            &filter,
            &include,
            &ordering,
            &cfg.unlocked_private(&model.table, identity.as_ref()),
        )
        .await?;
        for row in &mut rows {
            if let Some(ref meta) = list_meta {
                cfg.apply_overrides_with_meta(&table, meta, row);
            } else {
                cfg.apply_overrides(&table, row);
            }
            RestPlugin::apply_sparse_fields(row, fields_param);
        }
        return csv_response(&table, &model, &rows);
    }

    let page_req = cfg.pagination.extract_request(&params);
    let mut rows = fetch_rows(
        &model,
        None,
        Some(page_req),
        &filter,
        &include,
        &ordering,
        &cfg.unlocked_private(&model.table, identity.as_ref()),
    )
    .await?;
    for row in &mut rows {
        if let Some(ref meta) = list_meta {
            cfg.apply_overrides_with_meta(&table, meta, row);
        } else {
            cfg.apply_overrides(&table, row);
        }
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
///
/// Returns `Err(ApiError::Internal(...))` if the CSV writer or UTF-8
/// conversion fails, so the caller can return a 500 instead of a
/// silently-truncated or empty 200.
fn csv_response(
    table: &str,
    model: &ModelMeta,
    rows: &[Map<String, Value>],
) -> Result<Response, ApiError> {
    let csv = rows_to_csv(model, rows).map_err(ApiError::Internal)?;
    Ok((
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
        .into_response())
}

/// Serialize rows to CSV. Columns follow the model's field order (only
/// those present after hide / sparse-field filtering), with any extra keys
/// (computed fields) appended in first-seen order. Object / array cells
/// render as compact JSON. The `csv` writer handles quoting + escaping.
///
/// Returns `Err(msg)` if the underlying writer or UTF-8 conversion fails,
/// so callers can surface a 500 instead of returning a silently-truncated
/// or empty 200.
fn rows_to_csv(model: &ModelMeta, rows: &[Map<String, Value>]) -> Result<String, String> {
    let bytes = rows_to_csv_into(Vec::new(), model, rows)?;
    String::from_utf8(bytes).map_err(|e| format!("csv utf-8 conversion failed: {e}"))
}

/// Inner helper: writes CSV into any `std::io::Write`. Separated so
/// tests can inject a failing writer to exercise the error path without
/// spinning up the full HTTP stack.
fn rows_to_csv_into<W: std::io::Write>(
    sink: W,
    model: &ModelMeta,
    rows: &[Map<String, Value>],
) -> Result<W, String> {
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
    let mut wtr = csv::Writer::from_writer(sink);
    wtr.write_record(&cols)
        .map_err(|e| format!("csv header write failed: {e}"))?;
    for r in rows {
        let record: Vec<String> = cols.iter().map(|c| csv_cell(r.get(c))).collect();
        wtr.write_record(&record)
            .map_err(|e| format!("csv row write failed: {e}"))?;
    }
    wtr.into_inner()
        .map_err(|e| format!("csv flush failed: {e}"))
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
    uri: axum::http::Uri,
    Query(params): Query<HashMap<String, String>>,
    headers: umbral::web::HeaderMap,
) -> Result<Json<Map<String, Value>>, ApiError> {
    retrieve_impl(table, id, None, uri, params, headers).await
}

/// `GET /api/{parent}/{parent_id}/{table}/{id}` (gaps3 #29 item 2).
async fn nested_retrieve(
    Path((parent_table, parent_id, table, id)): Path<(String, String, String, String)>,
    uri: axum::http::Uri,
    Query(params): Query<HashMap<String, String>>,
    headers: umbral::web::HeaderMap,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let parent = ParentRef {
        table: parent_table,
        id: parent_id,
    };
    retrieve_impl(table, id, Some(parent), uri, params, headers).await
}

async fn retrieve_impl(
    table: String,
    id: String,
    parent: Option<ParentRef>,
    uri: axum::http::Uri,
    params: HashMap<String, String>,
    headers: umbral::web::HeaderMap,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let _ctx = RequestContext {
        table: table.clone(),
        identity: identity.clone(),
        version: cfg.resolve_version(uri.path(), &headers)?,
    };
    let model = allowed_model(&table)?;
    cfg.gate(
        &table,
        &Action::Retrieve,
        EndpointKind::Detail,
        identity.as_ref(),
    )?;
    cfg.gate_throttle(
        &table,
        &Action::Retrieve,
        identity.as_ref(),
        throttle_client_ip(&headers).as_deref(),
    )?;
    let pk = pk_column(&model)?;
    // audit_2 H1/P2: object-level scope. A DenyAll (e.g. anonymous on an
    // owner-scoped resource) is a 404 — never reveal the row exists; a Filter
    // is ANDed into the by-id lookup so a non-owned row is Not Found.
    // gaps3 #29 item 2. Deliberately AFTER the permission gate: whether a parent row
    // exists is information, and a caller who may not read this resource must not
    // learn it from the difference between a 403 and a 404.
    let parent_scope = resolve_parent(cfg, &table, parent.as_ref()).await?;
    let scope_filter = match cfg
        .object_scope(&table, identity.as_ref(), parent_scope.as_ref())
        .await
    {
        ObjectScopeOutcome::DenyAll => {
            return Err(ApiError::NotFound(format!(
                "no row with {} = {} in {}",
                pk.name, id, table
            )));
        }
        ObjectScopeOutcome::Filter(cond) => FilterClause::default().and(cond),
        ObjectScopeOutcome::Unconstrained => FilterClause::default(),
    };
    // `?include=` works the same on the retrieve path — `GET
    // /api/customer/123/?include=user` returns the customer with
    // its `user` FK expanded to the full AuthUser object. Same
    // parser, same 400-on-bad-name semantics.
    let include = parse_include(params.get("include").map(|s| s.as_str()), &model)?;
    let mut rows = fetch_rows(
        &model,
        Some((&pk.name, &id)),
        None,
        &scope_filter,
        &include,
        &[],
        &cfg.unlocked_private(&model.table, identity.as_ref()),
    )
    .await?;
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

/// gaps3 #16: if `table` declared `.owner_field(col)`, set `col` in `body` from
/// the authenticated identity's user id and reject a body-supplied value. A
/// no-op for resources without an owner field. Anonymous callers are a 401
/// (there's no identity to inject).
///
/// The value is written as an integer when `user_id` parses as one (an `i64`
/// FK) and as a string otherwise (a `String`/UUID key), so the ORM's insert
/// type-coercion accepts it either way.
/// Write the parent id from the URL into the child's FK column (gaps3 #29 item 2).
///
/// A body-supplied value is REJECTED rather than silently overwritten. Silently winning
/// would be defensible, but a client that sent `{"fixture_id": 9}` to
/// `/api/fixture/3/selection` believes something false about what it just created, and
/// the 201 it gets back would confirm it. Better to say no.
fn inject_parent(
    model: &ModelMeta,
    parent_scope: Option<&(String, String)>,
    body: &mut serde_json::Map<String, Value>,
) -> Result<(), ApiError> {
    let table = &model.table;
    let Some((fk_column, parent_id)) = parent_scope else {
        return Ok(());
    };
    if body.contains_key(fk_column) {
        return Err(ApiError::BadInput(format!(
            "`{fk_column}` is taken from the URL and must not be supplied in the request \
             body when creating a `{table}` under its parent"
        )));
    }

    // Ask the COLUMN what type it is. This used to guess from the shape of the value —
    // `parse::<i64>()`, else a string — which is wrong for a `String` primary key whose
    // value happens to be numeric (an external reference, a Stripe id): it would be
    // written as a JSON number, silently, for the one row shape nobody tests.
    let Some(col) = model.fields.iter().find(|c| &c.name == fk_column) else {
        return Err(ApiError::Internal(format!(
            "`{table}` declares `.under(..., \"{fk_column}\")` but has no such column"
        )));
    };
    let Some(value) = umbral::orm::typed_json_value(col, parent_id) else {
        // The parent id cannot be that column's type at all. `resolve_parent` already
        // 404s this, so reaching here means the two disagree — say so rather than write
        // a value of the wrong type.
        return Err(ApiError::NotFound(format!(
            "`{parent_id}` is not a valid `{table}.{fk_column}`"
        )));
    };
    body.insert(fk_column.clone(), value);
    Ok(())
}

fn inject_owner_field(
    cfg: &RestPlugin,
    model: &ModelMeta,
    identity: Option<&Identity>,
    body: &mut serde_json::Map<String, Value>,
) -> Result<(), ApiError> {
    let table = &model.table;
    let Some(owner_col) = cfg.owner_fields.get(table) else {
        return Ok(());
    };
    let Some(id) = identity else {
        return Err(ApiError::Unauthenticated);
    };
    if body.contains_key(owner_col) {
        return Err(ApiError::BadInput(format!(
            "`{owner_col}` is set from your identity and must not be supplied in the request body"
        )));
    }

    // Ask the COLUMN, not the value (gaps3 #59). The old shape —
    // `id.user_id.parse::<i64>()`, else a string — guessed the owner column's type from
    // whether the id LOOKED like a number. A UUID user model fell to the string arm and
    // worked by luck; a `String`-keyed user model whose ids happen to be numeric got its
    // owner column written as a JSON number.
    let Some(col) = model.fields.iter().find(|c| &c.name == owner_col) else {
        return Err(ApiError::Internal(format!(
            "`{table}` declares `.owner_field(\"{owner_col}\")` but has no such column"
        )));
    };
    let Some(value) = umbral::orm::typed_json_value(col, &id.user_id) else {
        // The authenticated key cannot be this column's type. That is a wiring error
        // (an i64 owner column on a UUID user model), not a client mistake.
        return Err(ApiError::Internal(format!(
            "identity key `{}` is not a valid `{table}.{owner_col}` — the owner column's \
             type and the active user model's primary key disagree",
            id.user_id
        )));
    };
    body.insert(owner_col.clone(), value);
    Ok(())
}

async fn create(
    Path(table): Path<String>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
    Json(raw): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    create_impl(table, None, uri, headers, raw).await
}

/// `POST /api/{parent}/{parent_id}/{table}` (gaps3 #29 item 2).
async fn nested_create(
    Path((parent_table, parent_id, table)): Path<(String, String, String)>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
    Json(raw): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let parent = ParentRef {
        table: parent_table,
        id: parent_id,
    };
    create_impl(table, Some(parent), uri, headers, raw).await
}

async fn create_impl(
    table: String,
    parent: Option<ParentRef>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
    raw: Value,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let _ctx = RequestContext {
        table: table.clone(),
        identity: identity.clone(),
        version: cfg.resolve_version(uri.path(), &headers)?,
    };
    let model = allowed_model(&table)?;
    cfg.gate(
        &table,
        &Action::Create,
        EndpointKind::Collection,
        identity.as_ref(),
    )?;
    cfg.gate_throttle(
        &table,
        &Action::Create,
        identity.as_ref(),
        throttle_client_ip(&headers).as_deref(),
    )?;

    // gaps3 #29 item 2. Resolved BEFORE the bulk dispatch below, because bulk must not
    // become the way around parent scoping — the same reason owner-field injection runs
    // per item down there. A hole that only exists in the bulk path is still a hole.
    let parent_scope = resolve_parent(cfg, &table, parent.as_ref()).await?;

    // gaps2 #82: when this resource opted into bulk, a JSON ARRAY body is a
    // bulk create — every item in ONE transaction. A JSON object falls
    // through to the unchanged single-create path. Without `.bulk()` an
    // array is rejected exactly as before (it isn't a single-object body),
    // so behaviour is byte-for-byte unchanged for non-bulk resources.
    if let Value::Array(items) = raw {
        if !cfg.bulk.contains(&table) {
            return Err(ApiError::BadInput(
                "request body must be a JSON object (bulk create is not enabled for this \
                 resource — call ResourceConfig::bulk() to opt in)"
                    .into(),
            ));
        }
        return bulk_create(
            cfg,
            &table,
            model,
            items,
            identity.as_ref(),
            parent_scope.as_ref(),
        )
        .await;
    }

    let Value::Object(mut body) = raw else {
        return Err(ApiError::BadInput(
            "request body must be a JSON object".into(),
        ));
    };

    let nested_specs = cfg.nested.get(&table).cloned().unwrap_or_default();

    // WEB-2: a hidden field must not be writable (see strip_hidden_for_write).
    cfg.strip_hidden_for_write(&table, identity.as_ref(), &mut body);

    // gaps3 #16: owner-field injection — fill the declared owner column from the
    // authenticated identity and reject a body-supplied value (a client can't
    // create a row owned by someone else). No-op unless `.owner_field(col)` was
    // declared. Runs before the nested split so both flat and nested creates
    // inject the parent's owner.
    inject_owner_field(cfg, &model, identity.as_ref(), &mut body)?;

    // gaps3 #29 item 2: the parent id comes from the URL, and OVERRIDES whatever the
    // body claimed. The URL is the authority here — a body that disagrees with it is at
    // best confused and at worst an attempt to plant a row under someone else's parent.
    inject_parent(&model, parent_scope.as_ref(), &mut body)?;

    // Flat path (the common case) — unchanged, zero overhead. The ORM owns
    // pre-validation + constraint classification + noform-stripping;
    // `insert_json` returns a structured `WriteError` that
    // `From<WriteError> for ApiError` translates into a 400 with
    // field-level errors.
    if nested_specs.is_empty() {
        // No nesting declared for this table — an array-of-objects in the body
        // is an undeclared nested relation, not an ignorable extra (gaps3 #10).
        reject_undeclared_nested(&model, &body)?;
        let mut row = as_user(
            identity.as_ref(),
            umbral::orm::DynQuerySet::for_meta(&model).insert_json(&body),
        )
        .await?;
        cfg.apply_overrides(&table, &mut row);
        return Ok((StatusCode::CREATED, Json(Value::Object(row))));
    }

    let (status, Json(row)) = as_user(
        identity.as_ref(),
        create_nested(cfg, model, &mut body, identity.as_ref()),
    )
    .await?;
    Ok((status, Json(Value::Object(row))))
}

/// Largest batch a single bulk request may carry. Mirrors the list
/// ceiling ([`MAX_LIST_ROWS`]) so a bulk write can never be an unbounded
/// number of statements — an over-cap batch is a `400` before any DB work.
const MAX_BULK_ITEMS: usize = MAX_LIST_ROWS as usize;

/// Reject an over-cap batch (gaps2 #82 safety): a bulk request must never
/// translate into an unbounded number of statements.
fn check_bulk_size(len: usize) -> Result<(), ApiError> {
    if len > MAX_BULK_ITEMS {
        return Err(ApiError::BadInput(format!(
            "bulk batch of {len} exceeds the maximum of {MAX_BULK_ITEMS}"
        )));
    }
    Ok(())
}

/// Bulk create (gaps2 #82): insert EVERY item in `items` on ONE
/// transaction, returning `201` and the array of created rows. Each item
/// runs the SAME field denylist and validation as the single create (the
/// permission, throttle, and blocked-table checks already ran in
/// `create`). Any item failing rolls the whole transaction back, and the
/// error names the offending index.
async fn bulk_create(
    cfg: &RestPlugin,
    table: &str,
    model: ModelMeta,
    items: Vec<Value>,
    identity: Option<&Identity>,
    parent_scope: Option<&(String, String)>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    check_bulk_size(items.len())?;

    let mut tx = umbral::db::begin().await?;
    let mut created: Vec<Value> = Vec::with_capacity(items.len());
    for (idx, item) in items.into_iter().enumerate() {
        let Value::Object(mut body) = item else {
            return Err(ApiError::BadInput(format!(
                "bulk create item {idx} must be a JSON object"
            )));
        };
        // SAME hidden-field denylist (incl. password_hash) as single create.
        cfg.strip_hidden_for_write(table, identity, &mut body);
        // gaps3 #16: owner-field injection per item — same rule as single create,
        // so bulk can't be used to forge ownership on any row.
        inject_owner_field(cfg, &model, identity, &mut body)?;
        // gaps3 #29 item 2: per item, same rule as single create — bulk can't be used
        // to plant a child under a different parent than the URL names.
        inject_parent(&model, parent_scope, &mut body)?;
        let mut row = as_user(
            identity,
            umbral::orm::DynQuerySet::for_meta(&model).insert_json_in_tx(&body, &mut tx),
        )
        .await
        .map_err(|e| bulk_item_error(idx, "create", e.into()))?;
        cfg.apply_overrides(table, &mut row);
        created.push(Value::Object(row));
    }
    // Commit only after every item succeeded — all-or-nothing.
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(Value::Array(created))))
}

/// Collection-level bulk update (gaps2 #82): `PATCH {prefix}/<table>/`
/// with a JSON array where each item carries its primary key. Partial-
/// updates every item on ONE transaction → `200` + the updated rows.
/// Only mounted when the resource opted into `.bulk()`. Mirrors the
/// permission / throttle / denylist of the single-object PATCH.
async fn bulk_update(
    Path(table): Path<String>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
    Json(raw): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let _ctx = RequestContext {
        table: table.clone(),
        identity: identity.clone(),
        version: cfg.resolve_version(uri.path(), &headers)?,
    };
    // Blocked-table + bulk opt-in gate. `allowed_model` enforces the
    // DEFAULT_BLOCKED_TABLES set, so auth_user / session get no bulk
    // surface either. The route only mounts when `.bulk()` was set, but we
    // re-check defensively so a stray request can never bypass it.
    let model = allowed_model(&table)?;
    if !cfg.bulk.contains(&table) {
        return Err(ApiError::NotFound(format!(
            "bulk update is not enabled on /api/{table}"
        )));
    }
    cfg.gate(
        &table,
        &Action::Update,
        EndpointKind::Collection,
        identity.as_ref(),
    )?;
    cfg.gate_throttle(
        &table,
        &Action::Update,
        identity.as_ref(),
        throttle_client_ip(&headers).as_deref(),
    )?;

    let Value::Array(items) = raw else {
        return Err(ApiError::BadInput(
            "bulk update body must be a JSON array of objects, each carrying its primary key"
                .into(),
        ));
    };
    check_bulk_size(items.len())?;
    let pk = pk_column(&model)?.name.clone();

    let mut tx = umbral::db::begin().await?;
    let mut updated: Vec<Value> = Vec::with_capacity(items.len());
    for (idx, item) in items.into_iter().enumerate() {
        let Value::Object(mut body) = item else {
            return Err(ApiError::BadInput(format!(
                "bulk update item {idx} must be a JSON object"
            )));
        };
        // Pull the PK out of the item; never let the body rewrite it.
        let pk_value = body.remove(&pk).ok_or_else(|| {
            ApiError::BadInput(format!(
                "bulk update item {idx} is missing its primary key `{pk}`"
            ))
        })?;
        let pk_str = json_pk_to_string(&pk_value).ok_or_else(|| {
            ApiError::BadInput(format!("bulk update item {idx} has an invalid `{pk}`"))
        })?;
        // SAME hidden-field denylist as single update.
        cfg.strip_hidden_for_write(&table, identity.as_ref(), &mut body);

        let affected = as_user(
            identity.as_ref(),
            umbral::orm::DynQuerySet::for_meta(&model)
                .filter_eq_string(&pk, &pk_str)
                .update_json_in_tx(&body, &mut tx),
        )
        .await
        .map_err(|e| bulk_item_error(idx, "update", e.into()))?;
        if affected == 0 {
            // A PK that matched no row → roll the whole batch back.
            return Err(ApiError::NotFound(format!(
                "bulk update item {idx}: no row with {pk} = {pk_str} in {table}"
            )));
        }
        // Read the row back ON THE TX so the response reflects the
        // uncommitted update.
        let mut row = fetch_one_in_tx(&model, &pk, &pk_str, &mut tx).await?;
        cfg.apply_overrides(&table, &mut row);
        updated.push(Value::Object(row));
    }
    tx.commit().await?;
    Ok(Json(Value::Array(updated)))
}

/// Collection-level bulk delete (gaps2 #82): `DELETE {prefix}/<table>/`
/// with `{ "ids": [ ... ] }`. Deletes (or soft-deletes, for a
/// soft-delete model — consistent with #35) every matching row on ONE
/// transaction → `204`. Only mounted when `.bulk()` was set.
async fn bulk_delete(
    Path(table): Path<String>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
    body: Option<Json<Value>>,
) -> Result<StatusCode, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let _ctx = RequestContext {
        table: table.clone(),
        identity: identity.clone(),
        version: cfg.resolve_version(uri.path(), &headers)?,
    };
    let model = allowed_model(&table)?;
    if !cfg.bulk.contains(&table) {
        return Err(ApiError::NotFound(format!(
            "bulk delete is not enabled on /api/{table}"
        )));
    }
    cfg.gate(
        &table,
        &Action::Delete,
        EndpointKind::Collection,
        identity.as_ref(),
    )?;
    cfg.gate_throttle(
        &table,
        &Action::Delete,
        identity.as_ref(),
        throttle_client_ip(&headers).as_deref(),
    )?;

    let ids = match body {
        Some(Json(Value::Object(mut m))) => match m.remove("ids") {
            Some(Value::Array(a)) => a,
            _ => {
                return Err(ApiError::BadInput(
                    "bulk delete body must be `{ \"ids\": [ ... ] }`".into(),
                ));
            }
        },
        _ => {
            return Err(ApiError::BadInput(
                "bulk delete body must be `{ \"ids\": [ ... ] }`".into(),
            ));
        }
    };
    check_bulk_size(ids.len())?;
    let pk = pk_column(&model)?.name.clone();

    let mut tx = umbral::db::begin().await?;
    for (idx, id) in ids.into_iter().enumerate() {
        let pk_str = json_pk_to_string(&id).ok_or_else(|| {
            ApiError::BadInput(format!("bulk delete id at index {idx} is invalid"))
        })?;
        umbral::orm::DynQuerySet::for_meta(&model)
            .filter_eq_string(&pk, &pk_str)
            .delete_in_tx(&mut tx)
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Reshape a per-item write failure so the error names which index in the
/// batch tripped. Keeps the structured field errors from the underlying
/// `ApiError` for `Validation`, prefixes the message otherwise.
fn bulk_item_error(idx: usize, op: &str, err: ApiError) -> ApiError {
    match err {
        ApiError::BadInput(m) => ApiError::BadInput(format!("bulk {op} item {idx}: {m}")),
        ApiError::Validation {
            code,
            mut field_errors,
            mut non_field_errors,
        } => {
            non_field_errors.insert(0, format!("bulk {op} item {idx} failed; batch rolled back"));
            // Leave field_errors intact so the client still sees which field
            // tripped; the non_field_errors prefix carries the index.
            let _ = &mut field_errors;
            ApiError::Validation {
                code,
                field_errors,
                non_field_errors,
            }
        }
        other => other,
    }
}

/// Render a JSON PK value (number or string) into the string form
/// `filter_eq_string` expects. Returns `None` for shapes that can't be a
/// primary key (object / array / bool / null).
fn json_pk_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Read one row back on the open transaction so a bulk update's response
/// reflects its own uncommitted writes. Uses the typed select path on the
/// tx via `DynQuerySet::fetch_one_in_tx`.
async fn fetch_one_in_tx(
    model: &ModelMeta,
    pk: &str,
    pk_str: &str,
    tx: &mut umbral::db::Transaction,
) -> Result<Map<String, Value>, ApiError> {
    let row = umbral::orm::DynQuerySet::for_meta(model)
        .filter_eq_string(pk, pk_str)
        .fetch_one_json_in_tx(tx)
        .await?;
    row.ok_or_else(|| ApiError::BadInput("row updated but disappeared on read-back".into()))
}

/// Max writable-nesting depth. A cyclic `.nested()` declaration (A→B→A) or a
/// self-referential one would otherwise recurse without bound; hitting this
/// returns a 400 rather than blowing the stack.
const MAX_NEST_DEPTH: usize = 16;

/// Total child rows a single nested write may create/update across the whole
/// tree. Mirrors the bulk ceiling ([`MAX_BULK_ITEMS`]) so a nested payload can
/// never expand to an unbounded number of statements on one transaction
/// (audit_2 plugin-rest H3). Depth is bounded separately by [`MAX_NEST_DEPTH`].
const MAX_NEST_NODES: usize = MAX_BULK_ITEMS;

/// Cross-cutting state threaded through the nested-write recursion: the plugin
/// config, the request identity (so each child row is checked against its own
/// resource's permission class), and a running count of child rows written
/// (bounded by [`MAX_NEST_NODES`]).
struct NestCtx<'a> {
    cfg: &'a RestPlugin,
    identity: Option<&'a Identity>,
    nodes: usize,
}

impl NestCtx<'_> {
    /// Count one child row about to be written; reject past the cap (H3).
    fn charge_node(&mut self) -> Result<(), ApiError> {
        self.nodes += 1;
        if self.nodes > MAX_NEST_NODES {
            return Err(ApiError::BadInput(format!(
                "nested write exceeds the maximum of {MAX_NEST_NODES} child rows"
            )));
        }
        Ok(())
    }

    /// Enforce a nested child's OWN resource permission for `action` (H2).
    /// Mirrors [`RestPlugin::gate`]'s permission-error translation, but without
    /// the exposure check — a nested child need not be a routed resource.
    fn check_child_perm(&self, table: &str, action: &Action) -> Result<(), ApiError> {
        match self.cfg.permission_for(table).check(action, self.identity) {
            Ok(()) => Ok(()),
            Err(PermissionError::Unauthenticated) => Err(ApiError::Unauthenticated),
            Err(PermissionError::Forbidden) => Err(ApiError::Forbidden),
        }
    }
}

/// Writable nested create (feature #58, recursive since gaps3 #10): insert the
/// parent and every declared nested subtree — to arbitrary depth — returning
/// the full nested object.
///
/// **Atomicity (orm_fixes #2):** the whole tree runs on ONE
/// `umbral::db::Transaction` via `DynQuerySet::insert_json_in_tx`. Every row
/// inserts on the same open tx and only becomes durable when `tx.commit()`
/// succeeds at the end. Any failure — including a process crash mid-write —
/// leaves zero rows because the transaction is never committed; sqlx rolls it
/// back on drop.
async fn create_nested(
    cfg: &RestPlugin,
    model: ModelMeta,
    body: &mut Map<String, Value>,
    identity: Option<&Identity>,
) -> Result<(StatusCode, Json<Map<String, Value>>), ApiError> {
    // One transaction for the whole tree. Dropping `tx` without committing
    // rolls every insert back — the safety net for any error in the recursion.
    let mut tx = umbral::db::begin().await?;
    let mut ctx = NestCtx {
        cfg,
        identity,
        nodes: 0,
    };
    let row = insert_nested_tree(&mut ctx, &model, body, &mut tx, 0).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// Recursively insert a row and every nested subtree declared on its table
/// (and on its children's tables, to arbitrary depth) on the open `tx`.
///
/// The nested arrays are discovered from `cfg.nested`, keyed by table, so a
/// grandchild is written iff its parent's table *also* declared `.nested(...)`
/// — one level per declaration, no magic. Each child's FK to its parent is
/// filled from the parent's just-inserted (still-uncommitted) primary key, so
/// clients never repeat the parent id down the tree.
async fn insert_nested_tree(
    ctx: &mut NestCtx<'_>,
    meta: &ModelMeta,
    body: &mut Map<String, Value>,
    tx: &mut umbral::db::Transaction,
    depth: usize,
) -> Result<Map<String, Value>, ApiError> {
    if depth > MAX_NEST_DEPTH {
        return Err(ApiError::BadInput(format!(
            "nested write exceeds the maximum depth of {MAX_NEST_DEPTH}"
        )));
    }

    // Split THIS table's declared nested arrays out of the body BEFORE the
    // insert, so they're never handed to the row insert as unknown columns.
    let specs = ctx.cfg.nested.get(&meta.table).cloned().unwrap_or_default();
    let mut pending: Vec<(String, ModelMeta, String, Vec<Value>)> = Vec::new();
    for (field, child_table) in &specs {
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
        let fk = child_fk_to(&child, &meta.table)?.to_string();
        pending.push((field.clone(), child, fk, items));
    }

    // Anything array-shaped still in `body` is an undeclared nested relation —
    // reject it loudly instead of letting the insert silently drop it.
    reject_undeclared_nested(meta, body)?;

    // Insert this row on the tx.
    let mut row = umbral::orm::DynQuerySet::for_meta(meta)
        .insert_json_in_tx(body, tx)
        .await?;
    let pk_name = pk_column(meta)?.name.clone();
    let pk_value = row
        .get(&pk_name)
        .cloned()
        .ok_or_else(|| ApiError::BadInput("nested: row has no primary key after insert".into()))?;
    ctx.cfg.apply_overrides(&meta.table, &mut row);

    // Recurse into each declared child array.
    for (field, child, fk, items) in pending {
        let mut created = Vec::with_capacity(items.len());
        for item in items {
            let Value::Object(mut child_body) = item else {
                return Err(ApiError::BadInput(format!(
                    "items in nested `{field}` must be objects"
                )));
            };
            // Per-child security (audit_2 H2/H3): count against the tree cap,
            // strip the child's hidden/denied fields (so a parent-writer can't
            // set `is_superuser`/`password_hash` on a nested child), and enforce
            // the child resource's OWN create permission. Strip runs BEFORE the
            // FK is injected so the FK survives.
            ctx.charge_node()?;
            ctx.cfg
                .strip_hidden_for_write(&child.table, ctx.identity, &mut child_body);
            ctx.check_child_perm(&child.table, &Action::Create)?;
            child_body.insert(fk.clone(), pk_value.clone());
            // `Box::pin` breaks the otherwise-infinitely-sized async recursion.
            let crow = Box::pin(insert_nested_tree(
                ctx,
                &child,
                &mut child_body,
                tx,
                depth + 1,
            ))
            .await?;
            created.push(Value::Object(crow));
        }
        row.insert(field, Value::Array(created));
    }
    Ok(row)
}

/// Resolve a child model's `ModelMeta` by table (no allow-gate — nested
/// children are declared by the developer, not exposed as a top-level
/// resource).
fn meta_for_table(table: &str) -> Result<ModelMeta, ApiError> {
    // L-6: a nested child must clear the same block-list its own
    // `/api/<table>/` endpoint would. Without this, a `.nested(...)`
    // pointing at a DEFAULT_BLOCKED_TABLES entry (auth_user, session, …)
    // or an `.exclude(...)`d table becomes writable through the parent's
    // nested payload even though its direct endpoint 404s. `allow`
    // honours `expose`, so an explicitly exposed table still resolves.
    let config = CONFIG.get().expect("RestPlugin::routes was called");
    if !config.allow(table) {
        return Err(ApiError::BadInput(format!(
            "nested: child table `{table}` is not exposed over REST"
        )));
    }
    for plugin in umbral::migrate::registered_plugins() {
        for m in umbral::migrate::models_for_plugin(&plugin) {
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

/// Reject an array-of-values under a key that is neither a column, an M2M
/// relation, nor a declared writable nested relation on this table. Called
/// AFTER the declared nested arrays have been split out of `body`, so anything
/// array-shaped left over is an undeclared nesting attempt. Without this, the
/// row insert/update would iterate only the table's own columns and **silently
/// drop** the array — the level-2+ silent-data-loss footgun (gaps3 #10). A
/// scalar array *column* (e.g. `ArrayField`) or an M2M write-through list stays
/// allowed because it maps to a real relation on the model.
fn reject_undeclared_nested(meta: &ModelMeta, body: &Map<String, Value>) -> Result<(), ApiError> {
    for (key, val) in body {
        if !matches!(val, Value::Array(_)) {
            continue;
        }
        let is_column = meta.fields.iter().any(|c| c.name == *key);
        let is_m2m = meta.m2m_relations.iter().any(|r| r.field_name == *key);
        if !is_column && !is_m2m {
            return Err(ApiError::BadInput(format!(
                "`{key}` on `{}` is not a column, an M2M relation, or a declared writable \
                 nested relation — declare it with \
                 `ResourceConfig::for_::<…>().nested(\"{key}\", \"<child_table>\")` \
                 or remove it from the payload",
                meta.table
            )));
        }
    }
    Ok(())
}

/// Writable nested update (gaps3 #9, recursive since gaps3 #10): update the
/// parent, then upsert each declared nested subtree — to arbitrary depth — all
/// on ONE `umbral::db::Transaction`, the sibling of [`create_nested`] for the
/// `PATCH`/`PUT` path.
///
/// **Reconciliation policy — upsert, no implicit deletes.** At every level, a
/// nested item carrying the row's primary key UPDATES that row, scoped to its
/// parent via the FK so one parent's payload can never mutate another parent's
/// child. An item WITHOUT the pk is CREATED (its whole subtree inserted) with
/// its FK set to the parent. Rows absent from the payload are left untouched: a
/// forgotten item never silently deletes a row. Full replace-set
/// (delete-the-missing) semantics are intentionally deferred to a future
/// opt-in — see gaps3 #9.
///
/// **Atomicity.** Every statement runs on the one open `tx` and only becomes
/// durable at `tx.commit()`. Any failure drops `tx` un-committed and the DB
/// rolls the whole update back — no half-applied writes at any depth.
async fn update_nested(
    cfg: &RestPlugin,
    table: &str,
    model: ModelMeta,
    pk_name: &str,
    id: &str,
    body: &mut Map<String, Value>,
    identity: Option<&Identity>,
) -> Result<Map<String, Value>, ApiError> {
    // Split the parent's declared nested arrays out of the body.
    let specs = cfg.nested.get(table).cloned().unwrap_or_default();
    let mut pending: Vec<(String, ModelMeta, String, Vec<Value>)> = Vec::new();
    for (field, child_table) in &specs {
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
        pending.push((field.clone(), child, fk, items));
    }

    // Anything array-shaped left in `body` is an undeclared nested relation.
    reject_undeclared_nested(&model, body)?;

    // One transaction for the parent update + every nested upsert.
    let mut tx = umbral::db::begin().await?;

    // Update the parent's own columns on the tx. A body with only nested
    // arrays (no scalar columns) is a safe no-op — `update_json_in_tx`
    // returns 0 rather than emitting an UPDATE with no SET clause.
    umbral::orm::DynQuerySet::for_meta(&model)
        .filter_eq_string(pk_name, id)
        .update_json_in_tx(body, &mut tx)
        .await?;

    // The parent's typed pk value, read on the tx — used as the FK when
    // CREATING a child (so an i64 FK gets a number, not the stringified id).
    let pk_value = {
        let parent = fetch_one_in_tx(&model, pk_name, id, &mut tx).await?;
        parent
            .get(pk_name)
            .cloned()
            .ok_or_else(|| ApiError::BadInput("nested: parent row has no primary key".into()))?
    };

    // Upsert each child subtree on the same tx. The parent itself was already
    // gated + hidden-stripped by the `update` handler; `ctx` carries the
    // identity + node budget so each CHILD is checked and counted (H2/H3).
    let mut ctx = NestCtx {
        cfg,
        identity,
        nodes: 0,
    };
    let mut results: Vec<(String, Vec<Value>)> = Vec::new();
    for (field, child, fk, items) in pending {
        let mut upserted = Vec::with_capacity(items.len());
        for item in items {
            let Value::Object(child_body) = item else {
                return Err(ApiError::BadInput(format!(
                    "items in nested `{field}` must be objects"
                )));
            };
            let crow = upsert_nested_child(
                &mut ctx,
                &child,
                child_body,
                &NestAnchor {
                    fk_col: &fk,
                    pk_value: &pk_value,
                    pk_str: id,
                },
                &mut tx,
                1,
            )
            .await?;
            upserted.push(Value::Object(crow));
        }
        results.push((field, upserted));
    }

    // Commit only after every write succeeded.
    tx.commit().await?;

    // Read the parent back and attach the upserted children (the same shape
    // `create_nested` returns: only the children in the payload, hydrated).
    let no_filter = FilterClause::default();
    let mut rows = fetch_rows(
        &model,
        Some((pk_name, id)),
        None,
        &no_filter,
        &[],
        &[],
        &cfg.unlocked_private(&model.table, identity),
    )
    .await?;
    let mut parent = rows
        .pop()
        .ok_or_else(|| ApiError::BadInput("row updated but disappeared on read-back".into()))?;
    cfg.apply_overrides(table, &mut parent);
    for (field, children) in results {
        parent.insert(field, Value::Array(children));
    }
    Ok(parent)
}

/// The parent anchor threaded down one nesting level.
struct NestAnchor<'a> {
    /// Column on the child that FKs back to this parent.
    fk_col: &'a str,
    /// Parent pk as a typed `Value` — set as the child's FK on a CREATE.
    pk_value: &'a Value,
    /// Parent pk as a `String` — scopes an UPDATE's ownership check.
    pk_str: &'a str,
}

/// Recursively upsert one nested item (and its own subtree) during an update.
///
/// The `parent` anchor carries the FK column plus the parent pk in both the
/// typed form (set as the child's FK on a CREATE) and string form (scopes the
/// ownership check on an UPDATE).
///
/// An item WITH its primary key UPDATES that row, but only if it belongs to
/// this parent (`FK == parent pk`), else `404`; it then recurses into its own
/// declared nested arrays (upserting grandchildren). An item WITHOUT a pk
/// CREATEs the whole subtree via [`insert_nested_tree`].
async fn upsert_nested_child(
    ctx: &mut NestCtx<'_>,
    meta: &ModelMeta,
    mut body: Map<String, Value>,
    parent: &NestAnchor<'_>,
    tx: &mut umbral::db::Transaction,
    depth: usize,
) -> Result<Map<String, Value>, ApiError> {
    if depth > MAX_NEST_DEPTH {
        return Err(ApiError::BadInput(format!(
            "nested write exceeds the maximum depth of {MAX_NEST_DEPTH}"
        )));
    }
    // Count this child row against the whole-tree cap (H3).
    ctx.charge_node()?;

    let pk_col = pk_column(meta)?.name.clone();
    let supplied_pk = body.get(&pk_col).filter(|v| !v.is_null()).cloned();

    let Some(pk_json) = supplied_pk else {
        // CREATE — enforce this child's own hidden-field denylist + create
        // permission (H2), set the FK, then insert the whole subtree.
        ctx.cfg
            .strip_hidden_for_write(&meta.table, ctx.identity, &mut body);
        ctx.check_child_perm(&meta.table, &Action::Create)?;
        body.insert(parent.fk_col.to_string(), parent.pk_value.clone());
        return Box::pin(insert_nested_tree(ctx, meta, &mut body, tx, depth)).await;
    };

    // UPDATE — enforce this child's own update permission (H2) before any read.
    let pk_str = json_pk_to_string(&pk_json).ok_or_else(|| {
        ApiError::BadInput(format!("nested `{}`: invalid primary key", meta.table))
    })?;
    ctx.check_child_perm(&meta.table, &Action::Update)?;

    // Ownership gate: the row must exist AND belong to THIS parent. Checked
    // explicitly because `update_json_in_tx`'s return is
    // `matched.max(1 if any SET cols)` — it reports 1 even on a 0-row match,
    // so a cross-parent pk would silently no-op instead of 404.
    let owned = umbral::orm::DynQuerySet::for_meta(meta)
        .filter_eq_string(&pk_col, &pk_str)
        .filter_eq_string(parent.fk_col, parent.pk_str)
        .fetch_one_json_in_tx(tx)
        .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound(format!(
            "nested `{}`: no row with {} = {} belonging to this parent",
            meta.table, pk_col, pk_str
        )));
    }

    // Split this row's own nested arrays out before the scalar update.
    let specs = ctx.cfg.nested.get(&meta.table).cloned().unwrap_or_default();
    let mut pending: Vec<(String, ModelMeta, String, Vec<Value>)> = Vec::new();
    for (field, gc_table) in &specs {
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
        let gc = meta_for_table(gc_table)?;
        let gc_fk = child_fk_to(&gc, &meta.table)?.to_string();
        pending.push((field.clone(), gc, gc_fk, items));
    }

    // The pk is the WHERE key; the FK anchors ownership — never let the
    // payload rewrite either via the SET clause. Then strip the child's
    // hidden/denied fields so a nested UPDATE can't set them either (H2).
    body.remove(&pk_col);
    body.remove(parent.fk_col);
    ctx.cfg
        .strip_hidden_for_write(&meta.table, ctx.identity, &mut body);
    // Anything array-shaped still here is an undeclared nested relation.
    reject_undeclared_nested(meta, &body)?;
    umbral::orm::DynQuerySet::for_meta(meta)
        .filter_eq_string(&pk_col, &pk_str)
        .update_json_in_tx(&body, tx)
        .await?;
    let mut row = fetch_one_in_tx(meta, &pk_col, &pk_str, tx).await?;
    let this_pk_value = row
        .get(&pk_col)
        .cloned()
        .ok_or_else(|| ApiError::BadInput("nested: row has no primary key".into()))?;
    ctx.cfg.apply_overrides(&meta.table, &mut row);

    // Recurse into grandchildren (upsert), scoped to this row.
    for (field, gc, gc_fk, items) in pending {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let Value::Object(gc_body) = item else {
                return Err(ApiError::BadInput(format!(
                    "items in nested `{field}` must be objects"
                )));
            };
            let grow = Box::pin(upsert_nested_child(
                ctx,
                &gc,
                gc_body,
                &NestAnchor {
                    fk_col: &gc_fk,
                    pk_value: &this_pk_value,
                    pk_str: &pk_str,
                },
                tx,
                depth + 1,
            ))
            .await?;
            out.push(Value::Object(grow));
        }
        row.insert(field, Value::Array(out));
    }
    Ok(row)
}

async fn update(
    Path((table, id)): Path<(String, String)>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> Result<Json<Map<String, Value>>, ApiError> {
    update_impl(table, id, None, uri, headers, body).await
}

/// `PUT`/`PATCH /api/{parent}/{parent_id}/{table}/{id}` (gaps3 #29 item 2).
async fn nested_update(
    Path((parent_table, parent_id, table, id)): Path<(String, String, String, String)>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
    Json(body): Json<Map<String, Value>>,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let parent = ParentRef {
        table: parent_table,
        id: parent_id,
    };
    update_impl(table, id, Some(parent), uri, headers, body).await
}

async fn update_impl(
    table: String,
    id: String,
    parent: Option<ParentRef>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
    mut body: Map<String, Value>,
) -> Result<Json<Map<String, Value>>, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let _ctx = RequestContext {
        table: table.clone(),
        identity: identity.clone(),
        version: cfg.resolve_version(uri.path(), &headers)?,
    };
    let model = allowed_model(&table)?;
    cfg.gate(
        &table,
        &Action::Update,
        EndpointKind::Detail,
        identity.as_ref(),
    )?;
    cfg.gate_throttle(
        &table,
        &Action::Update,
        identity.as_ref(),
        throttle_client_ip(&headers).as_deref(),
    )?;
    let pk_name = pk_column(&model)?.name.clone();

    // WEB-2: a hidden field must not be writable (see strip_hidden_for_write).
    cfg.strip_hidden_for_write(&table, identity.as_ref(), &mut body);

    // audit_2 H1/P2: object-level scope. DenyAll → 404; a Filter is ANDed into
    // the existence check, the UPDATE's WHERE, and the read-back, so a caller
    // can't update a row outside their scope by id (the row is Not Found).
    // gaps3 #29 item 2. Deliberately AFTER the permission gate: whether a parent row
    // exists is information, and a caller who may not read this resource must not
    // learn it from the difference between a 403 and a 404.
    let parent_scope = resolve_parent(cfg, &table, parent.as_ref()).await?;
    let scope_cond = match cfg
        .object_scope(&table, identity.as_ref(), parent_scope.as_ref())
        .await
    {
        ObjectScopeOutcome::DenyAll => {
            return Err(ApiError::NotFound(format!(
                "no row with {pk_name} = {id} in {table}"
            )));
        }
        ObjectScopeOutcome::Filter(cond) => Some(cond),
        ObjectScopeOutcome::Unconstrained => None,
    };
    let scope_filter = match &scope_cond {
        Some(c) => FilterClause::default().and(c.clone()),
        None => FilterClause::default(),
    };

    // 404 if the target row doesn't exist (or is out of scope) before the UPDATE.
    let existing = fetch_rows(
        &model,
        Some((&pk_name, &id)),
        None,
        &scope_filter,
        &[],
        &[],
        &cfg.unlocked_private(&model.table, identity.as_ref()),
    )
    .await?;
    if existing.is_empty() {
        return Err(ApiError::NotFound(format!(
            "no row with {pk_name} = {id} in {table}"
        )));
    }

    // gaps3 #9: if this resource declared writable nested children, a PATCH
    // may carry those child arrays alongside the parent's own columns. Split
    // them out and upsert (update by child pk, else create) on ONE tx so the
    // parent update + child writes commit or roll back together, mirroring
    // `create_nested`. Children absent from the payload are left untouched
    // (no implicit deletes; see the reconciliation policy in `update_nested`).
    let nested_specs = cfg.nested.get(&table).cloned().unwrap_or_default();
    if !nested_specs.is_empty() {
        let row = as_user(
            identity.as_ref(),
            update_nested(
                cfg,
                &table,
                model,
                &pk_name,
                &id,
                &mut body,
                identity.as_ref(),
            ),
        )
        .await?;
        return Ok(Json(row));
    }

    // No nesting declared — an array-of-objects is an undeclared nested
    // relation, not an ignorable extra (gaps3 #10).
    reject_undeclared_nested(&model, &body)?;

    // PATCH-style update: only the columns supplied in the body are
    // written, primary key never. The ORM's `update_json` owns
    // validation + constraint classification; `From<WriteError>
    // for ApiError` handles the 400 translation.
    let mut update_qs = umbral::orm::DynQuerySet::for_meta(&model).filter_eq_string(&pk_name, &id);
    if let Some(c) = &scope_cond {
        update_qs = update_qs.filter_condition(c.clone());
    }
    as_user(identity.as_ref(), update_qs.update_json(&body)).await?;
    let mut rows = fetch_rows(
        &model,
        Some((&pk_name, &id)),
        None,
        &scope_filter,
        &[],
        &[],
        &cfg.unlocked_private(&model.table, identity.as_ref()),
    )
    .await?;
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
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
) -> Result<StatusCode, ApiError> {
    destroy_impl(table, id, None, uri, headers).await
}

/// `DELETE /api/{parent}/{parent_id}/{table}/{id}` (gaps3 #29 item 2).
async fn nested_destroy(
    Path((parent_table, parent_id, table, id)): Path<(String, String, String, String)>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
) -> Result<StatusCode, ApiError> {
    let parent = ParentRef {
        table: parent_table,
        id: parent_id,
    };
    destroy_impl(table, id, Some(parent), uri, headers).await
}

async fn destroy_impl(
    table: String,
    id: String,
    parent: Option<ParentRef>,
    uri: axum::http::Uri,
    headers: umbral::web::HeaderMap,
) -> Result<StatusCode, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let identity = cfg.authentication.authenticate(&headers).await;
    let _ctx = RequestContext {
        table: table.clone(),
        identity: identity.clone(),
        version: cfg.resolve_version(uri.path(), &headers)?,
    };
    let model = allowed_model(&table)?;
    cfg.gate(
        &table,
        &Action::Delete,
        EndpointKind::Detail,
        identity.as_ref(),
    )?;
    cfg.gate_throttle(
        &table,
        &Action::Delete,
        identity.as_ref(),
        throttle_client_ip(&headers).as_deref(),
    )?;
    let pk = pk_column(&model)?;
    // audit_2 H1/P2: scope the DELETE. DenyAll → 404; a Filter is ANDed into
    // the WHERE, so deleting an out-of-scope row affects 0 rows → 404 (a caller
    // can't delete another owner's/tenant's row by id).
    // gaps3 #29 item 2. Deliberately AFTER the permission gate: whether a parent row
    // exists is information, and a caller who may not read this resource must not
    // learn it from the difference between a 403 and a 404.
    let parent_scope = resolve_parent(cfg, &table, parent.as_ref()).await?;
    let scope_cond = match cfg
        .object_scope(&table, identity.as_ref(), parent_scope.as_ref())
        .await
    {
        ObjectScopeOutcome::DenyAll => {
            return Err(ApiError::NotFound(format!(
                "no row with {} = {} in {}",
                pk.name, id, table
            )));
        }
        ObjectScopeOutcome::Filter(cond) => Some(cond),
        ObjectScopeOutcome::Unconstrained => None,
    };
    let mut delete_qs = umbral::orm::DynQuerySet::for_meta(&model).filter_eq_string(&pk.name, &id);
    if let Some(c) = scope_cond {
        delete_qs = delete_qs.filter_condition(c);
    }
    let affected = delete_qs.delete().await?;
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
    headers: umbral::web::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    let cfg = CONFIG.get().expect("RestPlugin::routes was called");
    let version = cfg.resolve_version(uri.path(), &headers)?;
    let (table, name, pk) = parse_action_route(uri.path(), &cfg.base_path, version.as_deref())?;

    // L-7: re-check the block-list at dispatch, exactly as every CRUD
    // handler does via `allowed_model`. An `@action` registered on a
    // table that is blocked (DEFAULT_BLOCKED_TABLES / `.exclude(...)`)
    // must 404 like its CRUD siblings instead of staying reachable.
    let _ = allowed_model(&table)?;

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
    let custom = Action::Custom(name.clone());
    // Custom actions are never view-scoped out (`view_exposed` returns
    // true for them), so `kind` only shapes a 405 that can't occur here;
    // pass the kind matching the action's own collection/detail scope.
    let action_kind = match def.scope {
        ActionScope::Collection => EndpointKind::Collection,
        ActionScope::Detail => EndpointKind::Detail,
    };
    cfg.gate(&table, &custom, action_kind, identity.as_ref())?;
    cfg.gate_throttle(
        &table,
        &custom,
        identity.as_ref(),
        throttle_client_ip(&headers).as_deref(),
    )?;

    // Parse the request body leniently. An empty body — including a GET that
    // carries `Content-Type: application/json` with nothing after it, which
    // is what browser-based API explorers (the playground, Swagger UI) send —
    // means "no body", not invalid JSON. Only non-empty bytes are parsed, and
    // a genuine parse failure is a clean 400 rather than the 500 the old
    // `Option<Json<Value>>` extractor produced ("EOF while parsing a value").
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&body).map_err(ApiError::Json)?
    };

    let query = parse_query_string(uri.query().unwrap_or(""));
    let ctx = ActionContext {
        table: table.clone(),
        name: name.clone(),
        pk,
        identity,
        body,
        query,
        version,
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

#[cfg(test)]
mod parse_query_string_unit {
    use super::parse_query_string;

    #[test]
    fn percent_decodes_values() {
        // What the playground (correctly) sends for ?at=2026-06-26T19:03:00Z.
        let q = parse_query_string("at=2026-06-26T19%3A03%3A00Z&symbol=btcusd_chainlink");
        assert_eq!(
            q.get("at").map(String::as_str),
            Some("2026-06-26T19:03:00Z")
        );
        assert_eq!(
            q.get("symbol").map(String::as_str),
            Some("btcusd_chainlink")
        );
    }

    #[test]
    fn decodes_plus_as_space_and_encoded_keys() {
        let q = parse_query_string("full+name=ada+lovelace&a%3Ab=c");
        assert_eq!(q.get("full name").map(String::as_str), Some("ada lovelace"));
        assert_eq!(q.get("a:b").map(String::as_str), Some("c"));
    }

    #[test]
    fn bare_key_and_empty_string_tolerated() {
        let q = parse_query_string("flag&empty=");
        assert_eq!(q.get("flag").map(String::as_str), Some(""));
        assert_eq!(q.get("empty").map(String::as_str), Some(""));
    }
}

/// Decode a `key=value&key=value` query string into a HashMap, with keys
/// AND values percent-decoded (`%3A` → `:`, `+` → space). Query values
/// arrive percent-encoded over the wire — a correct client encodes
/// `at=2026-06-26T19:03:00Z` as `at=2026-06-26T19%3A03%3A00Z` — so a handler
/// reading `ctx.query.get("at")` wants the decoded `2026-06-26T19:03:00Z`,
/// not the raw bytes. `form_urlencoded::parse` is the standard query parser
/// and handles the splitting + decoding in one pass.
fn parse_query_string(q: &str) -> std::collections::HashMap<String, String> {
    form_urlencoded::parse(q.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Parse `{base}/<table>/<name>` and `{base}/<table>/<id>/<name>` —
/// trailing slash tolerated. Returns `(table, action_name, pk)` where
/// `pk` is `Some(id)` for detail-scope.
///
/// `base_path` (e.g. `"/api"`) and `version` (e.g. `Some("v1")` under
/// URL-path versioning) are stripped off the front first, so the same
/// parser works for `/api/post/recent` and `/api/v1/post/recent`.
fn parse_action_route(
    path: &str,
    base_path: &str,
    version: Option<&str>,
) -> Result<(String, String, Option<String>), ApiError> {
    let trimmed = path.trim_end_matches('/');
    let mut segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();

    // Strip the base-path segments (`/api` → ["api"], `/internal/api` →
    // ["internal", "api"]) off the front.
    for base_seg in base_path.split('/').filter(|s| !s.is_empty()) {
        if segments.first() == Some(&base_seg) {
            segments.remove(0);
        } else {
            return Err(ApiError::NotFound(format!(
                "{path} is not a recognised @action route"
            )));
        }
    }
    // Under URL-path versioning, the next segment is the version — drop it.
    if let Some(v) = version {
        if segments.first() == Some(&v) {
            segments.remove(0);
        }
    }

    match segments.as_slice() {
        [table, name] => Ok((table.to_string(), name.to_string(), None)),
        [table, id, name] => Ok((table.to_string(), name.to_string(), Some(id.to_string()))),
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
    ordering: &[(String, bool)],
    // `#[umbral(private)]` columns THIS caller has unlocked (`ResourceConfig::allow_private_if`).
    // Empty for everyone else, which is the default and the safe direction: the ORM does not
    // even SELECT a private column it was not told to, so the value never leaves the database.
    unlocked_private: &[String],
) -> Result<Vec<Map<String, Value>>, ApiError> {
    let mut qs = umbral::orm::DynQuerySet::for_meta(model);
    if !unlocked_private.is_empty() {
        let refs: Vec<&str> = unlocked_private.iter().map(String::as_str).collect();
        qs = qs.allow_private(&refs);
    }

    if let Some((col, val)) = where_clause {
        // Single-row lookup (retrieve / update / delete read-back).
        // The WHERE col = val + LIMIT 1 shape is the same as before.
        qs = qs.filter_eq_string(col, val);
        // audit_2 H1/P2: apply the object-scope filter on the single-row path
        // too, so an out-of-scope row is simply NOT FOUND (no oracle) rather
        // than returned by id. The list path applies it below.
        if !filter.is_empty()
            && let Some(cond) = filter.condition_clone()
        {
            qs = qs.filter_condition(cond);
        }
        qs = qs.limit(1);
    } else {
        // List path: pagination applies, plus any filter the resource
        // opted in to. `FilterClause` ANDs every parsed predicate.
        if !filter.is_empty()
            && let Some(cond) = filter.condition_clone()
        {
            qs = qs.filter_condition(cond);
        }
        // `?ordering=-created_at,name` — apply each directive in order.
        // Unknown fields were already stripped by `parse_ordering`; what
        // remains are validated column names safe to pass directly to
        // `order_by_col` without further SQL-injection risk.
        for (col, desc) in ordering {
            qs = qs.order_by_col(col, *desc);
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
    let registered = umbral::migrate::registered_models();
    let mut out: Vec<String> = Vec::new();
    for token in raw.split(',') {
        let name = token.trim();
        if name.is_empty() {
            continue;
        }
        // Accept both `.` (URL-natural) and `__` (common
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
    let mut qs = umbral::orm::DynQuerySet::for_meta(model);
    if !filter.is_empty()
        && let Some(cond) = filter.condition_clone()
    {
        qs = qs.filter_condition(cond);
    }
    Ok(qs.count().await?)
}

// The `noform` filtering tests that used to live here moved
// into the ORM alongside the logic — see
// `crates/umbral-core/src/orm/dynamic.rs`'s test module. The
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
        assert!(!p.allow("umbral_migrations"));
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
        assert!(!p.allow("umbral_migrations"));
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

    // ---- M-4: anonymous-read boot-warning decision helper ----

    #[test]
    fn allows_anonymous_read_true_under_default_readonly() {
        // The default (safe) permission is ReadOnly — it serves anonymous
        // reads of every non-blocked business table. That IS the exposure
        // M-4 wants the boot warning to surface.
        let p = RestPlugin::new();
        assert!(p.allows_anonymous_read("article"));
        assert!(p.allows_anonymous_read("customer"));
    }

    #[test]
    fn allows_anonymous_read_false_for_blocked_tables() {
        // A blocked table isn't served at all, so it isn't an anonymous
        // read exposure regardless of the permission.
        let p = RestPlugin::new();
        assert!(!p.allows_anonymous_read("auth_user"));
        assert!(!p.allows_anonymous_read("session"));
    }

    #[test]
    fn allows_anonymous_read_false_when_default_requires_auth() {
        // Behind IsAuthenticated the anonymous caller is denied even reads,
        // so there is nothing to warn about.
        let p = RestPlugin::new().default_permission(crate::IsAuthenticated);
        assert!(!p.allows_anonymous_read("article"));
    }

    #[test]
    fn allows_anonymous_read_true_under_allow_any_too() {
        // AllowAny also allows anonymous reads (the open-CRUD warning is a
        // superset; the boot code partitions on `is_open` so these aren't
        // double-counted).
        let p = RestPlugin::new().default_permission(crate::AllowAny);
        assert!(p.allows_anonymous_read("article"));
    }

    // ---- M-5: write-without-throttle boot-warning decision helpers ----

    #[test]
    fn has_no_throttle_true_by_default_false_once_set() {
        assert!(RestPlugin::new().has_no_throttle());
        let p = RestPlugin::new().default_throttle(crate::AnonRateThrottle::new("100/hour"));
        assert!(!p.has_no_throttle());
    }

    #[test]
    fn permits_writes_false_under_default_readonly() {
        // ReadOnly denies Create/Update/Delete to everyone, including a
        // staff identity — a read-only resource needs no write throttle.
        let p = RestPlugin::new();
        assert!(!p.permits_writes("article"));
    }

    #[test]
    fn permits_writes_true_when_writes_are_allowed() {
        // AllowAny and IsAuthenticated both let some caller write, so they
        // count as write endpoints for the throttle warning.
        assert!(
            RestPlugin::new()
                .default_permission(crate::AllowAny)
                .permits_writes("article")
        );
        assert!(
            RestPlugin::new()
                .default_permission(crate::IsAuthenticated)
                .permits_writes("article")
        );
    }

    #[test]
    fn permits_writes_false_for_blocked_tables() {
        // Even with an open default, a blocked table isn't a write surface.
        let p = RestPlugin::new().default_permission(crate::AllowAny);
        assert!(!p.permits_writes("auth_user"));
    }

    // ---- L-8: internal error strings are not echoed to the client ----

    #[test]
    fn protocol_sqlx_error_maps_to_generic_400_without_leaking_detail() {
        // A driver/wire-level Protocol error must become a generic 400 —
        // its text (which can carry column metadata / internal wire detail)
        // stays server-side.
        let leaked = "unexpected server column metadata secret-table.secret_col";
        let err: ApiError = sqlx::Error::Protocol(leaked.into()).into();
        match err {
            ApiError::BadInput(msg) => {
                assert_eq!(msg, "malformed request");
                assert!(!msg.contains("secret"), "must not echo the protocol detail");
            }
            other => panic!("expected BadInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn json_parse_error_body_is_generic() {
        use http_body_util::BodyExt;
        use umbral::web::IntoResponse;

        // A serde parse error carries text that can echo internal type
        // names; the response body must be a fixed generic message.
        let parse_err = serde_json::from_str::<Value>("{ not json").unwrap_err();
        let detail = parse_err.to_string();
        let resp = ApiError::Json(parse_err).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let rendered = body.to_string();
        assert!(
            rendered.contains("request body is not valid JSON"),
            "body should carry the generic message, got {rendered}"
        );
        assert!(
            !rendered.contains(&detail),
            "body must not echo the raw serde parse detail"
        );
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

#[cfg(test)]
mod csv_writer_unit {
    //! Unit tests for `rows_to_csv` / `rows_to_csv_into`.
    //!
    //! The happy-path test verifies header order + cell quoting without
    //! spinning up the HTTP stack.  The error-path test injects a `Write`
    //! implementation that always returns an `io::Error` and asserts that
    //! `rows_to_csv_into` surfaces the error rather than swallowing it.

    use super::{rows_to_csv, rows_to_csv_into};
    use serde_json::{Map, Value, json};
    use umbral::migrate::ModelMeta;
    use umbral::orm::SqlType;

    fn make_meta(fields: &[(&str, SqlType)]) -> ModelMeta {
        // Build via JSON round-trip so we stay insulated from new
        // `#[serde(default)]` fields added in the future. We must
        // supply the three non-defaulted `Column` fields explicitly:
        // `name`, `ty`, `primary_key`, and `nullable`.
        let cols: Vec<serde_json::Value> = fields
            .iter()
            .map(|(n, ty)| {
                serde_json::json!({
                    "name": n,
                    "ty": ty,
                    "primary_key": false,
                    "nullable": false,
                })
            })
            .collect();
        let json = serde_json::json!({
            "name": "Test",
            "table": "test",
            "fields": cols,
        });
        serde_json::from_value(json).expect("ModelMeta round-trip")
    }

    fn row(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    /// Happy path: header follows model field order; a value containing a
    /// comma is quoted by the csv crate; `rows_to_csv` returns `Ok`.
    #[test]
    fn happy_path_header_and_quoting() {
        let meta = make_meta(&[("id", SqlType::BigInt), ("name", SqlType::Text)]);
        let rows = vec![
            row(&[("id", json!(1)), ("name", json!("Anvil"))]),
            row(&[("id", json!(2)), ("name", json!("Rope, sturdy"))]),
        ];
        let csv = rows_to_csv(&meta, &rows).expect("rows_to_csv should succeed");
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some("id,name"), "header row");
        let body: Vec<&str> = lines.collect();
        assert!(
            body.iter().any(|l| l.contains("Anvil")),
            "first row present"
        );
        assert!(
            body.iter().any(|l| l.contains("\"Rope, sturdy\"")),
            "comma value is quoted: {body:?}",
        );
    }

    /// Empty rows: the function succeeds (no panic / no Err), produces
    /// valid UTF-8, and the output terminates with a newline. Column
    /// detection requires at least one row to confirm a field is
    /// present, so the zero-row case emits an empty header — that's the
    /// documented behaviour and not a bug; this test just guards that we
    /// don't crash or return an Err.
    #[test]
    fn empty_rows_does_not_error() {
        let meta = make_meta(&[("id", SqlType::BigInt), ("name", SqlType::Text)]);
        let result = rows_to_csv(&meta, &[]);
        assert!(result.is_ok(), "zero rows must not return Err: {result:?}");
    }

    /// Error path: a `Write` impl that always fails causes `rows_to_csv_into`
    /// to return `Err(...)` rather than swallowing the failure and returning
    /// an empty/truncated result.
    #[test]
    fn write_error_is_surfaced_not_swallowed() {
        /// A `Write` that always returns an IO error on the first write.
        #[derive(Debug)]
        struct AlwaysError;
        impl std::io::Write for AlwaysError {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "injected write failure",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "injected flush failure",
                ))
            }
        }

        let meta = make_meta(&[("id", SqlType::BigInt), ("name", SqlType::Text)]);
        let rows = vec![row(&[("id", json!(1)), ("name", json!("x"))])];

        let result = rows_to_csv_into(AlwaysError, &meta, &rows);
        assert!(
            result.is_err(),
            "a failing writer must propagate Err, not return truncated data"
        );
        let msg = result.unwrap_err();
        // The csv crate buffers writes internally; the injected IO error
        // surfaces when `into_inner()` flushes the buffer, so the message
        // identifies the flush stage rather than the record-write stage.
        assert!(
            msg.contains("csv header write failed") || msg.contains("csv flush failed"),
            "error message identifies a write or flush stage: {msg:?}",
        );
    }
}

/// Stamp `Cache-Control` on REST responses (gaps3 #36).
///
/// A `200 application/json` with NO cache directive is *heuristically cacheable*
/// by browsers and shared proxies (RFC 9111 §4.2.2). On a mutable API that is a
/// data-loss bug, not a perf nit: a refetch immediately after a write can be
/// served the pre-write snapshot from cache and silently clobber fresh state.
///
/// Never overwrites a header a handler set on purpose — an explicit directive
/// from a custom action wins over the default.
async fn cache_control_layer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path().to_string();
    let mut res = next.run(req).await;

    if res.headers().contains_key(http::header::CACHE_CONTROL) {
        return res;
    }
    let Some(cfg) = CONFIG.get() else {
        return res;
    };
    // `<base>/<table>/...` — the table is the segment after the base path.
    let table = path
        .strip_prefix(cfg.base_path.as_str())
        .map(|rest| rest.trim_start_matches('/'))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("");
    let value = cfg.cache_controls.get(table).unwrap_or(&cfg.cache_control);
    if let Ok(v) = http::HeaderValue::from_str(value) {
        res.headers_mut().insert(http::header::CACHE_CONTROL, v);
    }
    res
}
