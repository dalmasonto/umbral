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

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::{Row, SqlitePool};
use umbra::migrate::{Column, ModelMeta};
use umbra::orm::SqlType;
use umbra::prelude::*;
use umbra::web::{Json, Path, Query, Response, StatusCode};
use uuid::Uuid;

pub mod pagination;
pub use pagination::{
    LimitOffsetPagination, NoPagination, PageNumberPagination, PageRequest, Pagination,
};

pub mod resource;
pub use resource::ResourceConfig;

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
    fn view_exposed(&self, table: &str, action: Action) -> bool {
        match self.view_scope.get(table) {
            Some(scope) => scope.contains(&action),
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
        action: Action,
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
            hidden: Vec::new(),
            transforms: Vec::new(),
            computed: Vec::new(),
            pagination: Arc::new(NoPagination),
            authentication: Arc::new(NoAuthentication),
            permissions: HashMap::new(),
            view_scope: HashMap::new(),
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

impl Plugin for RestPlugin {
    fn name(&self) -> &'static str {
        "rest"
    }

    fn routes(&self) -> Router {
        // The OnceLock-captured config is what the static handlers
        // read. `routes()` is called exactly once per App::build, so
        // setting it here is safe.
        let _ = CONFIG.set(self.clone());

        Router::new()
            .route("/api/{table}/", get(list).post(create))
            .route("/api/{table}", get(list).post(create))
            .route(
                "/api/{table}/{id}",
                get(retrieve).put(update).patch(update).delete(destroy),
            )
    }
}

// =========================================================================
// Errors. Mapped to a JSON envelope so clients get a consistent shape.
// =========================================================================

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: String,
    code: &'static str,
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
        (status, Json(ApiErrorBody { error: msg, code })).into_response()
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

fn pk_column(model: &ModelMeta) -> Result<&Column, ApiError> {
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
    cfg.gate(&table, Action::List, identity.as_ref())?;
    let pool = umbra::db::pool();

    let page_req = cfg.pagination.extract_request(&params);
    let mut rows = fetch_rows(&pool, &model, None, Some(page_req)).await?;
    for row in &mut rows {
        cfg.apply_overrides(&table, row);
    }
    // Skip the extra COUNT round-trip for NoPagination — it would
    // throw away the result anyway. Other paginators read the total
    // for their envelope.
    let total = if cfg.pagination.needs_total() {
        count_rows(&pool, &model).await?
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
    cfg.gate(&table, Action::Retrieve, identity.as_ref())?;
    let pk = pk_column(&model)?;
    let pool = umbra::db::pool();
    let mut rows = fetch_rows(&pool, &model, Some((&pk.name, &id)), None).await?;
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
    cfg.gate(&table, Action::Create, identity.as_ref())?;
    let pool = umbra::db::pool();
    let new_id = insert_row(&pool, &model, &body).await?;
    let pk = pk_column(&model)?;
    let mut rows = fetch_rows(&pool, &model, Some((&pk.name, &new_id)), None).await?;
    let Some(mut row) = rows.pop() else {
        return Err(ApiError::BadInput(
            "row inserted but disappeared on read-back".into(),
        ));
    };
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
    cfg.gate(&table, Action::Update, identity.as_ref())?;
    let pk = pk_column(&model)?;
    let pool = umbra::db::pool();

    // 404 if the target row doesn't exist before we attempt the UPDATE.
    let existing = fetch_rows(&pool, &model, Some((&pk.name, &id)), None).await?;
    if existing.is_empty() {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    }

    update_row(&pool, &model, pk, &id, &body).await?;
    let mut rows = fetch_rows(&pool, &model, Some((&pk.name, &id)), None).await?;
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
    cfg.gate(&table, Action::Delete, identity.as_ref())?;
    let pk = pk_column(&model)?;
    let pool = umbra::db::pool();
    let result = sqlx::query(&format!(
        "DELETE FROM \"{}\" WHERE \"{}\" = ?",
        q(&model.table),
        q(&pk.name)
    ))
    .bind(&id)
    .execute(&pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!(
            "no row with {} = {} in {}",
            pk.name, id, table
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Double-quote-escape a SQL identifier (table or column name). All
/// `"` chars inside the name become `""`. Apply this AT the
/// interpolation site (`"{}", q(&model.table)`), never after — the
/// returned string is meant to land inside the quotes, not include
/// them. Latent safety today (table names are compile-time constants
/// from the derive macro) but future-proofs against dynamic
/// registrations.
fn q(name: &str) -> String {
    name.replace('"', "\"\"")
}

// =========================================================================
// Row marshalling. Per-SqlType dispatch on both directions; same pattern
// the backup and admin modules use.
// =========================================================================

async fn fetch_rows(
    pool: &SqlitePool,
    model: &ModelMeta,
    where_clause: Option<(&str, &str)>,
    page: Option<PageRequest>,
) -> Result<Vec<Map<String, Value>>, ApiError> {
    let columns = model
        .fields
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = match where_clause {
        Some((col, _)) => format!(
            "SELECT {columns} FROM \"{}\" WHERE \"{}\" = ? LIMIT 1",
            q(&model.table),
            q(col)
        ),
        None => {
            // Pagination applies only to the no-WHERE-clause list
            // path. Single-row lookups (retrieve/update/delete) keep
            // their existing `LIMIT 1` and bypass paging.
            let (limit_clause, offset_clause) = match page {
                Some(req) if req.limit != u64::MAX => (
                    format!(" LIMIT {}", req.limit),
                    format!(" OFFSET {}", req.offset),
                ),
                _ => (String::new(), String::new()),
            };
            format!(
                "SELECT {columns} FROM \"{}\" ORDER BY 1{limit_clause}{offset_clause}",
                q(&model.table)
            )
        }
    };
    let mut q = sqlx::query(&sql);
    if let Some((_, val)) = where_clause {
        q = q.bind(val.to_string());
    }
    let rows = q.fetch_all(pool).await?;
    let mut out: Vec<Map<String, Value>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut obj = Map::new();
        for col in &model.fields {
            obj.insert(col.name.clone(), column_to_json(&row, col)?);
        }
        out.push(obj);
    }
    Ok(out)
}

/// `SELECT COUNT(*)` for the given model. Used by the pagination
/// envelope to render `count` / `total_pages` / `next` links.
/// Quote-doubles the table name for the same future-proofing
/// reason `fetch_rows` does.
async fn count_rows(pool: &SqlitePool, model: &ModelMeta) -> Result<i64, ApiError> {
    let sql = format!("SELECT COUNT(*) FROM \"{}\"", q(&model.table));
    let (n,): (i64,) = sqlx::query_as(&sql).fetch_one(pool).await?;
    Ok(n)
}

fn column_to_json(row: &sqlx::sqlite::SqliteRow, col: &Column) -> Result<Value, ApiError> {
    let name = col.name.as_str();
    if col.nullable {
        return Ok(match col.ty {
            SqlType::SmallInt | SqlType::Integer => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::BigInt => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Real => row
                .try_get::<Option<f32>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v as f64)),
            SqlType::Double => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Boolean => row
                .try_get::<Option<bool>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Text => row
                .try_get::<Option<String>, _>(name)?
                .map_or(Value::Null, Value::from),
            SqlType::Date => row
                .try_get::<Option<NaiveDate>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Time => row
                .try_get::<Option<NaiveTime>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            SqlType::Timestamptz => row
                .try_get::<Option<DateTime<Utc>>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_rfc3339())),
            SqlType::Uuid => row
                .try_get::<Option<Uuid>, _>(name)?
                .map_or(Value::Null, |v| Value::from(v.to_string())),
            // Json columns serialize as themselves — the on-the-wire
            // JSON the REST endpoint emits already nests the document
            // structure verbatim. No string-wrapping.
            SqlType::Json => row
                .try_get::<Option<Value>, _>(name)?
                .unwrap_or(Value::Null),
            // Array fields are Postgres-only and the REST plugin reads
            // through a SqlitePool today. The field.backend system
            // check fires at boot when an Array field is registered
            // against SQLite, so the column-to-JSON path never reaches
            // this arm in practice.
            SqlType::Array(_) => panic_array_unsupported(&col.name),
            SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
                panic_pg_only_unsupported(&col.name)
            }
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => Value::from(row.try_get::<i32, _>(name)?),
        SqlType::BigInt => Value::from(row.try_get::<i64, _>(name)?),
        SqlType::Real => Value::from(row.try_get::<f32, _>(name)? as f64),
        SqlType::Double => Value::from(row.try_get::<f64, _>(name)?),
        SqlType::Boolean => Value::from(row.try_get::<bool, _>(name)?),
        SqlType::Text => Value::from(row.try_get::<String, _>(name)?),
        SqlType::Date => Value::from(row.try_get::<NaiveDate, _>(name)?.to_string()),
        SqlType::Time => Value::from(row.try_get::<NaiveTime, _>(name)?.to_string()),
        SqlType::Timestamptz => Value::from(row.try_get::<DateTime<Utc>, _>(name)?.to_rfc3339()),
        SqlType::Uuid => Value::from(row.try_get::<Uuid, _>(name)?.to_string()),
        SqlType::Json => row.try_get::<Value, _>(name)?,
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
    })
}

