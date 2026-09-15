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
    /// **`.action_by(...)` only** (gaps4 #80): the full row the lookup
    /// column resolved to, as a JSON object — `None` for every ordinary
    /// `.action(...)`. Saves the handler a round-trip: the dispatch already
    /// fetched the row to confirm it exists (and to read `pk` off it), so it
    /// hands the row over rather than making the handler re-query for data
    /// it's already holding.
    pub resolved_row: Option<Value>,
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
    /// **gaps4 #80.** `Some(column)` marks this as a `.action_by(...)`
    /// registration: a detail action keyed by a `#[umbral(unique)]` (or
    /// primary-key) column instead of the PK, mounted at
    /// `/api/<table>/<value>/` with no `/<name>/` suffix. `None` for a
    /// plain `.action(...)`, which keeps the `/<name>/`-suffixed shape.
    /// `name` doubles as the column name for a by-field action (used to
    /// key `.action_input_schema`/`.action_output_schema` and to label the
    /// OpenAPI operation), since there's no separate action name in the URL.
    pub(crate) lookup_field: Option<String>,
}

impl std::fmt::Debug for ActionDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionDef")
            .field("name", &self.name)
            .field("method", &self.method)
            .field("scope", &self.scope)
            .field("lookup_field", &self.lookup_field)
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
    /// Restrict to rows reachable from `value` through a forward FK/O2O
    /// relation **path** — the multi-hop sibling of [`Self::Restrict`], which
    /// only covers a column on the resource's OWN table. `path` is a
    /// Django-style `__`-joined chain, e.g. `"developer__user"` for "this
    /// row's `developer`'s `user` equals the caller" (gaps4 #78). Built by
    /// [`ResourceConfig::owned_via`] — resolved via
    /// [`umbral::orm::build_dynamic_relation`], the same registry-driven
    /// engine gaps4 #76 added for the filter (`?`-query) side, so no
    /// hand-written `scope_async` resolver is needed for an ownership chain
    /// that is more than one hop away.
    RestrictVia {
        /// The `__`-joined relation path from this resource to the owner
        /// column, e.g. `"developer__user"`.
        path: String,
        /// The caller id the leaf column must equal.
        value: String,
    },
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
    /// Hidden columns this resource can REVEAL for an authorized caller, and
    /// for whom. Stronger than `private_unlocks`: covers `secret` / `Masked`
    /// columns too, and DECRYPTS `Masked<T>` to plaintext in the response.
    pub(crate) reveal_unlocks: Vec<(String, PrivateFn)>,
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
    /// Read-side reverse-FK relations embeddable via `?expand=<field>`
    /// (gaps4 #72): `(field, child_table)`. A `GET` naming `field` in
    /// `?expand=` gets an ARRAY of full child objects — every row in
    /// `child_table` whose foreign key points back at this row — spliced
    /// in under `field`. Declared via [`ResourceConfig::embed`]. Distinct
    /// from `nested` (write-side, POST) so the two can be declared
    /// independently; a resource that wants both calls both builders.
    pub(crate) expand_reverse: Vec<(String, String)>,
    /// Read-side M2M fields expandable via `?expand=<field>` (gaps4 #72).
    /// Without this the M2M field always serializes as a bare `[id, ...]`
    /// array (the existing behavior). Naming the field in `?expand=`
    /// replaces the id array with the full child objects, batched (one
    /// query per relation regardless of row count). Declared via
    /// [`ResourceConfig::expand_m2m`].
    pub(crate) expand_m2m: Vec<String>,
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
    /// gaps4 #79: a WRITE-only object scope. `None` means writes fall back to
    /// [`Self::scope`] (today's behavior, unchanged); `Some(fn)` scopes ONLY
    /// `create`/`update`/`delete` (and bulk variants) to the rows it returns,
    /// while `list`/`retrieve` stay governed by `scope` alone (unconstrained
    /// if `scope` is unset). This is what makes "public read, owner-only
    /// write" expressible without touching `.scope`/`.owned_by`, which keep
    /// scoping every action for anyone already relying on that. Declared via
    /// [`Self::scope_writes`] / [`Self::scope_writes_async`] /
    /// [`Self::owned_by_for_writes`].
    pub(crate) write_scope: Option<ObjectScopeFn>,
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
    /// Acknowledgement marker for the `security.object_scope` boot check
    /// (IDOR design spec). `Some(reason)` declares that this write-enabled
    /// resource is intentionally left without an object scope — because row
    /// security is enforced elsewhere (a Postgres RLS policy the REST plugin
    /// can't see across the crate boundary) or because the rows are genuinely
    /// public. Set via [`Self::unscoped_ok`] / [`Self::rls_backed`]. The reason
    /// is a declared, greppable record of the decision; it silences the warning
    /// for exactly this resource without disabling the check.
    pub(crate) unscoped_ok: Option<String>,
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
            reveal_unlocks: Vec::new(),
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
            expand_reverse: Vec::new(),
            expand_m2m: Vec::new(),
            bulk: false,
            scope: None,
            write_scope: None,
            cache_control: None,
            owner_field: None,
            under: None,
            unscoped_ok: None,
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

    /// Acknowledge that this write-enabled resource is intentionally NOT
    /// object-scoped, silencing the `security.object_scope` boot check for it
    /// (IDOR design spec). `reason` is a short, greppable note explaining why
    /// leaving every row reachable is safe here.
    ///
    /// The check exists because a write endpoint with no `scope` / `owned_by`
    /// hook lets any authorized caller mutate any row by id — an IDOR hole. Two
    /// cases are legitimately unscoped at the app layer: rows secured one layer
    /// down by a Postgres RLS policy (use [`Self::rls_backed`]), and genuinely
    /// public data. This marker declares the decision instead of leaving it to
    /// omission; it does not disable the check for other resources.
    ///
    /// ```ignore
    /// ResourceConfig::new("changelog").unscoped_ok("public, append-only feed")
    /// ```
    pub fn unscoped_ok(mut self, reason: impl Into<String>) -> Self {
        self.unscoped_ok = Some(reason.into());
        self
    }

    /// Sugar for [`Self::unscoped_ok`] declaring that row security for this
    /// table is enforced by a Postgres RLS policy (IDOR design spec).
    ///
    /// `umbral-rest` cannot depend on `umbral-rls` (the crate-dependency ban),
    /// so the REST boot check cannot read RLS policies to confirm coverage — it
    /// asks you to declare it. Pair this with an actual `umbral-rls` policy on
    /// the table; the marker only silences the app-layer warning.
    ///
    /// ```ignore
    /// ResourceConfig::new("invoice").rls_backed()
    /// ```
    pub fn rls_backed(self) -> Self {
        self.unscoped_ok("row security enforced by RLS")
    }

    /// gaps4 #79 — an `IsOwnerOrReadOnly`-shaped object scope: `list`/`retrieve`
    /// stay UNCONSTRAINED (every row is publicly readable), while
    /// `create`/`update`/`delete` (and their bulk equivalents) are restricted to
    /// rows where `owner_column` equals the caller's user id. A superuser may
    /// write any row.
    ///
    /// This is the opt-in twin of [`Self::owned_by`], which scopes EVERY action
    /// (reads included) — existing `.owned_by(...)` resources are unaffected by
    /// this method existing; picking one is a per-resource choice, and calling
    /// both is legal (`scope` still governs reads, `write_scope` governs writes,
    /// and they can name different columns).
    ///
    /// ```ignore
    /// // Public profile, private editing: anyone can GET, only the owner can
    /// // PATCH/DELETE.
    /// ResourceConfig::new("post").owned_by_for_writes("author_id")
    /// ```
    pub fn owned_by_for_writes(self, owner_column: impl Into<String>) -> Self {
        let col = owner_column.into();
        self.scope_writes(move |identity| match identity {
            Some(id) if id.is_superuser => ScopeDecision::All,
            Some(id) => ScopeDecision::Restrict(vec![(col.clone(), id.user_id.clone())]),
            None => ScopeDecision::None,
        })
    }

    /// The relation-path owner-scope shorthand for [`Self::scope`] (gaps4
    /// #78): restrict every CRUD/bulk action to rows reachable from the
    /// caller through a forward FK/O2O chain, when ownership is not a column
    /// on THIS table but on a table one or more hops away.
    ///
    /// `relation` is the `__`-joined chain of forward relation fields from
    /// this resource to the table that carries `owner_column` — a single
    /// field for one hop (`"developer"`), or `"a__b"` for two. The two are
    /// joined into the same Django-style path
    /// [`umbral::orm::build_dynamic_relation`] resolves for the read side's
    /// `Predicate::related` (gaps4 #76):
    ///
    /// ```ignore
    /// // "this gig's developer's user must be me" — TWO hops, no
    /// // hand-written scope_async resolver:
    /// ResourceConfig::new("gig").owned_via("developer", "user")
    /// // equivalent path: "developer__user"
    /// ```
    ///
    /// Same fail-closed contract as [`Self::owned_by`]: a superuser sees
    /// every row ([`ScopeDecision::All`]); everyone else is restricted to
    /// rows whose `relation`-path leaf equals their id
    /// ([`ScopeDecision::RestrictVia`]); an anonymous caller sees none
    /// ([`ScopeDecision::None`]). A create is checked against the SAME chain
    /// (does the developer named in the body actually belong to the caller?)
    /// via one `EXISTS` query — see the write-side handling in
    /// `RestPlugin::object_scope_allows_create`.
    ///
    /// **Scope**: forward FK/O2O hops only, to arbitrary depth — the same
    /// surface [`umbral::orm::Predicate::related`] supports. A reverse-FK or
    /// M2M hop in the chain (e.g. "the rows any of my teams' members can
    /// see") is a documented follow-up, not covered here.
    pub fn owned_via(self, relation: impl Into<String>, owner_column: impl Into<String>) -> Self {
        let path = format!("{}__{}", relation.into(), owner_column.into());
        self.scope(move |identity| match identity {
            Some(id) if id.is_superuser => ScopeDecision::All,
            Some(id) => ScopeDecision::RestrictVia {
                path: path.clone(),
                value: id.user_id.clone(),
            },
            None => ScopeDecision::None,
        })
    }

    /// [`Self::owned_by_for_writes`]'s general form: a caller-supplied
    /// [`ScopeDecision`] hook that applies ONLY to
    /// `create`/`update`/`delete` (+ bulk). `list`/`retrieve` are governed by
    /// [`Self::scope`]/[`Self::owned_by`] alone — unconstrained if neither is
    /// set on this resource.
    ///
    /// ```ignore
    /// use umbral_rest::{ResourceConfig, ScopeDecision};
    /// ResourceConfig::new("post").scope_writes(|identity| match identity {
    ///     Some(id) if id.is_staff => ScopeDecision::All,
    ///     Some(id) => ScopeDecision::Restrict(vec![("author_id".into(), id.user_id.clone())]),
    ///     None => ScopeDecision::None,
    /// });
    /// ```
    pub fn scope_writes<F>(self, f: F) -> Self
    where
        F: Fn(Option<&Identity>) -> ScopeDecision + Send + Sync + 'static,
    {
        self.scope_writes_async(move |identity| {
            let decision = f(identity.as_ref());
            std::future::ready(decision)
        })
    }

    /// [`Self::scope_writes`] for a decision that needs a database round-trip —
    /// the write-only twin of [`Self::scope_async`].
    pub fn scope_writes_async<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Option<Identity>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ScopeDecision> + Send + 'static,
    {
        self.write_scope = Some(Arc::new(move |identity| Box::pin(f(identity))));
        self
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

    /// Declare a REVERSE-FK relation embeddable on `GET` via `?expand=<field>`
    /// (gaps4 #72) — the read-side counterpart to [`Self::nested`]'s write
    /// side. `child_table` is a table with a foreign key pointing AT this
    /// resource; `field` is the name under which the full child ARRAY
    /// appears in the response when the caller asks for it.
    ///
    /// ```ignore
    /// ResourceConfig::for_::<Developer>()
    ///     .embed("projects", "project")       // reverse-FK: project.developer_id -> developer.id
    ///     .expand_m2m("favorite_software")     // M2M: expand ids to full objects
    /// // GET /api/developer/7?expand=projects,favorite_software
    /// ```
    ///
    /// Without `?expand=projects` the response is unchanged — no `projects`
    /// key at all, exactly like before this method existed. Naming an
    /// UNDECLARED relation in `?expand=` is a `400`, not a silent no-op —
    /// same contract as `?include=` for forward FKs.
    ///
    /// Batched: a LIST request expanding `field` issues ONE
    /// `SELECT ... WHERE <fk> IN (...)` across every row on the page, not
    /// one query per row. Embedded children are scrubbed by their own
    /// table's `.hide()` / hidden-column rules before they reach the
    /// response — the same recursion `?include=` already applies to
    /// forward-FK objects.
    ///
    /// **One level.** `?expand=projects` embeds `project` rows as-is; it
    /// does not also expand a relation declared on `project` itself
    /// (`?expand=projects.tasks` is not supported in this version — a
    /// documented follow-up, not silently ignored: it 400s like any other
    /// unknown name).
    pub fn embed(mut self, field: impl Into<String>, child_table: impl Into<String>) -> Self {
        self.expand_reverse.push((field.into(), child_table.into()));
        self
    }

    /// Declare an M2M field expandable on `GET` via `?expand=<field>`
    /// (gaps4 #72). By default every M2M field always serializes as a bare
    /// `[id, id, ...]` array; naming it here lets a caller ask for the full
    /// child objects instead via `?expand=<field>` — the id array survives
    /// unchanged when the caller doesn't ask.
    ///
    /// ```ignore
    /// ResourceConfig::for_::<Developer>().expand_m2m("favorite_software")
    /// // GET /api/developer/7                     -> favorite_software: [41, 109]
    /// // GET /api/developer/7?expand=favorite_software -> favorite_software: [{...}, {...}]
    /// ```
    ///
    /// `field` must be a real M2M relation declared on the model
    /// (`Model::M2M_RELATIONS`) — the target table is discovered from
    /// there, not passed here. Batched the same way as [`Self::embed`]: one
    /// query per relation across a whole list page.
    pub fn expand_m2m(mut self, field: impl Into<String>) -> Self {
        self.expand_m2m.push(field.into());
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

    /// REVEAL a hidden column (`private`, `secret`, or `Masked<T>`) for a
    /// caller the predicate authorizes — the read-side twin of the ORM's
    /// `DynQuerySet::reveal`. Unlike [`allow_private_if`](Self::allow_private_if)
    /// (which only unlocks `private`), this also surfaces `secret` columns
    /// and DECRYPTS `Masked<T>` to plaintext in the response.
    ///
    /// ```rust,ignore
    /// // Staff (and only staff) see the decrypted API key on this resource:
    /// ResourceConfig::for_::<Account>()
    ///     .reveal_if("api_key", |id| id.is_some_and(|i| i.is_staff()))
    /// ```
    ///
    /// The value only leaves the server when the predicate returns true, so
    /// pair it with a `.permission(...)` that gates the endpoint itself. Call
    /// once per column you want revealed.
    pub fn reveal_if<F>(mut self, field: &str, f: F) -> Self
    where
        F: Fn(Option<&umbral::auth::Identity>) -> bool + Send + Sync + 'static,
    {
        self.reveal_unlocks.push((field.to_string(), Arc::new(f)));
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
            lookup_field: None,
        });
        self
    }

    /// Register a by-natural-key detail action (gaps4 #80): a `.action()`
    /// keyed by a real column instead of the primary key, mounted at
    /// `/api/<table>/<value>/` with **no** trailing `/<name>/` segment —
    /// the shape `.action()` can't express (its detail form is always
    /// `/api/<table>/<id>/<name>/`, keyed by the PK).
    ///
    /// This is the fix for "give me this record at a clean, natural-key URL"
    /// — `/api/communities/<slug>`, `/api/developers/<username>` — as a
    /// first-class REST citizen: it reuses the `.action()` machinery end to
    /// end, so it shows up in the OpenAPI spec and the playground for free,
    /// runs under the resource's `Permission::check` with `Action::Custom
    /// (lookup_field)` (same gate a plain `.action()` gets), and accepts
    /// `.action_input_schema` / `.action_output_schema` keyed by
    /// `lookup_field` exactly like a named action.
    ///
    /// ```ignore
    /// use http::Method;
    /// use umbral_rest::ResourceConfig;
    /// use serde_json::json;
    ///
    /// ResourceConfig::new("community")
    ///     // GET /api/community/{slug}/ — read the row keyed by its slug.
    ///     .action_by("slug", Method::GET, |ctx| async move {
    ///         let row = ctx.resolved_row.expect("action_by always resolves a row");
    ///         Ok(json!({ "community": row }))
    ///     })
    /// ```
    ///
    /// The handler's [`ActionContext`] carries the resolved row
    /// (`ctx.resolved_row`, the whole row as JSON) and its primary key
    /// (`ctx.pk`) — the dispatch already looked the row up by `lookup_field`
    /// to confirm it exists, so the handler doesn't need to re-query.
    ///
    /// **Column requirement.** `lookup_field` must name a real column on
    /// this table that is `#[umbral(unique)]` or the primary key — a
    /// non-unique lookup could match more than one row, and there is no
    /// well-defined "first match" semantic for a public API endpoint.
    /// `RestPlugin::routes()` panics at boot with a clear message if the
    /// column is missing or not unique, the same "caught at boot, not in
    /// prod" posture the framework uses for backend/field mismatches.
    ///
    /// **Route collision with the PK-based detail route.** Only ONE literal
    /// route can live at `/api/<table>/<value>/`, so declaring `.action_by`
    /// on a table replaces that table's plain `/api/<table>/<id>/` mount
    /// with a combined one: **GET** first tries `lookup_field = <value>`,
    /// and — since the intended use is a human-facing key like a slug —
    /// falls back to the ordinary PK-based `retrieve` when no row matches
    /// the lookup column (so `GET /api/community/42` still works exactly
    /// as before when `42` isn't a valid slug). **PUT / PATCH / DELETE /
    /// OPTIONS at that URL are untouched** — they keep using the standard
    /// PK-based `update` / `destroy` / `options` handlers, unaffected by
    /// `.action_by`. The one sharp edge: a lookup value that is ALSO a
    /// valid PK for some other row (e.g. a purely numeric slug) resolves to
    /// the slug match first, never to that other row's plain retrieve — an
    /// acceptable, documented trade-off for the natural-key use case this
    /// exists for.
    ///
    /// **Only `Method::GET` is supported today** — `.action_by` is a
    /// read-oriented endpoint (the PK-lookup fallback above is meaningful
    /// only for GET); the builder panics on any other method. Extending
    /// to write verbs is a straightforward but unneeded generalization for
    /// now (no fallback rule to define — YAGNI until a real use case shows
    /// up).
    pub fn action_by<F, Fut>(mut self, lookup_field: &str, method: Method, f: F) -> Self
    where
        F: Fn(ActionContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, ActionError>> + Send + 'static,
    {
        assert!(
            !lookup_field.is_empty() && lookup_field.chars().all(is_action_name_char),
            "ResourceConfig::action_by: lookup_field {lookup_field:?} must be ASCII [a-z0-9_-]"
        );
        assert!(
            method == Method::GET,
            "ResourceConfig::action_by({lookup_field:?}, ...): only Method::GET is supported \
             today — a by-field lookup has a well-defined PK-lookup fallback only for reads"
        );
        let handler: ActionHandler = Arc::new(move |ctx| Box::pin(f(ctx)));
        self.actions.push(ActionDef {
            name: lookup_field.to_string(),
            method,
            scope: ActionScope::Detail,
            handler,
            input_schema: None,
            output_schema: None,
            lookup_field: Some(lookup_field.to_string()),
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
