//! Per-table REST customization bundles.
//!
//! [`ResourceConfig`] groups every customization for ONE table
//! (`hide` / `transform` / `computed`) into a single value that any
//! module — a plugin crate, a free function, `main.rs` itself —
//! can build and hand to [`crate::RestPlugin::resource`].
//!
//! Why this matters: without this type the only way to customize REST
//! responses was the per-call builder chain on `RestPlugin` itself —
//! `.hide("user", "password_hash").transform("user", ...)`. Every
//! customization landed in `main.rs` because that's where
//! `RestPlugin` was constructed. With `ResourceConfig` the user
//! plugin (or whatever module owns the `User` model) can define its
//! REST shape next to the model:
//!
//! ```ignore
//! // plugins/users/src/lib.rs
//! pub fn rest_resource() -> umbral_rest::ResourceConfig {
//!     umbral_rest::ResourceConfig::new("user")
//!         .hide("password_hash")
//!         .transform("email", mask_email)
//!         .computed("display_name", display_name)
//! }
//!
//! // main.rs
//! RestPlugin::default()
//!     .resource(users::rest_resource())
//!     .resource(posts::rest_resource())
//! ```
//!
//! ## Composition with the per-call builders
//!
//! `ResourceConfig` doesn't *replace* the existing
//! `RestPlugin::hide` / `.transform` / `.computed` builders — it
//! complements them. Calls land in the same vecs internally, so
//! mixing the two is fine:
//!
//! ```ignore
//! RestPlugin::default()
//!     .resource(users::rest_resource())     // bundled user customization
//!     .hide("audit_log", "user_id")          // one-off case in main.rs
//! ```
//!
//! `ResourceConfig` keeps the per-model REST shape next to the model
//! (serializers per app/model), not in the project's wiring layer.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use http::Method;
use serde_json::{Map, Value};

use crate::auth::Identity;
use crate::permission::{Action, Permission};
use crate::throttle::Throttle;
use crate::{ComputedFn, HideFields, TransformFn};

/// Whether a custom action is mounted on the collection
/// (`/api/<table>/<name>/`) or on a single row
/// (`/api/<table>/<id>/<name>/`): collection-scoped vs row-scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionScope {
    /// Mounted on the resource as a whole: no `{id}` segment. Use for
    /// "recent posts", "search", "stats", anything keyed off the
    /// collection or query params, not one row.
    Collection,
    /// Mounted on a single row: the URL carries `{id}` before the
    /// action name. Use for "publish this post", "archive this user",
    /// anything that takes a single primary key.
    Detail,
}

/// Context handed to every `@action` handler at invocation time.
/// Bundles the resolved identity, the parsed JSON body (`null` when
/// the request body was empty), the query-string map, and — for
/// detail-scope actions — the primary-key string the client sent.
#[derive(Debug, Clone)]
pub struct ActionContext {
    /// The table the action is mounted on (e.g. `"post"`).
    pub table: String,
    /// The custom action's name as written in the URL
    /// (e.g. `"publish"`).
    pub name: String,
    /// Detail-scope only: the primary-key value the client sent, as the raw URL segment.
    /// `None` for collection-scope actions.
    ///
    /// **Parse it against your model's PK type, not against `i64`** (gaps3 #59). This
    /// doc-comment used to say "parse with `.parse::<i64>()`", which is wrong the moment
    /// a model has a `String` or `Uuid` primary key — and doc-comments that hand you a
    /// snippet decide the code that gets written:
    ///
    /// ```ignore
    /// let pk: <Post as Model>::PrimaryKey = ctx.pk.as_deref().unwrap_or_default().parse()?;
    /// ```
    ///
    /// Or skip parsing entirely and let the ORM coerce it against the column:
    ///
    /// ```ignore
    /// Post::objects().filter(post::ID.eq(pk)) // typed
    /// // or, on the dynamic path:
    /// DynQuerySet::for_meta(&meta).filter_eq_string(&pk_name, raw)
    /// ```
    pub pk: Option<String>,
    /// Whoever the auth backend resolved. `None` is anonymous.
    pub identity: Option<Identity>,
    /// The JSON body. `Value::Null` when the request had no body or
    /// the body was literally `null`.
    pub body: Value,
    /// The query-string parameters as `(key, value)` pairs.
    pub query: std::collections::HashMap<String, String>,
    /// The resolved API version for this request, or `None` when
    /// versioning is off (the default) / the request carried none.
    /// See [`RestPlugin::versioning`](crate::RestPlugin::versioning).
    pub version: Option<String>,
}

