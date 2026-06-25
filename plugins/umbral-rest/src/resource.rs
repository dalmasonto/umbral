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
    /// Detail-scope only: the primary-key value the client sent, as
    /// the raw URL segment. Parse with `.parse::<i64>()` (or whatever
    /// matches your PK type). `None` for collection-scope actions.
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

/// Bundled REST customization for one table. Build via
/// [`Self::new`] + chainable methods; register with
/// [`crate::RestPlugin::resource`].
///
/// Fields are public-ish via the constructor + builder methods
/// only — the closure-bearing vecs are kept private because the
/// `ComputedFn` / `TransformFn` types are internal implementation
/// detail.
pub struct ResourceConfig {
    pub(crate) table: String,
    pub(crate) hidden: Vec<String>,
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
        }
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