/// Boot-path-bypassed sentinel for Array fields. The REST plugin's
/// SqlitePool-based code path can't bind or decode Postgres arrays;
/// the field.backend system check should have failed boot when an
/// Array field was registered against SQLite. A future Postgres-aware
/// REST upgrade lifts this.
fn panic_array_unsupported(column: &str) -> ! {
    panic!(
        "umbra-rest: column `{column}` is a Postgres-only Array; the \
         field.backend system check should have failed boot. The REST \
         plugin's auto-CRUD path runs against SqlitePool today; a \
         Postgres-aware upgrade is a Phase 4 follow-on."
    )
}

/// Phase 4.4 sentinel for Inet/Cidr/MacAddr — same gating story as
/// arrays.
fn panic_pg_only_unsupported(column: &str) -> ! {
    panic!(
        "umbra-rest: column `{column}` is a Postgres-only network type \
         (Inet/Cidr/MacAddr); the field.backend system check should \
         have failed boot."
    )
}

async fn insert_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    body: &Map<String, Value>,
) -> Result<String, ApiError> {
    // PK with an integer SqlType is auto-generated by SQLite, so it
    // skips the writable set unless the client supplied it. Other
    // PK shapes (uuid::Uuid, String) the client must supply.
    let pk = pk_column(model)?;
    let pk_is_autoincrement = pk.primary_key
        && matches!(
            pk.ty,
            SqlType::Integer | SqlType::BigInt | SqlType::SmallInt
        );
    let writable: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| {
            !(c.primary_key
                && matches!(c.ty, SqlType::Integer | SqlType::BigInt | SqlType::SmallInt)
                && !body.contains_key(&c.name))
        })
        .collect();
    let names = writable
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = writable.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "INSERT INTO \"{}\" ({names}) VALUES ({placeholders})",
        q(&model.table)
    );
    let mut q = sqlx::query(&sql);
    for col in &writable {
        q = bind_json_value(q, col, body)?;
    }
    let result = q.execute(pool).await?;

    if pk_is_autoincrement {
        // SQLite hands out monotonic ids via ROWID; read back via
        // last_insert_rowid().
        Ok(result.last_insert_rowid().to_string())
    } else {
        // String / uuid PK: the client supplied it; echo it back.
        let id = body
            .get(&pk.name)
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                ApiError::BadInput(format!(
                    "non-integer primary key `{}` must be supplied in the request body",
                    pk.name
                ))
            })?;
        Ok(id)
    }
}