/// Per-request context the built-in CRUD handlers resolve before
/// dispatching. Bundles the table, the authenticated identity, and the
/// resolved API version so handlers — and, later, `transform` / `computed`
/// callbacks — can branch on who's calling and which version they asked
/// for.
///
/// `version` is `None` unless the plugin opted into
/// [`RestPlugin::versioning`](crate::RestPlugin::versioning); see that
/// method and the [`versioning`](crate::versioning) module for the two
/// schemes (URL-path and accept-header).
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// The table the request targets (e.g. `"post"`).
    pub table: String,
    /// Whoever the auth backend resolved. `None` is anonymous.
    pub identity: Option<Identity>,
    /// The resolved API version (`"v1"`, `"v2"`, ...), or `None` when
    /// versioning is off or the request carried no recognisable version.
    pub version: Option<String>,
}

/// Errors a custom action handler can return. Maps to the same JSON
/// envelope the built-in handlers use.
#[derive(Debug)]
pub enum ActionError {
    /// 400 — bad input. Use for unprocessable bodies or missing
    /// required fields.
    BadInput(String),
    /// 404 — target row missing (detail-scope actions on a deleted
    /// row, etc.).
    NotFound(String),
    /// 401 — authentication required. Permission rules raise this
    /// before the handler runs; you can also raise it from a handler
    /// that needs to enforce its own auth.
    Unauthenticated,
    /// 403 — authenticated but forbidden.
    Forbidden,
    /// 500 — internal failure, with a short message. Database
    /// errors and other unexpected failures.
    Internal(String),
}

impl ActionError {
    /// Wrap any `Display` value into an `Internal(...)` variant, the
    /// shortcut for `.map_err(ActionError::internal)` on `?` chains.
    pub fn internal(e: impl std::fmt::Display) -> Self {
        ActionError::Internal(e.to_string())
    }
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadInput(m) => write!(f, "{m}"),
            Self::NotFound(m) => write!(f, "{m}"),
            Self::Unauthenticated => write!(f, "authentication required"),
            Self::Forbidden => write!(f, "forbidden"),
            Self::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ActionError {}

impl From<sqlx::Error> for ActionError {
    fn from(e: sqlx::Error) -> Self {
        ActionError::Internal(e.to_string())
    }
}

/// The boxed-future shape every registered action collapses to.
/// Internal-only — users go through the `.action(...)` builder.
pub(crate) type ActionFuture =
    Pin<Box<dyn Future<Output = Result<Value, ActionError>> + Send + 'static>>;

/// The stored action-handler closure. `Arc<dyn Fn>` so the plugin
/// can clone refs cheaply when mounting routes.
pub(crate) type ActionHandler = Arc<dyn Fn(ActionContext) -> ActionFuture + Send + Sync + 'static>;

/// One registered `@action` endpoint: HTTP method, collection-or-detail
/// scope, action name, and the handler closure. Stored on
/// `ResourceConfig`; the plugin merges them into its own per-table
/// vec during `RestPlugin::resource(...)`.
#[derive(Clone)]
pub(crate) struct ActionDef {
    pub(crate) name: String,
    pub(crate) method: Method,
    pub(crate) scope: ActionScope,
    pub(crate) handler: ActionHandler,
    /// Optional JSON Schema for the request body. When set, the dispatch
    /// validates the body against it (the common `type`/`required`/
    /// `properties`/`enum` subset) before the handler runs, and the schema
    /// is published into the OpenAPI spec.
    pub(crate) input_schema: Option<Value>,
    /// Optional JSON Schema for the 200 response — published into OpenAPI
    /// (the playground reads it). Not validated at runtime.
    pub(crate) output_schema: Option<Value>,
}

impl std::fmt::Debug for ActionDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionDef")
            .field("name", &self.name)
            .field("method", &self.method)
            .field("scope", &self.scope)
            .finish()
    }
}

/// The rows a request may see/act on, decided per-request from the caller's
/// [`Identity`] (audit_2 H1/P2 — object-level scoping). Returned by a
/// [`ResourceConfig::scope`] hook and applied to EVERY built-in CRUD action
/// (list / retrieve / update / destroy), so a caller can't reach another
/// tenant's / owner's row by id.
pub enum ScopeDecision {
    /// No additional constraint — every row is in scope (the default when no
    /// scope hook is set).
    All,
    /// Restrict to rows where every `(column, value)` equality holds (ANDed).
    /// The canonical owner scope is `vec![("owner_id".into(), id.user_id.clone())]`.
    Restrict(Vec<(String, String)>),
    /// Restrict to rows whose `column` is one of `values` — `column IN (…)`.
    ///
    /// The membership case [`Self::Restrict`] cannot express: a caller who
    /// belongs to *several* clubs/teams/workspaces sees rows from all of them.
    /// `Restrict` is equality-only and ANDed, so `club_id = 1 AND club_id = 2`
    /// matches nothing.
    ///
    /// **An empty `values` means no rows** — the same as [`Self::None`], never
    /// "all rows". A user who belongs to nothing must see nothing; the failure
    /// mode of the opposite default is a data leak.
    RestrictIn(String, Vec<String>),
    /// No rows are in scope — e.g. an anonymous caller on an owner-scoped
    /// resource. List returns an empty page; retrieve/update/destroy 404
    /// (a non-owned row is indistinguishable from a missing one — no oracle).
    None,
}

