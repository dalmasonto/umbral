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
//! pub fn rest_resource() -> umbra_rest::ResourceConfig {
//!     umbra_rest::ResourceConfig::new("user")
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
//! The DRF analog: each Django app has a `serializers.py` next to its
//! `models.py`. `ResourceConfig` plays the same role — the per-model
//! REST shape lives next to the model, not in the project's wiring
//! layer.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use http::Method;
use serde_json::{Map, Value};

use crate::auth::Identity;
use crate::permission::{Action, Permission};
use crate::{ComputedFn, TransformFn};

/// Whether a custom action is mounted on the collection
/// (`/api/<table>/<name>/`) or on a single row
/// (`/api/<table>/<id>/<name>/`). DRF's `detail=False` / `detail=True`
/// flag.
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
    /// Opt-in view scope. `None` means "all actions exposed" — the
    /// backward-compatible default. `Some(set)` restricts the
    /// resource to exactly that set; everything else 404s.
    pub(crate) view_scope: Option<HashSet<Action>>,
    /// DRF-style `@action` endpoints registered on this resource.
    /// Merged into the plugin's per-table action map at `.resource()`
    /// time; mounted as new axum routes at `RestPlugin::routes()`.
    pub(crate) actions: Vec<ActionDef>,
    /// Opt OUT of django-filter-style query-string filtering on the
    /// list endpoint for this resource. Filters are ON by default —
    /// every column gets the standard lookup grammar (`__eq`, `__in`,
    /// `__contains`, etc.) — and `.disable_filters()` removes them
    /// for tables where filtering is undesirable (audit logs,
    /// append-only event streams, etc.).
    pub(crate) filters_disabled: bool,
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
            view_scope: None,
            actions: Vec::new(),
            filters_disabled: false,
        }
    }

    /// Opt OUT of django-filter-style query-string filtering on the
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

    /// Strip a field from every REST response for this table.
    /// Equivalent to [`crate::RestPlugin::hide`] but with the table
    /// implicit. The column stays writable and ORM-readable; only
    /// the outbound JSON shape changes.
    pub fn hide(mut self, field: &str) -> Self {
        self.hidden.push(field.to_string());
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

    /// Register a DRF-style `@action` endpoint.
    ///
    /// In Django REST Framework you write:
    ///
    /// ```python
    /// class PostViewSet(ViewSet):
    ///     @action(detail=True, methods=['post'])
    ///     def publish(self, request, pk=None):
    ///         ...
    /// ```
    ///
    /// The umbra-rest shape is a builder call on the resource:
    ///
    /// ```ignore
    /// use http::Method;
    /// use umbra_rest::{ActionScope, ResourceConfig};
    /// use serde_json::json;
    ///
    /// ResourceConfig::new("post")
    ///     // POST /api/post/{id}/publish/ — one row at a time.
    ///     .action("publish", Method::POST, ActionScope::Detail, |ctx| async move {
    ///         let id: i64 = ctx.pk.as_deref().unwrap_or_default().parse()
    ///             .map_err(|_| umbra_rest::ActionError::BadInput("bad id".into()))?;
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
        });
        self
    }
}

fn is_action_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}