async fn update_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    pk: &Column,
    pk_value: &str,
    body: &Map<String, Value>,
) -> Result<(), ApiError> {
    // For PATCH semantics: update only the columns the body provided.
    // For PUT semantics: same, since missing columns we treat as
    // "leave alone" rather than clobbering with NULL/default. The
    // difference between PUT and PATCH at v1 is purely method
    // routing; both call this.
    let updates: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| !c.primary_key && body.contains_key(&c.name))
        .collect();
    if updates.is_empty() {
        return Ok(());
    }
    let setters = updates
        .iter()
        .map(|c| format!("\"{}\" = ?", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE \"{}\" SET {setters} WHERE \"{}\" = ?",
        q(&model.table),
        q(&pk.name)
    );
    let mut q = sqlx::query(&sql);
    for col in &updates {
        q = bind_json_value(q, col, body)?;
    }
    q = q.bind(pk_value.to_string());
    q.execute(pool).await?;
    Ok(())
}

type SqlxQuery<'q> = sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>;

/// Bind one column's value to a `sqlx::query::Query`. The JSON value
/// is parsed against the column's `SqlType` and coerced where the
/// HTML / JSON shapes differ from sqlx's native types (RFC-3339
/// strings for timestamps, "true"/"1" for booleans coming through a
/// stringly-typed body).
fn bind_json_value<'q>(
    q: SqlxQuery<'q>,
    col: &Column,
    body: &Map<String, Value>,
) -> Result<SqlxQuery<'q>, ApiError> {
    let raw = body.get(&col.name).cloned().unwrap_or(Value::Null);
    Ok(match raw {
        Value::Null if col.nullable => bind_null(q, col),
        Value::Null => {
            return Err(ApiError::BadInput(format!(
                "field `{}` is required and was null",
                col.name
            )));
        }
        Value::Bool(b) if matches!(col.ty, SqlType::Boolean) => q.bind(b),
        Value::Number(n) if matches!(col.ty, SqlType::SmallInt | SqlType::Integer) => {
            q.bind(n.as_i64().ok_or_else(|| {
                ApiError::BadInput(format!("field `{}` must be an integer", col.name))
            })? as i32)
        }
        Value::Number(n) if matches!(col.ty, SqlType::BigInt) => {
            q.bind(n.as_i64().ok_or_else(|| {
                ApiError::BadInput(format!("field `{}` must be an integer", col.name))
            })?)
        }
        Value::Number(n) if matches!(col.ty, SqlType::Real | SqlType::Double) => {
            q.bind(n.as_f64().ok_or_else(|| {
                ApiError::BadInput(format!("field `{}` must be a number", col.name))
            })?)
        }
        Value::String(s) => bind_string(q, col, &s)?,
        other => {
            return Err(ApiError::BadInput(format!(
                "field `{}`: unsupported JSON value `{:?}` for {:?}",
                col.name, other, col.ty
            )));
        }
    })
}