/// A per-request row-scoping hook: maps the caller's [`Identity`] (or `None`
/// for anonymous) to a [`ScopeDecision`]. Installed via
/// [`ResourceConfig::scope`] / [`ResourceConfig::owned_by`].
/// Async because the interesting scopes need a database round-trip: "the rows
/// belonging to any club this user is a member of" is a query, not a field on
/// the `Identity`. A sync hook can express `owner_id = me` and nothing more.
pub(crate) type ObjectScopeFn = Arc<
    dyn Fn(Option<Identity>) -> Pin<Box<dyn Future<Output = ScopeDecision> + Send>> + Send + Sync,
>;

/// Bundled REST customization for one table. Build via
/// [`Self::new`] + chainable methods; register with
/// [`crate::RestPlugin::resource`].
///
/// Fields are public-ish via the constructor + builder methods
/// only — the closure-bearing vecs are kept private because the
/// `ComputedFn` / `TransformFn` types are internal implementation
/// detail.
/// Decides whether THIS caller may see a `#[umbral(private)]` column.
pub(crate) type PrivateFn =
    Arc<dyn Fn(Option<&umbral::auth::Identity>) -> bool + Send + Sync + 'static>;

pub struct ResourceConfig {
    pub(crate) table: String,
    pub(crate) hidden: Vec<String>,
    /// `#[umbral(private)]` columns this resource can unlock, and for whom.
    pub(crate) private_unlocks: Vec<(String, PrivateFn)>,
    pub(crate) transforms: Vec<(String, TransformFn)>,
    pub(crate) computed: Vec<(String, ComputedFn)>,
    /// Permission class for this resource. `None` defaults to
    /// [`crate::permission::AllowAny`] at merge time.
    pub(crate) permission: Option<Arc<dyn Permission>>,
    /// Throttles for this resource. Run (after the plugin-wide
    /// `default_throttle`s) on every request to this table — all must
    /// pass, the first to deny returns 429. Empty = no per-table
    /// throttle. Merged into the plugin's per-table map at `.resource()`.
    pub(crate) throttles: Vec<Arc<dyn Throttle>>,
    /// Opt-in view scope. `None` means "all actions exposed" — the
    /// backward-compatible default. `Some(set)` restricts the
    /// resource to exactly that set; everything else 404s.
    pub(crate) view_scope: Option<HashSet<Action>>,
    /// Custom-action endpoints registered on this resource.
    /// Merged into the plugin's per-table action map at `.resource()`
    /// time; mounted as new axum routes at `RestPlugin::routes()`.
    pub(crate) actions: Vec<ActionDef>,
    /// Opt OUT of query-string filtering on the
    /// list endpoint for this resource. Filters are ON by default —
    /// every column gets the standard lookup grammar (`__eq`, `__in`,
    /// `__contains`, etc.) — and `.disable_filters()` removes them
    /// for tables where filtering is undesirable (audit logs,
    /// append-only event streams, etc.).
    pub(crate) filters_disabled: bool,
    /// Opt OUT of `?search=<term>` free-text search on this resource.
    /// Search is ON by default and walks every searchable column.
    pub(crate) search_disabled: bool,
    /// Restrict `?search=` to a specific subset of columns. When
    /// `None`, every searchable column participates (Text +
    /// numeric + FK + Boolean — see `filtering::parse_search`).
    /// When `Some(list)`, only those column names contribute.
    pub(crate) search_fields: Option<Vec<String>>,
    /// Writable nested resources: `(json_field, child_table)`. A `POST`
    /// with `{ ..., "<json_field>": [ {child}, ... ] }` creates the parent
    /// then each child (with its FK to the parent set), returning the full
    /// nested object. Declared via [`ResourceConfig::nested`].
    pub(crate) nested: Vec<(String, String)>,
    /// Opt IN to bulk endpoints (gaps2 #82). `false` (the
    /// default) keeps the resource byte-for-byte unchanged: a `POST` with
    /// a JSON array is rejected as a bad single-object body, and no
    /// collection-level `PATCH` / `DELETE` is mounted. `true` enables:
    /// bulk create (`POST` an array), bulk update (`PATCH` an array of
    /// objects each carrying its PK), and bulk delete
    /// (`DELETE { "ids": [...] }`) — each transactional + subject to the
    /// SAME permission / throttle / field-denylist / blocked-table checks
    /// as the single-object handlers. Declared via [`ResourceConfig::bulk`].
    pub(crate) bulk: bool,
    /// Object-level row scope (audit_2 H1/P2). `None` = every row is reachable
    /// (the backward-compatible default); `Some(fn)` restricts every built-in
    /// CRUD action to the rows the caller may access. Declared via
    /// [`Self::scope`] / [`Self::owned_by`].
    pub(crate) scope: Option<ObjectScopeFn>,
    /// Per-resource `Cache-Control` override (gaps3 #36). `None` → the plugin
    /// default (`no-store`). Set this on a genuinely cacheable read endpoint.
    pub(crate) cache_control: Option<String>,
    /// gaps3 #16: owner-field injection on create. `Some(col)` fills `col` from
    /// the authenticated identity's user id when a row is created, and rejects a
    /// body-supplied value — so a client can't create a row owned by someone
    /// else. Declared via [`ResourceConfig::owner_field`].
    pub(crate) owner_field: Option<String>,
    /// gaps3 #29 item 2 — `(parent_table, fk_column)`. The resource is mounted under
    /// its parent's URL and scoped to it.
    pub(crate) under: Option<(String, String)>,
}

impl std::fmt::Debug for ResourceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceConfig")
            .field("table", &self.table)
            .field("hidden", &self.hidden)
            .field("transforms_count", &self.transforms.len())
            .field("computed_count", &self.computed.len())
            .field("actions", &self.actions)
            .finish()
    }
}

impl ResourceConfig {
    /// Start a new resource config for the given table.
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            hidden: Vec::new(),
            private_unlocks: Vec::new(),
            transforms: Vec::new(),
            computed: Vec::new(),
            permission: None,
            throttles: Vec::new(),
            view_scope: None,
            actions: Vec::new(),
            filters_disabled: false,
            search_disabled: false,
            search_fields: None,
            nested: Vec::new(),
            bulk: false,
            scope: None,
            cache_control: None,
            owner_field: None,
            under: None,
        }
    }

    /// Restrict every built-in CRUD action (list / retrieve / update /
    /// destroy) to the rows the caller may access (audit_2 H1/P2 — object-level
    /// authorization / IDOR fix). Without a scope, model-level permission only
    /// gates *whether* a caller may use the endpoint, not *which rows* — so any
    /// caller past the gate can read/mutate any row by id.
    ///
    /// The hook maps the authenticated [`Identity`] (or `None` for anonymous)
    /// to a [`ScopeDecision`]. The decision is ANDed into the query, so an
    /// out-of-scope row returns `404` (never revealing it exists) and list only
    /// returns in-scope rows.
    ///
    /// ```ignore
    /// use umbral_rest::{ResourceConfig, ScopeDecision};
    /// ResourceConfig::new("order").scope(|identity| match identity {
    ///     Some(id) if id.is_staff => ScopeDecision::All,          // staff see all
    ///     Some(id) => ScopeDecision::Restrict(vec![("owner_id".into(), id.user_id.clone())]),
    ///     None => ScopeDecision::None,                            // anonymous see none
    /// });
    /// ```
    pub fn scope<F>(self, f: F) -> Self
    where
        F: Fn(Option<&Identity>) -> ScopeDecision + Send + Sync + 'static,
    {
        // The sync hook is the async one with a ready future — one code path.
        self.scope_async(move |identity| {
            let decision = f(identity.as_ref());
            std::future::ready(decision)
        })
    }

    /// [`Self::scope`] for a decision that needs to hit the database.
    ///
    /// This is what the membership pattern requires — "the rows belonging to any
    /// club/team/workspace this user has joined" is a query, not a field on the
    /// [`Identity`], so a sync hook cannot express it. Pair it with
    /// [`ScopeDecision::RestrictIn`]:
    ///
    /// ```ignore
    /// use umbral_rest::{ResourceConfig, ScopeDecision};
    ///
    /// ResourceConfig::new("fixture").scope_async(|identity| async move {
    ///     let Some(id) = identity else { return ScopeDecision::None };  // anonymous: nothing
    ///     if id.is_superuser { return ScopeDecision::All; }
    ///
    ///     // Which clubs has this user joined?
    ///     let clubs: Vec<Membership> = Membership::objects()
    ///         .filter(membership::USER.eq(&id.user_id))
    ///         .fetch()
    ///         .await
    ///         .unwrap_or_default();
    ///
    ///     // Rows in ANY of them. An empty list means no rows — never all rows.
    ///     ScopeDecision::RestrictIn(
    ///         "club_id".into(),
    ///         clubs.iter().map(|m| m.club.to_string()).collect(),
    ///     )
    /// });
    /// ```
    ///
    /// The hook runs once per request, before the query it constrains. Keep it
    /// cheap — it is on the read path of every list and detail call — and prefer
    /// failing closed (`ScopeDecision::None`) if the lookup errors, rather than
    /// falling back to [`ScopeDecision::All`].
    pub fn scope_async<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Option<Identity>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ScopeDecision> + Send + 'static,
    {
        self.scope = Some(Arc::new(move |identity| Box::pin(f(identity))));
        self
    }

    /// The common owner-scope shorthand for [`Self::scope`]: restrict every
    /// CRUD action to rows whose `owner_column` equals the caller's user id,
    /// and deny anonymous callers entirely. A superuser sees all rows.
    ///
    /// ```ignore
    /// ResourceConfig::new("order").owned_by("owner_id")
    /// ```
    /// Override `Cache-Control` for this resource only (gaps3 #36).
    ///
    /// The framework defaults every REST response to `no-store` because a
    /// mutable API served stale is a data-loss bug. A genuinely cacheable read
    /// endpoint — a public, slow-changing list — can opt back in here.
    ///
    /// ```ignore
    /// ResourceConfig::new("country").cache_control("public, max-age=3600")
    /// ```
    pub fn cache_control(mut self, value: impl Into<String>) -> Self {
        self.cache_control = Some(value.into());
        self
    }

    pub fn owned_by(self, owner_column: impl Into<String>) -> Self {
        let col = owner_column.into();
        self.scope(move |identity| match identity {
            Some(id) if id.is_superuser => ScopeDecision::All,
            Some(id) => ScopeDecision::Restrict(vec![(col.clone(), id.user_id.clone())]),
            None => ScopeDecision::None,
        })
    }

    /// Fill `owner_column` from the authenticated identity when a row is
    /// CREATED, and reject a body-supplied value — so a client can't create a
    /// row owned by someone else (the DRF `perform_create(owner=request.user)`
    /// pattern). Anonymous creates are rejected (401): there's no identity to
    /// inject. Pairs naturally with [`Self::owned_by`] (inject on write, scope
    /// on read):
    ///
    /// ```ignore
    /// ResourceConfig::new("order").owner_field("owner_id").owned_by("owner_id")
    /// ```
    ///
    /// The injected value is the identity's `user_id`; if it parses as an
    /// integer it's written as a number (an `i64` FK), otherwise as the string
    /// (a `String`/UUID key).
    pub fn owner_field(mut self, owner_column: impl Into<String>) -> Self {
        self.owner_field = Some(owner_column.into());
        self
    }

    /// Mount this resource UNDER a parent, scoped to it (gaps3 #29 item 2).
    ///
    /// ```ignore
    /// ResourceConfig::new("selection").under("fixture", "fixture_id")
    /// ```
    ///
    /// gives you `/api/fixture/{fixture_id}/selection[/{id}]`, and with it, for free,
    /// the four things every hand-written nested handler writes out longhand:
    ///
    /// - **404 if the parent row does not exist.** Not "empty list" — a child
    ///   collection under a fixture that was never created is a wrong URL, and saying
    ///   `200 []` tells the client it asked a valid question.
    /// - **List, retrieve, update and delete are FILTERED to the parent.** The scope is
    ///   ANDed into the same query the row-level `scope`/`owned_by` hook feeds, so it
    ///   composes with them instead of racing them.
    /// - **Create INJECTS the parent id** from the URL, overriding whatever the body
    ///   claimed. The URL is the authority; a body that disagrees with it is at best
    ///   confused and at worst an attempt to plant a row under someone else's parent.
    /// - **The flat route stops existing.** `/api/selection/{id}` returns 404 once the
    ///   resource declares a parent. A nested resource that is *also* reachable flat is
    ///   not scoped — it just has a scoped-looking URL, which is worse than no scoping,
    ///   because you would trust it.
    ///
    /// `fk_column` is the child's column pointing at the parent — the same column you
    /// would filter on by hand.
    pub fn under(mut self, parent_table: impl Into<String>, fk_column: impl Into<String>) -> Self {
        self.under = Some((parent_table.into(), fk_column.into()));
        self
    }

    /// Opt IN to bulk endpoints for this resource.
    ///
    /// Off by default. Without this call the resource behaves exactly as
    /// before: a `POST` whose body is a JSON array is rejected, and no
    /// collection-level `PATCH` / `DELETE` route exists.
    ///
    /// With it, three transactional (all-or-nothing) endpoints turn on:
    ///
    /// - **Bulk create** — `POST {prefix}/<table>/` with a JSON **array**
    ///   creates every item in ONE transaction → `201` + the created rows.
    ///   A single JSON **object** still does the ordinary single create.
    /// - **Bulk update** — `PATCH {prefix}/<table>/` with a JSON array
    ///   where each item carries its primary key partial-updates each in
    ///   ONE transaction → `200` + the updated rows.
    /// - **Bulk delete** — `DELETE {prefix}/<table>/` with
    ///   `{ "ids": [ ... ] }` deletes (or soft-deletes) all matching rows
    ///   in ONE transaction → `204`.
    ///
    /// Every bulk item runs the SAME validation, field denylist
    /// (`password_hash` / hidden / `noform`), permission class
    /// (`Add` / `Change` / `Delete`), throttle, and blocked-table check as
    /// the single-object handler — bulk opens no bypass. A batch is capped
    /// at the list ceiling (1000 items); an oversize batch is a `400`.
    ///
    /// ```ignore
    /// RestPlugin::default()
    ///     .resource(ResourceConfig::for_::<Post>().bulk())
    /// ```
    pub fn bulk(mut self) -> Self {
        self.bulk = true;
        self
    }

    /// Declare a writable nested resource. A `POST` to this resource whose
    /// body carries `"<json_field>": [ {child}, ... ]` creates the parent,
    /// then inserts each child with its foreign key to the parent set
    /// automatically (the FK column is discovered from the child model —
    /// the column that references this resource's table). The response
    /// echoes the created children back under `json_field`.
    ///
    /// If any child fails validation the whole write is undone (the parent
    /// and any already-created siblings are deleted), so you never get a
    /// half-created parent.
    ///
    /// ```ignore
    /// ResourceConfig::for_::<Order>().nested("items", "order_item")
    /// // POST /api/order/ { "customer": 1, "items": [{ "product": 7, "qty": 2 }] }
    /// ```
    pub fn nested(mut self, json_field: impl Into<String>, child_table: impl Into<String>) -> Self {
        self.nested.push((json_field.into(), child_table.into()));
        self
    }

    /// Start a new resource config keyed off a model's
    /// [`Model::TABLE`](umbral::orm::Model) const instead of a literal
    /// table name. Matches the `ModelMeta::for_` convention, and turns
    /// a misspelled table into a compile error.
    ///
    /// ```ignore
    /// ResourceConfig::for_::<AuthUser>().hide(["password_hash", "email"])
    /// ```
    pub fn for_<M: umbral::orm::Model>() -> Self {
        Self::new(M::TABLE)
    }

    /// Opt OUT of query-string filtering on the
    /// list endpoint for this resource.
    ///
    /// Filtering is ON by default. Query-string keys of the form
    /// `<field>` or `<field>__<lookup>` are parsed into SQL WHERE
    /// predicates and ANDed together before pagination is applied.
    /// Unrecognised field names, inapplicable lookups, and malformed
    /// values all return HTTP 400 with a descriptive JSON error.
    ///
    /// Supported lookups: `eq` (default), `ne`, `gte`, `lte`, `gt`,
    /// `lt`, `in` (comma-separated), `contains`, `icontains`,
    /// `startswith`, `isnull`.
    ///
    /// Call this on tables where filtering doesn't make sense (audit
    /// logs, append-only streams, dashboards meant to surface every
    /// row):
    ///
    /// ```ignore
    /// RestPlugin::default()
    ///     .resource(ResourceConfig::new("audit_log").disable_filters())
    /// ```
    pub fn disable_filters(mut self) -> Self {
        self.filters_disabled = true;
        self
    }

    /// Opt OUT of `?search=<term>` free-text search on this resource.
    ///
    /// Search is ON by default. A `?search=foo` query string ORs an
    /// `icontains` predicate across every Text column with `eq`
    /// predicates against numeric / FK / Boolean columns when the
    /// term parses as those types.
    /// Call this on resources where free-text matching makes no
    /// sense (event streams, metric samples, opaque payloads).
    pub fn disable_search(mut self) -> Self {
        self.search_disabled = true;
        self
    }

    /// Restrict `?search=<term>` to a specific subset of columns.
    ///
    /// By default `parse_search` walks every searchable column on
    /// the model. When you only want a subset to participate — say,
    /// title + body on a post but never the internal `slug` — pass
    /// the allow-list here:
    ///
    /// ```ignore
    /// RestPlugin::default().resource(
    ///     ResourceConfig::new("post").search_fields(["title", "body"])
    /// )
    /// ```
    ///
    /// Calling `search_fields` does NOT enable search by itself —
    /// search is already on. Composes with `disable_search()`
    /// (last-call wins: if you disable then restrict, the restrict
    /// is ignored because search is off).
    pub fn search_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.search_fields = Some(fields.into_iter().map(Into::into).collect());
        self
    }

    /// Attach a permission class to this resource. Every request to
    /// any action on this table will be authorised through the
    /// permission's `check(action, identity)` before the actual
    /// handler runs.
    ///
    /// Override examples:
    ///
    /// ```ignore
    /// // Only authenticated callers can do anything.
    /// ResourceConfig::new("post").permission(IsAuthenticated)
    ///
    /// // Public-read, staff-write (and only staff CRUD).
    /// ResourceConfig::new("post").permission(OrPermission::new(vec![
    ///     Box::new(ReadOnly),
    ///     Box::new(IsStaff),
    /// ]))
    /// ```
    pub fn permission<P: Permission>(mut self, perm: P) -> Self {
        self.permission = Some(Arc::new(perm));
        self
    }

    /// Attach a throttle to this resource. Run after auth and the
    /// permission check, before the handler; on a denial the request
    /// returns **429 Too Many Requests** with a `Retry-After` header.
    ///
    /// Throttles **stack**: call this more than once, and combine with the
    /// plugin-wide [`RestPlugin::default_throttle`](crate::RestPlugin::
    /// default_throttle) — every throttle that applies to the request must
    /// pass. Throttling is opt-in; a resource with none imposes no limit.
    ///
    /// ```ignore
    /// // Cap anonymous reads on this table at 100/hour, plus a tight
    /// // 10/min on the "uploads" scope.
    /// ResourceConfig::new("upload")
    ///     .throttle(AnonRateThrottle::new("100/hour"))
    ///     .throttle(ScopedRateThrottle::new("10/min", "upload:create"))
    /// ```
    pub fn throttle<T: Throttle>(mut self, throttle: T) -> Self {
        self.throttles.push(Arc::new(throttle));
        self
    }

    /// Restrict this resource to a specific set of REST actions —
    /// the opt-in alternative to having every model expose all five
    /// (`List` / `Retrieve` / `Create` / `Update` / `Delete`). Any
    /// action not in the set returns 404 from the handler.
    ///
    /// Default (no call) is "every action exposed" so existing
    /// resources don't change shape on upgrade.
    ///
    /// ```ignore
    /// // Read-only public catalogue: no create/update/delete endpoints
    /// // even mount.
    /// ResourceConfig::new("product").views([Action::List, Action::Retrieve])
    /// ```
    pub fn views<I: IntoIterator<Item = Action>>(mut self, actions: I) -> Self {
        self.view_scope = Some(actions.into_iter().collect());
        self
    }

    /// The table this config is for. Used by [`crate::RestPlugin::
    /// resource`] when folding into the plugin's per-table vecs.
    pub fn table(&self) -> &str {
        &self.table
    }

    /// Strip one or more fields from every REST response for this
    /// table. Equivalent to [`crate::RestPlugin::hide`] but with the
    /// table implicit. The columns stay writable and ORM-readable;
    /// only the outbound JSON shape changes.
    ///
    /// `fields` accepts a single name or many via [`HideFields`]:
    ///
    /// ```ignore
    /// ResourceConfig::new("user")
    ///     .hide("password_hash")               // single
    ///     .hide(["password_hash", "ssn"])      // many
    /// ```
    pub fn hide(mut self, fields: impl HideFields) -> Self {
        self.hidden.extend(fields.into_field_list());
        self
    }

    /// Let approved callers see a `#[umbral(private)]` column on this resource.
    ///
    /// ```rust,ignore
    /// ResourceConfig::new("product")
    ///     .allow_private_if("cost", |id| id.is_some_and(|i| i.is_staff))
    /// ```
    ///
    /// A `private` column is stripped from every response by default — it is not even
    /// SELECTed, so the value never leaves the database. This is the unlock: for a caller
    /// your closure approves, the column is fetched and returned; for everyone else nothing
    /// changes.
    ///
    /// **Reads and writes both.** The unlock also governs whether the column may be SET
    /// through this endpoint, because a field only trusted callers may READ is not one an
    /// anonymous `POST` should be able to write. (Write authority for ordinary columns is
    /// `#[umbral(privileged)]`'s job; this is the private tier's own gate.)
    ///
    /// Cannot unlock a `#[umbral(secret)]` column — that tier has no unlock, and naming one
    /// here does nothing.
    ///
    /// # What this does to your OpenAPI spec
    ///
    /// One path cannot describe two response shapes, so a conditionally-visible column is
    /// emitted as **optional** (`cost?: string`) with a description saying who gets it. That
    /// is the honest answer, and it is correct for both audiences: the field genuinely may or
    /// may not be present. A generated TypeScript client will make you check, which is
    /// exactly right.
    pub fn allow_private_if<F>(mut self, field: &str, f: F) -> Self
    where
        F: Fn(Option<&umbral::auth::Identity>) -> bool + Send + Sync + 'static,
    {
        self.private_unlocks.push((field.to_string(), Arc::new(f)));
        self
    }

    /// Replace a field's value in every REST response for this table.
    /// Equivalent to [`crate::RestPlugin::transform`] with the table
    /// implicit.
    pub fn transform<F>(mut self, field: &str, f: F) -> Self
    where
        F: Fn(&Value) -> Value + Send + Sync + 'static,
    {
        self.transforms
            .push((field.to_string(), std::sync::Arc::new(f)));
        self
    }

    /// Add a derived field to every REST response for this table.
    /// Equivalent to [`crate::RestPlugin::computed`] with the table
    /// implicit.
    pub fn computed<F>(mut self, name: &str, f: F) -> Self
    where
        F: Fn(&Map<String, Value>) -> Value + Send + Sync + 'static,
    {
        self.computed
            .push((name.to_string(), std::sync::Arc::new(f)));
        self
    }

    /// Register a custom-action endpoint
    /// (`/api/<table>/<id>/<name>/`) for behaviour that doesn't fit CRUD.
    ///
    /// The umbral-rest shape is a builder call on the resource:
    ///
    /// ```ignore
    /// use http::Method;
    /// use umbral_rest::{ActionScope, ResourceConfig};
    /// use serde_json::json;
    ///
    /// ResourceConfig::new("post")
    ///     // POST /api/post/{id}/publish/ — one row at a time.
    ///     .action("publish", Method::POST, ActionScope::Detail, |ctx| async move {
    ///         let id: i64 = ctx.pk.as_deref().unwrap_or_default().parse()
    ///             .map_err(|_| umbral_rest::ActionError::BadInput("bad id".into()))?;
    ///         // hit the ORM, update state, return a JSON response
    ///         Ok(json!({ "id": id, "published": true }))
    ///     })
    ///     // GET /api/post/recent/ — collection-scope endpoint.
    ///     .action("recent", Method::GET, ActionScope::Collection, |_ctx| async move {
    ///         Ok(json!({ "results": [] }))
    ///     });
    /// ```
    ///
    /// The handler runs AFTER the resource's `Permission::check` has
    /// approved the call with `Action::Custom(name)`. Inside the
    /// handler you have full async access to the ORM, sqlx, and
    /// whatever else you need.
    ///
    /// **URL shapes:**
    /// - `ActionScope::Collection` → `/api/<table>/<name>/`
    /// - `ActionScope::Detail`     → `/api/<table>/<id>/<name>/`
    ///
    /// Both with and without trailing slash are accepted.
    ///
    /// **Action names** must be URL-safe ASCII (`a-z`, `0-9`, `-`,
    /// `_`); the builder panics at registration time on anything else
    /// (validation is cheap and the wrong name is always a bug).
    pub fn action<F, Fut>(mut self, name: &str, method: Method, scope: ActionScope, f: F) -> Self
    where
        F: Fn(ActionContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, ActionError>> + Send + 'static,
    {
        assert!(
            !name.is_empty() && name.chars().all(is_action_name_char),
            "ResourceConfig::action: name {name:?} must be ASCII [a-z0-9_-]"
        );
        let handler: ActionHandler = Arc::new(move |ctx| Box::pin(f(ctx)));
        self.actions.push(ActionDef {
            name: name.to_string(),
            method,
            scope,
            handler,
            input_schema: None,
            output_schema: None,
        });
        self
    }

    /// Attach a JSON Schema for a custom action's **request body**. The
    /// dispatch validates the body against it before the handler runs (the
    /// common `type` / `required` / `properties` / `enum` subset; a failure
    /// is a `400` with the field errors), and the schema is published into
    /// the OpenAPI spec so the playground knows the expected shape. Applies
    /// to the most recently declared action with that `name`.
    ///
    /// ```ignore
    /// .action("ship", Method::POST, ActionScope::Detail, ship_handler)
    /// .action_input_schema("ship", json!({
    ///     "type": "object",
    ///     "required": ["carrier"],
    ///     "properties": { "carrier": { "type": "string" }, "express": { "type": "boolean" } }
    /// }))
    /// ```
    pub fn action_input_schema(mut self, action: &str, schema: Value) -> Self {
        if let Some(def) = self.actions.iter_mut().rev().find(|d| d.name == action) {
            def.input_schema = Some(schema);
        }
        self
    }

    /// Attach a JSON Schema for a custom action's **200 response**.
    /// Published into the OpenAPI spec (documentation only — not validated
    /// at runtime). Applies to the most recently declared action with that
    /// `name`.
    pub fn action_output_schema(mut self, action: &str, schema: Value) -> Self {
        if let Some(def) = self.actions.iter_mut().rev().find(|d| d.name == action) {
            def.output_schema = Some(schema);
        }
        self
    }
}

fn is_action_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}