fn bind_string<'q>(q: SqlxQuery<'q>, col: &Column, s: &str) -> Result<SqlxQuery<'q>, ApiError> {
    Ok(match col.ty {
        SqlType::Text => q.bind(s.to_string()),
        SqlType::SmallInt | SqlType::Integer => q.bind(
            s.parse::<i32>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::BigInt => q.bind(
            s.parse::<i64>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Real => q.bind(
            s.parse::<f32>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Double => q.bind(
            s.parse::<f64>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Boolean => q.bind(matches!(s, "true" | "1")),
        SqlType::Date => q.bind(
            s.parse::<NaiveDate>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Time => q.bind(
            s.parse::<NaiveTime>()
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Timestamptz => {
            let parsed = DateTime::parse_from_rfc3339(s)
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?;
            q.bind(parsed.with_timezone(&Utc))
        }
        SqlType::Uuid => q.bind(
            Uuid::parse_str(s).map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        // The string came from a JSON request body's text-typed value
        // (admin form or query string). Parse it back to a structured
        // serde_json::Value so the binder stores it as JSONB / JSON
        // rather than as an opaque string. A client that wants to send
        // a JSON object directly should put it in the body as JSON, not
        // wrapped in a string — the JSON-body path (column_to_json's
        // inverse) handles that case.
        SqlType::Json => q.bind(
            serde_json::from_str::<Value>(s)
                .map_err(|e| ApiError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
    })
}

fn bind_null<'q>(q: SqlxQuery<'q>, col: &Column) -> SqlxQuery<'q> {
    match col.ty {
        SqlType::SmallInt | SqlType::Integer => q.bind(None::<i32>),
        SqlType::BigInt => q.bind(None::<i64>),
        SqlType::Real => q.bind(None::<f32>),
        SqlType::Double => q.bind(None::<f64>),
        SqlType::Boolean => q.bind(None::<bool>),
        SqlType::Text => q.bind(None::<String>),
        SqlType::Date => q.bind(None::<NaiveDate>),
        SqlType::Time => q.bind(None::<NaiveTime>),
        SqlType::Timestamptz => q.bind(None::<DateTime<Utc>>),
        SqlType::Uuid => q.bind(None::<Uuid>),
        SqlType::Json => q.bind(None::<Value>),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
    }
}
